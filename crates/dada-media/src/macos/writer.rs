//! AVAssetWriter-alapú videó-felvétel: natív, hardveres H.264 encode.
//!
//! A CVPixelBuffereket az adaptor pixel buffer pool-ja adja (IOSurface-hátú,
//! Metal-kompatibilis), így a kompozit közvetlenül a felvevő pufferébe
//! renderelhető, CPU-másolás nélkül.

use anyhow::{anyhow, bail, Result};
use std::ffi::c_void;
use std::path::Path;
use std::ptr::NonNull;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVFileTypeMPEG4,
    AVMediaTypeAudio, AVMediaTypeVideo,
};
use objc2_core_audio_types::{
    kAudioFormatFlagIsFloat, kAudioFormatFlagIsPacked, kAudioFormatFlagsNativeEndian,
    kAudioFormatLinearPCM, AudioStreamBasicDescription,
};
use objc2_core_foundation::{kCFAllocatorDefault, CFRetained};
use objc2_core_media::{
    CMAudioFormatDescriptionCreate, CMBlockBuffer, CMFormatDescription, CMSampleBuffer,
    CMSampleTimingInfo, CMTime, CMTimeFlags,
};
use objc2_core_video::{
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferPixelFormatTypeKey, CVPixelBuffer, CVPixelBufferGetBaseAddress,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetHeight, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferPoolCreatePixelBuffer, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString, NSURL};

/// BGRA pixel-formátum (OSType / FourCC).
pub const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241;

/// Érvényes CMTime adott ezredmásodperchez.
fn cm_time_ms(ms: i64) -> CMTime {
    CMTime {
        value: ms,
        timescale: 1000,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

fn as_any<T: objc2::Message>(value: Retained<T>) -> Retained<AnyObject> {
    // Objektum-erased gyökérre kasztolunk; a futásidejű típus ugyanaz.
    unsafe { Retained::cast_unchecked(value) }
}

/// Retained objektumokból álló szótár.
fn dict(
    keys: &[&NSString],
    values: Vec<Retained<AnyObject>>,
) -> Retained<NSDictionary<NSString, AnyObject>> {
    NSDictionary::from_retained_objects(keys, &values)
}

/// Videó-felvevő (MP4, H.264).
pub struct VideoWriter {
    writer: Retained<AVAssetWriter>,
    input: Retained<AVAssetWriterInput>,
    adaptor: Retained<AVAssetWriterInputPixelBufferAdaptor>,
    audio_input: Option<Retained<AVAssetWriterInput>>,
    audio_format: Option<CFRetained<CMFormatDescription>>,
    audio_sample_rate: f64,
    audio_channels: u32,
    audio_frames: u64,
    pub size: (u32, u32),
    pub fps: f64,
    pub frames: u64,
    finished: bool,
}

impl VideoWriter {
    pub fn new(path: &Path, width: u32, height: u32, fps: f64, bitrate: u32) -> Result<Self> {
        Self::new_impl(path, width, height, fps, bitrate, None)
    }

    /// Felvétel AAC audióval. A PCM mintákat a hívó adja (append_audio),
    /// az AVAssetWriter kódolja és muxolja a videó mellé.
    pub fn new_with_audio(
        path: &Path,
        width: u32,
        height: u32,
        fps: f64,
        bitrate: u32,
        sample_rate: f64,
        channels: u32,
        audio_bitrate: u32,
    ) -> Result<Self> {
        Self::new_impl(
            path,
            width,
            height,
            fps,
            bitrate,
            Some((sample_rate, channels.clamp(1, 2), audio_bitrate)),
        )
    }

    fn new_impl(
        path: &Path,
        width: u32,
        height: u32,
        fps: f64,
        bitrate: u32,
        audio: Option<(f64, u32, u32)>,
    ) -> Result<Self> {
        // A felvevő mindig felülír: a meglévő fájl blokkolhatja az AVAssetWritert.
        if path.exists() {
            let _ = std::fs::remove_file(path);
        }
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("nem UTF-8 útvonal: {}", path.display()))?;
        unsafe {
            let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
            let file_type = AVFileTypeMPEG4.ok_or_else(|| anyhow!("nincs AVFileTypeMPEG4"))?;
            let writer = AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type)
                .map_err(|e| anyhow!("AVAssetWriter: {}", e.localizedDescription()))?;

            let key_codec = NSString::from_str("AVVideoCodecKey");
            let key_width = NSString::from_str("AVVideoWidthKey");
            let key_height = NSString::from_str("AVVideoHeightKey");
            let key_comp = NSString::from_str("AVVideoCompressionPropertiesKey");
            let key_bitrate = NSString::from_str("AverageBitRate");
            let key_profile = NSString::from_str("ProfileLevel");
            let codec = NSString::from_str("avc1");
            // A ProfileLevel-hez névvel ellátott konstans kell (nem FourCC).
            let profile = NSString::from_str("H264_High_AutoLevel");
            let w = NSNumber::numberWithUnsignedInt(width);
            let h = NSNumber::numberWithUnsignedInt(height);
            let br = NSNumber::numberWithUnsignedInt(bitrate);
            let comp = dict(&[&key_bitrate, &key_profile], vec![as_any(br), as_any(profile)]);
            let settings = dict(
                &[&key_codec, &key_width, &key_height, &key_comp],
                vec![as_any(codec), as_any(w), as_any(h), as_any(comp)],
            );

            let media = AVMediaTypeVideo.ok_or_else(|| anyhow!("nincs AVMediaTypeVideo"))?;
            let input = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                media,
                Some(&settings),
            );
            input.setExpectsMediaDataInRealTime(true);
            if !writer.canAddInput(&input) {
                bail!("az AVAssetWriter nem fogadja el a videó-inputot");
            }
            writer.addInput(&input);

            // A pixel buffer pool: BGRA + IOSurface + Metal-kompatibilitás.
            let fmt = kCVPixelBufferPixelFormatTypeKey.to_string();
            let metal = kCVPixelBufferMetalCompatibilityKey.to_string();
            let ios = kCVPixelBufferIOSurfacePropertiesKey.to_string();
            let fmt_key = NSString::from_str(&fmt);
            let metal_key = NSString::from_str(&metal);
            let ios_key = NSString::from_str(&ios);
            let fmt_value = NSNumber::numberWithUnsignedInt(PIXEL_FORMAT_BGRA);
            let metal_value = NSNumber::numberWithBool(true);
            let empty = dict(&[], Vec::new());
            // Több puffert kérünk, hogy az encoder visszanyomása ne blokkoljon.
            let min_key = NSString::from_str("kCVPixelBufferPoolMinimumBufferCountKey");
            let min_value = NSNumber::numberWithUnsignedInt(10);
            let attrs = dict(
                &[&fmt_key, &metal_key, &ios_key, &min_key],
                vec![
                    as_any(fmt_value),
                    as_any(metal_value),
                    as_any(empty),
                    as_any(min_value),
                ],
            );

            let adaptor = AVAssetWriterInputPixelBufferAdaptor::
                assetWriterInputPixelBufferAdaptorWithAssetWriterInput_sourcePixelBufferAttributes(
                    &input,
                    Some(&attrs),
                );

            let mut audio_input: Option<Retained<AVAssetWriterInput>> = None;
            let mut audio_format: Option<CFRetained<CMFormatDescription>> = None;
            let mut audio_sample_rate = 0.0f64;
            let mut audio_channels = 0u32;
            if let Some((rate, ch, br)) = audio {
                let key_aid = NSString::from_str("AVFormatIDKey");
                let key_arate = NSString::from_str("AVSampleRateKey");
                let key_ach = NSString::from_str("AVNumberOfChannelsKey");
                let key_abr = NSString::from_str("AVEncoderBitRateKey");
                let aid = NSNumber::numberWithUnsignedInt(0x6161_6320); // 'aac '
                let arate = NSNumber::numberWithDouble(rate);
                let ach = NSNumber::numberWithUnsignedInt(ch);
                let abr = NSNumber::numberWithUnsignedInt(br);
                let asettings = dict(
                    &[&key_aid, &key_arate, &key_ach, &key_abr],
                    vec![as_any(aid), as_any(arate), as_any(ach), as_any(abr)],
                );
                let amedia = AVMediaTypeAudio.ok_or_else(|| anyhow!("nincs AVMediaTypeAudio"))?;
                let ainput = AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                    amedia,
                    Some(&asettings),
                );
                ainput.setExpectsMediaDataInRealTime(true);
                if !writer.canAddInput(&ainput) {
                    bail!("az AVAssetWriter nem fogadja el az audió-inputot");
                }
                writer.addInput(&ainput);

                let asbd = AudioStreamBasicDescription {
                    mSampleRate: rate,
                    mFormatID: kAudioFormatLinearPCM,
                    mFormatFlags: kAudioFormatFlagIsFloat
                        | kAudioFormatFlagsNativeEndian
                        | kAudioFormatFlagIsPacked,
                    mBytesPerPacket: ch * 4,
                    mFramesPerPacket: 1,
                    mBytesPerFrame: ch * 4,
                    mChannelsPerFrame: ch,
                    mBitsPerChannel: 32,
                    mReserved: 0,
                };
                let mut fmt_ptr: *const CMFormatDescription = std::ptr::null();
                let status = CMAudioFormatDescriptionCreate(
                    None,
                    NonNull::from(&asbd),
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null::<c_void>(),
                    None,
                    NonNull::new(&mut fmt_ptr).unwrap(),
                );
                if status != 0 {
                    bail!("CMAudioFormatDescriptionCreate: {status}");
                }
                let fmt_raw = NonNull::new(fmt_ptr as *mut CMFormatDescription)
                    .ok_or_else(|| anyhow!("nincs audio format description"))?;
                audio_format = Some(CFRetained::from_raw(fmt_raw));
                audio_input = Some(ainput);
                audio_sample_rate = rate;
                audio_channels = ch;
            }

            if !writer.startWriting() {
                let err = writer
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_else(|| "ismeretlen hiba".to_string());
                bail!("startWriting sikertelen: {err}");
            }
            writer.startSessionAtSourceTime(cm_time_ms(0));

            Ok(Self {
                writer,
                input,
                adaptor,
                audio_input,
                audio_format,
                audio_sample_rate,
                audio_channels,
                audio_frames: 0,
                size: (width, height),
                fps,
                frames: 0,
                finished: false,
            })
        }
    }

    /// Egy pixel buffer a felvevő pool-jából.
    pub fn pixel_buffer(&self) -> Result<CFRetained<CVPixelBuffer>> {
        let pool = unsafe { self.adaptor.pixelBufferPool() }
            .ok_or_else(|| anyhow!("nincs pixel buffer pool"))?;
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        let ptr = std::ptr::NonNull::new(&mut out as *mut *mut CVPixelBuffer)
            .ok_or_else(|| anyhow!("null pointer"))?;
        let status = unsafe { CVPixelBufferPoolCreatePixelBuffer(None, &pool, ptr) };
        if status != 0 {
            bail!("CVPixelBufferPoolCreatePixelBuffer: {status}");
        }
        let raw = std::ptr::NonNull::new(out).ok_or_else(|| anyhow!("null pixel buffer"))?;
        Ok(unsafe { CFRetained::from_raw(raw) })
    }

    /// A következő frame hozzáfűzése; a PTS a frame-számból számol (nincs drift).
    pub fn append(&mut self, buffer: &CVPixelBuffer) -> Result<()> {
        if self.finished {
            bail!("a felvevő már lezárult");
        }
        // Valós idejű módban az encoder időnként nem kész: meg kell várni.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !self.is_ready() {
            if std::time::Instant::now() > deadline {
                bail!("a felvevő nem kész fogadni (timeout)");
            }
            std::thread::sleep(std::time::Duration::from_micros(500));
        }
        let pts = (self.frames as f64 / self.fps * 1000.0).round() as i64;
        self.append_at_ms(buffer, pts as f64)
    }

    /// Hozzáfűzés megadott megjelenítési idővel (ms) — élő felvételnél a
    /// valós időt érdemes használni, hogy a videó valós idejű legyen.
    pub fn append_at_ms(&mut self, buffer: &CVPixelBuffer, pts_ms: f64) -> Result<()> {
        if self.finished {
            bail!("a felvevő már lezárult");
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !self.is_ready() {
            if std::time::Instant::now() > deadline {
                bail!("a felvevő nem kész fogadni (timeout)");
            }
            std::thread::sleep(std::time::Duration::from_micros(500));
        }
        let pts = pts_ms.round() as i64;
        let ok = unsafe {
            self.adaptor
                .appendPixelBuffer_withPresentationTime(buffer, cm_time_ms(pts))
        };
        if !ok {
            let err = unsafe {
                self.writer
                    .error()
                    .map(|e| e.localizedDescription().to_string())
            }
            .unwrap_or_else(|| "ismeretlen hiba".to_string());
            bail!("appendPixelBuffer sikertelen: {err}");
        }
        self.frames += 1;
        Ok(())
    }

    /// Kész-e fogadni a következő frame-et.
    pub fn is_ready(&self) -> bool {
        unsafe { self.input.isReadyForMoreMediaData() }
    }

    /// A pixel buffer feltöltése CPU-val (teszthez).
    pub fn fill(buffer: &CVPixelBuffer, mut fill: impl FnMut(&mut [u8], usize)) -> Result<()> {
        unsafe {
            CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::empty());
            let base = CVPixelBufferGetBaseAddress(buffer);
            let stride = CVPixelBufferGetBytesPerRow(buffer);
            let height = CVPixelBufferGetHeight(buffer);
            if base.is_null() {
                CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::empty());
                bail!("a pixel buffer nem képezhető le");
            }
            for y in 0..height {
                let row = (base as *mut u8).add(y * stride);
                let slice = std::slice::from_raw_parts_mut(row, stride);
                fill(slice, y);
            }
            CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::empty());
        }
        Ok(())
    }

    /// Egy pixel kiolvasása BGRA sorrendben (teszthez).
    pub fn read_pixel_bgra(buffer: &CVPixelBuffer, x: u32, y: u32) -> [u8; 4] {
        let mut out = [0u8; 4];
        unsafe {
            CVPixelBufferLockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
            let base = CVPixelBufferGetBaseAddress(buffer);
            let stride = CVPixelBufferGetBytesPerRow(buffer);
            let height = CVPixelBufferGetHeight(buffer);
            if !base.is_null()
                && (y as usize) < height
                && (x as usize + 1) * 4 <= stride
            {
                let p = (base as *const u8).add(y as usize * stride + x as usize * 4);
                out.copy_from_slice(std::slice::from_raw_parts(p, 4));
            }
            CVPixelBufferUnlockBaseAddress(buffer, CVPixelBufferLockFlags::ReadOnly);
        }
        out
    }

    /// Mono Float32 PCM minták hozzáfűzése a felvételhez. A hangot az
    /// AVAssetWriter AAC-be kódolja, és a videó mellé muxolja.
    pub fn append_audio(&mut self, mono: &[f32]) -> Result<()> {
        if self.finished || mono.is_empty() {
            return Ok(());
        }
        let (Some(audio_input), Some(format)) =
            (self.audio_input.as_ref(), self.audio_format.as_ref())
        else {
            return Ok(());
        };
        let channels = self.audio_channels.max(1) as usize;
        let frames = mono.len();
        let mut pcm = Vec::with_capacity(frames * channels * 4);
        for sample in mono {
            let bytes = sample.to_ne_bytes();
            for _ in 0..channels {
                pcm.extend_from_slice(&bytes);
            }
        }

        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while !unsafe { audio_input.isReadyForMoreMediaData() } {
            if std::time::Instant::now() > deadline {
                bail!("az audió-input nem kész fogadni (timeout)");
            }
            std::thread::sleep(std::time::Duration::from_micros(500));
        }

        let mut block_ptr: *mut CMBlockBuffer = std::ptr::null_mut();
        let allocator = unsafe { kCFAllocatorDefault };
        let status = unsafe {
            CMBlockBuffer::create_with_memory_block(
                None,
                std::ptr::null_mut(),
                pcm.len(),
                allocator,
                std::ptr::null(),
                0,
                pcm.len(),
                0,
                NonNull::new(&mut block_ptr).unwrap(),
            )
        };
        if status != 0 {
            bail!("CMBlockBufferCreateWithMemoryBlock: {status}");
        }
        let block = unsafe {
            CFRetained::from_raw(
                NonNull::new(block_ptr).ok_or_else(|| anyhow!("nincs audio block buffer"))?,
            )
        };
        let src = NonNull::new(pcm.as_ptr() as *mut c_void)
            .ok_or_else(|| anyhow!("null PCM pointer"))?;
        let status = unsafe { CMBlockBuffer::replace_data_bytes(src, &block, 0, pcm.len()) };
        if status != 0 {
            bail!("CMBlockBufferReplaceDataBytes: {status}");
        }

        let pts_ms = (self.audio_frames as f64 / self.audio_sample_rate * 1000.0).round() as i64;
        let timing = CMSampleTimingInfo {
            duration: CMTime {
                value: 1,
                timescale: self.audio_sample_rate.round() as i32,
                flags: CMTimeFlags::Valid,
                epoch: 0,
            },
            presentationTimeStamp: cm_time_ms(pts_ms),
            decodeTimeStamp: cm_time_ms(pts_ms),
        };
        let sample_size = (channels * 4) as usize;
        let mut sbuf_ptr: *mut CMSampleBuffer = std::ptr::null_mut();
        let status = unsafe {
            CMSampleBuffer::create_ready(
                None,
                Some(&block),
                Some(&**format),
                frames as isize,
                1,
                &timing,
                1,
                &sample_size,
                NonNull::new(&mut sbuf_ptr).unwrap(),
            )
        };
        if status != 0 {
            bail!("CMSampleBufferCreateReady: {status}");
        }
        let sample_buffer = unsafe {
            CFRetained::from_raw(
                NonNull::new(sbuf_ptr).ok_or_else(|| anyhow!("nincs audio sample buffer"))?,
            )
        };
        let ok = unsafe { audio_input.appendSampleBuffer(&sample_buffer) };
        if !ok {
            bail!("appendSampleBuffer (audio) sikertelen");
        }
        self.audio_frames += frames as u64;
        Ok(())
    }

    /// Befejezés: a fájl lezárása.
    pub fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        unsafe {
            self.input.markAsFinished();
            if let Some(audio_input) = self.audio_input.as_ref() {
                audio_input.markAsFinished();
            }
            if !self.writer.finishWriting() {
                let err = self
                    .writer
                    .error()
                    .map(|e| e.localizedDescription().to_string())
                    .unwrap_or_else(|| "ismeretlen hiba".to_string());
                bail!("finishWriting sikertelen: {err}");
            }
        }
        Ok(())
    }
}

impl Drop for VideoWriter {
    fn drop(&mut self) {
        if !self.finished {
            unsafe {
                self.input.markAsFinished();
                if let Some(audio_input) = self.audio_input.as_ref() {
                    audio_input.markAsFinished();
                }
                let _ = self.writer.finishWriting();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_av_foundation::{AVMediaTypeAudio, AVURLAsset};
    use objc2_foundation::{NSString, NSURL};

    #[test]
    fn writes_mp4_with_audio_track() {
        let path = std::env::temp_dir().join("dada-vj-audio-writer-test.mp4");
        let mut writer =
            VideoWriter::new_with_audio(&path, 320, 180, 30.0, 1_000_000, 48000.0, 2, 128_000)
                .expect("audio writer");
        let mut phase = 0.0f32;
        for _ in 0..30 {
            let pb = writer.pixel_buffer().expect("pixel buffer");
            VideoWriter::fill(&pb, |row, _| {
                for px in row.chunks_mut(4) {
                    px.copy_from_slice(&[10, 20, 30, 255]);
                }
            })
            .expect("fill");
            writer.append(&pb).expect("video append");
            let mut audio = vec![0f32; 1600];
            for sample in audio.iter_mut() {
                *sample = phase.sin() * 0.2;
                phase += 440.0 * std::f32::consts::TAU / 48000.0;
            }
            writer.append_audio(&audio).expect("audio append");
        }
        writer.finish().expect("finish");
        assert!(std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 1000);

        unsafe {
            let ns_path = NSString::from_str(path.to_str().unwrap());
            let url = NSURL::fileURLWithPath(&ns_path);
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            let media = AVMediaTypeAudio.expect("audio media type");
            let tracks = asset.tracksWithMediaType(media);
            assert!(tracks.count() > 0, "a felvételben nincs audió track");
        }
        let _ = std::fs::remove_file(path);
    }
}

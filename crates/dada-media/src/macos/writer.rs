//! AVAssetWriter-alapú videó-felvétel: natív, hardveres H.264 encode.
//!
//! A CVPixelBuffereket az adaptor pixel buffer pool-ja adja (IOSurface-hátú,
//! Metal-kompatibilis), így a kompozit közvetlenül a felvevő pufferébe
//! renderelhető, CPU-másolás nélkül.

use anyhow::{anyhow, bail, Result};
use std::path::Path;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_av_foundation::{
    AVAssetWriter, AVAssetWriterInput, AVAssetWriterInputPixelBufferAdaptor, AVFileTypeMPEG4,
    AVMediaTypeVideo,
};
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMTime, CMTimeFlags};
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
    pub size: (u32, u32),
    pub fps: f64,
    pub frames: u64,
    finished: bool,
}

impl VideoWriter {
    pub fn new(path: &Path, width: u32, height: u32, fps: f64, bitrate: u32) -> Result<Self> {
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

    /// Befejezés: a fájl lezárása.
    pub fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        unsafe {
            self.input.markAsFinished();
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
                let _ = self.writer.finishWriting();
            }
        }
    }
}

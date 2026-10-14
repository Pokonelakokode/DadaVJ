//! AAC encoder az ObjC AVAudioConverterrel — lépésenként tesztelve.
//!
//! A C AudioConverter nem alkalmas élő streamelésre (lásd a 20. dokumentumot),
//! ezért az ObjC utat használjuk. Minden ObjC híváshoz külön teszt tartozik,
//! hogy egy kivétel esetén pontosan látszódjon, melyik lépés a hibás.

use anyhow::{anyhow, Result};
use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AnyThread;
use objc2_avf_audio::{
    AVAudioBuffer, AVAudioCommonFormat, AVAudioCompressedBuffer, AVAudioConverter,
    AVAudioConverterInputStatus, AVAudioConverterOutputStatus, AVAudioFormat, AVAudioPCMBuffer,
};
use objc2_foundation::{NSDictionary, NSNumber, NSString};

/// kAudioFormatMPEG4AAC
pub const FORMAT_AAC: u32 = 0x6161_6320; // 'aac '

fn as_any<T: objc2::Message>(value: Retained<T>) -> Retained<AnyObject> {
    unsafe { Retained::cast_unchecked(value) }
}

/// 1. lépés: a bemeneti PCM formátum.
pub fn make_in_format(in_rate: f64, channels: u32) -> Result<Retained<AVAudioFormat>> {
    unsafe {
        AVAudioFormat::initWithCommonFormat_sampleRate_channels_interleaved(
            AVAudioFormat::alloc(),
            AVAudioCommonFormat::PCMFormatFloat32,
            in_rate,
            channels,
            false,
        )
        .ok_or_else(|| anyhow!("a bemeneti PCM formátum nem hozható létre"))
    }
}

/// 2. lépés: az AAC kimeneti formátum a beállítás-szótárból.
pub fn make_out_format(out_rate: f64, channels: u32, bitrate: u32) -> Result<Retained<AVAudioFormat>> {
    unsafe {
        let key_id = NSString::from_str("AVFormatIDKey");
        let key_rate = NSString::from_str("AVSampleRateKey");
        let key_ch = NSString::from_str("AVNumberOfChannelsKey");
        let key_br = NSString::from_str("AVEncoderBitRateKey");
        let id_v = NSNumber::numberWithUnsignedInt(FORMAT_AAC);
        let rate_v = NSNumber::numberWithDouble(out_rate);
        let ch_v = NSNumber::numberWithUnsignedInt(channels);
        let br_v = NSNumber::numberWithUnsignedInt(bitrate);
        let settings: Retained<NSDictionary<NSString, AnyObject>> =
            NSDictionary::from_retained_objects(
                &[&*key_id, &*key_rate, &*key_ch, &*key_br],
                &[as_any(id_v), as_any(rate_v), as_any(ch_v), as_any(br_v)],
            );
        AVAudioFormat::initWithSettings(AVAudioFormat::alloc(), &settings)
            .ok_or_else(|| anyhow!("az AAC formátum nem hozható létre"))
    }
}

/// 3. lépés: az átalakító.
pub fn make_converter(
    in_format: &AVAudioFormat,
    out_format: &AVAudioFormat,
) -> Result<Retained<AVAudioConverter>> {
    unsafe {
        AVAudioConverter::initFromFormat_toFormat(
            AVAudioConverter::alloc(),
            in_format,
            out_format,
        )
        .ok_or_else(|| anyhow!("az átalakító nem hozható létre"))
    }
}

/// 4. lépés: a bemeneti PCM puffer.
pub fn make_pcm_buffer(
    format: &AVAudioFormat,
    capacity: u32,
) -> Result<Retained<AVAudioPCMBuffer>> {
    unsafe {
        AVAudioPCMBuffer::initWithPCMFormat_frameCapacity(
            AVAudioPCMBuffer::alloc(),
            format,
            capacity,
        )
        .ok_or_else(|| anyhow!("a PCM puffer nem hozható létre"))
    }
}

/// 5. lépés: a tömörített kimeneti puffer.
///
/// FONTOS: az AAC **változó bájt/packet**, és a 2 argumentumos init pont ezt
/// tiltja ("fails if the format has variable bytes per packet"). Ezért a
/// 3 argumentumos változat kell, a converter maximumOutputPacketSize értékével.
pub fn make_compressed_buffer(
    format: &AVAudioFormat,
    packet_capacity: u32,
    maximum_packet_size: isize,
) -> Retained<AVAudioCompressedBuffer> {
    unsafe {
        AVAudioCompressedBuffer::initWithFormat_packetCapacity_maximumPacketSize(
            AVAudioCompressedBuffer::alloc(),
            format,
            packet_capacity,
            maximum_packet_size,
        )
    }
}

/// Az élő streamelésre kész AAC encoder.
///
/// A bemenet mono Float32 (minden csatorna ugyanazt kapja), a kimenet
/// AAC-LC. A blokk NoDataNow-t ad, amikor elfogy a bemenet — ez NEM
/// end-of-stream, ezért a converter a következő hívásnál újra kérhet.
pub struct AacEncoder {
    converter: Retained<AVAudioConverter>,
    in_buffer: Retained<AVAudioPCMBuffer>,
    out_buffer: Retained<AVAudioCompressedBuffer>,
    channels: u32,
    out_rate: f64,
    asc: Vec<u8>,
    pub frames_encoded: u64,
    pub packets_encoded: u64,
}

unsafe impl Send for AacEncoder {}

/// A bemeneti puffer kapacitása (frame).
const IN_CAPACITY: u32 = 8192;

impl AacEncoder {
    pub fn new(in_rate: f64, out_rate: f64, channels: u32, bitrate: u32) -> Result<Self> {
        let in_format = make_in_format(in_rate, channels)?;
        let out_format = make_out_format(out_rate, channels, bitrate)?;
        let converter = make_converter(&in_format, &out_format)?;
        let in_buffer = make_pcm_buffer(&in_format, IN_CAPACITY)?;
        let max_packet = unsafe { converter.maximumOutputPacketSize() };
        let out_buffer = make_compressed_buffer(&out_format, 1, max_packet);
        Ok(Self {
            converter,
            in_buffer,
            out_buffer,
            channels,
            out_rate,
            asc: super::aac::audio_specific_config(out_rate, channels),
            frames_encoded: 0,
            packets_encoded: 0,
        })
    }

    pub fn audio_specific_config(&self) -> &[u8] {
        &self.asc
    }

    pub fn sample_rate(&self) -> f64 {
        self.out_rate
    }

    /// Mono minták kódolása; a visszaadott vektor 0..n AAC packet.
    pub fn encode(&mut self, mono: &[f32]) -> Result<Vec<Vec<u8>>> {
        if mono.is_empty() {
            return Ok(Vec::new());
        }
        let frames = (mono.len() as u32).min(IN_CAPACITY);
        if frames == 0 {
            return Ok(Vec::new());
        }
        unsafe {
            self.in_buffer.setFrameLength(frames);
            let ch = self.in_buffer.floatChannelData();
            for c in 0..self.channels as usize {
                let ptr = (*ch.add(c)).as_ptr();
                std::ptr::copy_nonoverlapping(mono.as_ptr(), ptr, frames as usize);
            }
        }
        self.frames_encoded += frames as u64;

        let in_ptr = Retained::as_ptr(&self.in_buffer) as *mut AVAudioBuffer;
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f2 = std::sync::Arc::clone(&flag);
        let block = RcBlock::new(
            move |_count: u32,
                  status: std::ptr::NonNull<AVAudioConverterInputStatus>|
                  -> *mut AVAudioBuffer {
                if f2.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    unsafe { status.as_ptr().write(AVAudioConverterInputStatus::NoDataNow) };
                    std::ptr::null_mut()
                } else {
                    unsafe { status.as_ptr().write(AVAudioConverterInputStatus::HaveData) };
                    in_ptr
                }
            },
        );

        let mut out: Vec<Vec<u8>> = Vec::new();
        for _ in 0..128 {
            let status = unsafe {
                self.converter.convertToBuffer_error_withInputFromBlock(
                    &self.out_buffer,
                    None,
                    &*block as *const _ as *mut _,
                )
            };
            let packets = unsafe { self.out_buffer.packetCount() } as usize;
            let len = unsafe { self.out_buffer.byteLength() } as usize;
            if packets > 0 && len > 0 {
                let data = unsafe { self.out_buffer.data() };
                let bytes =
                    unsafe { std::slice::from_raw_parts(data.as_ptr() as *const u8, len) };
                out.push(bytes.to_vec());
                self.packets_encoded += 1;
            }
            if status != AVAudioConverterOutputStatus::HaveData {
                break;
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f64 = 48000.0;
    const CH: u32 = 2;
    const BR: u32 = 128_000;

    #[test]
    fn step1_in_format() {
        let f = make_in_format(RATE, CH).expect("1. lépés");
        unsafe {
            assert!((f.sampleRate() - RATE).abs() < 1.0, "minta ráta");
            assert_eq!(f.channelCount(), CH, "csatornaszám");
        }
        println!("1. lépés OK: bemeneti formátum {RATE} Hz, {CH} csatorna");
    }

    #[test]
    fn step2_out_format() {
        let f = make_out_format(RATE, CH, BR).expect("2. lépés");
        unsafe {
            assert!((f.sampleRate() - RATE).abs() < 1.0);
            assert_eq!(f.channelCount(), CH);
        }
        println!("2. lépés OK: AAC formátum");
    }

    #[test]
    fn step3_converter() {
        let i = make_in_format(RATE, CH).expect("bemenet");
        let o = make_out_format(RATE, CH, BR).expect("kimenet");
        let _c = make_converter(&i, &o).expect("3. lépés");
        println!("3. lépés OK: átalakító");
    }

    #[test]
    fn step4_pcm_buffer() {
        let f = make_in_format(RATE, CH).expect("bemenet");
        let b = make_pcm_buffer(&f, 8192).expect("4. lépés");
        unsafe {
            assert_eq!(b.frameCapacity(), 8192);
            b.setFrameLength(1024);
            assert_eq!(b.frameLength(), 1024);
        }
        println!("4. lépés OK: PCM puffer");
    }

    #[test]
    fn step5_compressed_buffer() {
        let i = make_in_format(RATE, CH).expect("bemenet");
        let o = make_out_format(RATE, CH, BR).expect("kimenet");
        let c = make_converter(&i, &o).expect("átalakító");
        let max = unsafe { c.maximumOutputPacketSize() };
        println!("  maximumOutputPacketSize = {max}");
        assert!(max > 0, "a max packet méret nem pozitív");
        let b = make_compressed_buffer(&o, 1, max);
        unsafe {
            assert_eq!(b.packetCapacity(), 1);
        }
        println!("5. lépés OK: tömörített puffer");
    }

    #[test]
    fn step7_streaming_chunks() {
        let mut enc = AacEncoder::new(RATE, RATE, CH, BR).expect("encoder");
        let mut total = 0usize;
        let mut bytes = 0usize;
        // 1 másodperc, 100 ms-os blokkokban (mint az app).
        for i in 0..10 {
            let n = 4800usize;
            let mut mono = vec![0f32; n];
            for (k, s) in mono.iter_mut().enumerate() {
                let t = (i * n + k) as f32 / RATE as f32;
                *s = (t * 440.0 * std::f32::consts::TAU).sin() * 0.4;
            }
            let p = enc.encode(&mono).expect("kódolás");
            total += p.len();
            bytes += p.iter().map(|b| b.len()).sum::<usize>();
        }
        println!("7. lépés: {total} packet, {bytes} bájt, ASC {:02X?}", enc.audio_specific_config());
        assert!(
            (40..=60).contains(&total),
            "a packet-szám nem a várt ~47: {total}"
        );
        assert_eq!(enc.audio_specific_config(), &[0x11, 0x90]);
    }

    #[test]
    fn step6_convert_one_packet() {
        let i = make_in_format(RATE, CH).expect("bemenet");
        let o = make_out_format(RATE, CH, BR).expect("kimenet");
        let c = make_converter(&i, &o).expect("átalakító");
        let in_buf = make_pcm_buffer(&i, 4096).expect("puffer");
        let max = unsafe { c.maximumOutputPacketSize() };
        let out_buf = make_compressed_buffer(&o, 1, max);

        // 4096 minta minden csatornába (szinusz).
        let frames = 4096usize;
        unsafe {
            in_buf.setFrameLength(frames as u32);
            let ch = in_buf.floatChannelData();
            for c_idx in 0..CH as usize {
                let ptr = (*ch.add(c_idx)).as_ptr();
                for n in 0..frames {
                    *ptr.add(n) = (n as f32 * 0.05).sin() * 0.3;
                }
            }
        }

        let in_ptr = Retained::as_ptr(&in_buf) as *mut AVAudioBuffer;
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let f2 = std::sync::Arc::clone(&flag);
        let block = RcBlock::new(
            move |_count: u32,
                  status: std::ptr::NonNull<AVAudioConverterInputStatus>|
                  -> *mut AVAudioBuffer {
                if f2.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    unsafe { status.as_ptr().write(AVAudioConverterInputStatus::NoDataNow) };
                    std::ptr::null_mut()
                } else {
                    unsafe { status.as_ptr().write(AVAudioConverterInputStatus::HaveData) };
                    in_ptr
                }
            },
        );
        let status = unsafe {
            c.convertToBuffer_error_withInputFromBlock(
                &out_buf,
                None,
                &*block as *const _ as *mut _,
            )
        };
        let packets = unsafe { out_buf.packetCount() };
        let len = unsafe { out_buf.byteLength() };
        println!(
            "6. lépés: status {status:?}, {packets} packet, {len} bájt"
        );
        assert!(packets >= 1, "nem készült packet");
    }
}

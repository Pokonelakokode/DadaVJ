//! AAC encoder (AudioToolbox) — az RTMP stream hangjához.
//!
//! Az AudioConverter C API-t használjuk (raw FFI): a bemenet Float32
//! interleaved PCM, a kimenet AAC-LC. A kimeneti packetekből FLV audio tag
//! lesz (a dada-stream csomagolja).
//!
//! Az AudioSpecificConfig-ot (a sequence headerhez) előbb a converter
//! magic cookie-jából kérjük; ha az nem elérhető, szabvány szerint építjük.

use anyhow::{bail, Result};
use std::ffi::c_void;

type OSStatus = i32;
type AudioConverterRef = *mut c_void;

/// kAudioFormatLinearPCM
const FORMAT_LPCM: u32 = 0x6C70_636D; // 'lpcm'
/// kAudioFormatMPEG4AAC
const FORMAT_AAC: u32 = 0x6161_6320; // 'aac '
/// kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked
const FLAG_FLOAT_PACKED: u32 = 1 | 8;
/// kAudioConverterEncodeBitRate
const PROP_BITRATE: u32 = 0x6272_6174; // 'brat'
/// kAudioConverterCompressionMagicCookie
const PROP_MAGIC_COOKIE: u32 = 0x6163_6B6B; // 'ackk'

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
struct Asbd {
    sample_rate: f64,
    format_id: u32,
    format_flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels_per_frame: u32,
    bits_per_channel: u32,
    reserved: u32,
}

#[repr(C)]
struct AudioBuffer {
    number_channels: u32,
    data_byte_size: u32,
    data: *mut c_void,
}

#[repr(C)]
struct AudioBufferList {
    number_buffers: u32,
    buffers: [AudioBuffer; 1],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PacketDescription {
    start_offset: i64,
    variable_frames: u32,
    data_byte_size: u32,
}

type InputProc = extern "C" fn(
    conv: AudioConverterRef,
    io_number_data_packets: *mut u32,
    io_data: *mut AudioBufferList,
    out_packet_desc: *mut *mut PacketDescription,
    user_data: *mut c_void,
) -> OSStatus;

#[link(name = "AudioToolbox", kind = "framework")]
extern "C" {
    fn AudioConverterNew(
        in_desc: *const Asbd,
        out_desc: *const Asbd,
        out: *mut AudioConverterRef,
    ) -> OSStatus;
    fn AudioConverterFillComplexBuffer(
        conv: AudioConverterRef,
        proc_fn: InputProc,
        user_data: *mut c_void,
        io_packet_size: *mut u32,
        out_data: *mut AudioBufferList,
        out_packet_desc: *mut PacketDescription,
    ) -> OSStatus;
    fn AudioConverterSetProperty(
        conv: AudioConverterRef,
        prop_id: u32,
        size: u32,
        data: *const c_void,
    ) -> OSStatus;
    fn AudioConverterGetProperty(
        conv: AudioConverterRef,
        prop_id: u32,
        io_size: *mut u32,
        out_data: *mut c_void,
    ) -> OSStatus;
    fn AudioConverterDispose(conv: AudioConverterRef) -> OSStatus;
}

/// Diagnosztika: a callback hívásai és az utolsó kérés.
pub static CB_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static CB_WANT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static CB_GIVE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A callback bemenete.
struct Feed {
    samples: Vec<f32>,
    pos: usize,
    channels: usize,
}

extern "C" fn input_proc(
    _conv: AudioConverterRef,
    io_number_data_packets: *mut u32,
    io_data: *mut AudioBufferList,
    _out_packet_desc: *mut *mut PacketDescription,
    user_data: *mut c_void,
) -> OSStatus {
    if user_data.is_null() || io_data.is_null() || io_number_data_packets.is_null() {
        return -1;
    }
    let feed = unsafe { &mut *(user_data as *mut Feed) };
    let list = unsafe { &mut *io_data };
    let want = unsafe { *io_number_data_packets } as usize;
    let avail = (feed.samples.len().saturating_sub(feed.pos)) / feed.channels.max(1);
    let give = want.min(avail);
    CB_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    CB_WANT.store(want as u64, std::sync::atomic::Ordering::Relaxed);
    CB_GIVE.store(give as u64, std::sync::atomic::Ordering::Relaxed);
    if give == 0 {
        unsafe {
            *io_number_data_packets = 0;
        }
        return 0;
    }
    let byte_off = feed.pos * 4;
    let byte_len = give * feed.channels * 4;
    let base = feed.samples.as_ptr() as *const u8;
    unsafe {
        list.number_buffers = 1;
        list.buffers[0].number_channels = feed.channels as u32;
        list.buffers[0].data_byte_size = byte_len as u32;
        list.buffers[0].data = base.add(byte_off) as *mut c_void;
        *io_number_data_packets = give as u32;
    }
    feed.pos += give * feed.channels;
    0
}

/// AAC-LC encoder.
pub struct AacEncoder {
    conv: AudioConverterRef,
    channels: usize,
    sample_rate: f64,
    /// Újrahasznosított feed (a callback ezt olvassa).
    feed: Feed,
    asc: Vec<u8>,
    pub frames_encoded: u64,
    pub packets_encoded: u64,
}

unsafe impl Send for AacEncoder {}

fn pcm_asbd(sample_rate: f64, channels: u32) -> Asbd {
    Asbd {
        sample_rate,
        format_id: FORMAT_LPCM,
        format_flags: FLAG_FLOAT_PACKED,
        bytes_per_packet: 4 * channels,
        frames_per_packet: 1,
        bytes_per_frame: 4 * channels,
        channels_per_frame: channels,
        bits_per_channel: 32,
        reserved: 0,
    }
}

fn aac_asbd(sample_rate: f64, channels: u32) -> Asbd {
    Asbd {
        sample_rate,
        format_id: FORMAT_AAC,
        // Az AAC-nál a formatFlags a kodek objektumtípusa:
        // kMPEG4Object_AAC_LC = 2. (0-val a converter máshogy viselkedik.)
        format_flags: 2,
        bytes_per_packet: 0,
        frames_per_packet: 1024,
        bytes_per_frame: 0,
        channels_per_frame: channels,
        bits_per_channel: 0,
        reserved: 0,
    }
}

impl AacEncoder {
    /// in_rate: a bemeneti PCM rátája; out_rate: az AAC rátája (a converter
    /// átmintavételez, ha eltérnek).
    pub fn new(in_rate: f64, out_rate: f64, channels: u32, bitrate: u32) -> Result<Self> {
        let mut conv: AudioConverterRef = std::ptr::null_mut();
        let input = pcm_asbd(in_rate, channels);
        let output = aac_asbd(out_rate, channels);
        let status = unsafe { AudioConverterNew(&input, &output, &mut conv) };
        if status != 0 || conv.is_null() {
            bail!("AudioConverterNew hiba: {status} ({})", fourcc_str(status as u32));
        }
        // Bitráta.
        let br = bitrate;
        let st = unsafe {
            AudioConverterSetProperty(conv, PROP_BITRATE, 4, &br as *const u32 as *const c_void)
        };
        if st != 0 {
            log::warn!("az AAC bitráta nem állítható: {st}");
        }
        let mut enc = Self {
            conv,
            channels: channels as usize,
            sample_rate: out_rate,
            feed: Feed {
                samples: Vec::new(),
                pos: 0,
                channels: channels as usize,
            },
            asc: Vec::new(),
            frames_encoded: 0,
            packets_encoded: 0,
        };
        enc.asc = enc
            .read_magic_cookie()
            .unwrap_or_else(|| audio_specific_config(out_rate, channels));
        Ok(enc)
    }

    fn read_magic_cookie(&self) -> Option<Vec<u8>> {
        let mut size: u32 = 0;
        let st = unsafe {
            AudioConverterGetProperty(self.conv, PROP_MAGIC_COOKIE, &mut size, std::ptr::null_mut())
        };
        if st != 0 || size == 0 || size > 256 {
            return None;
        }
        let mut buf = vec![0u8; size as usize];
        let st = unsafe {
            AudioConverterGetProperty(
                self.conv,
                PROP_MAGIC_COOKIE,
                &mut size,
                buf.as_mut_ptr() as *mut c_void,
            )
        };
        if st != 0 {
            return None;
        }
        buf.truncate(size as usize);
        Some(buf)
    }

    /// Az AudioSpecificConfig (a FLV sequence headerhez).
    pub fn audio_specific_config(&self) -> &[u8] {
        &self.asc
    }

    /// Interleaved Float32 minták kódolása; a kimenet a nyers AAC packetek.
    pub fn encode(&mut self, interleaved: &[f32]) -> Result<Vec<Vec<u8>>> {
        if interleaved.is_empty() || self.channels == 0 {
            return Ok(Vec::new());
        }
        // FONTOS: a C AudioConverter 0 packetet kapva LEFÁRJA a streamet, ezért
        // a callback soha nem fogyhat ki a híváson belül. Ezért perzisztens
        // puffert tartunk, és mindig egy teljes packet tartalékot hagyunk.
        if self.feed.pos > 0 {
            self.feed.samples.drain(..self.feed.pos);
            self.feed.pos = 0;
        }
        self.feed.samples.extend_from_slice(interleaved);
        let avail_frames = self.feed.samples.len() / self.channels;
        let frames = (interleaved.len() / self.channels) as u64;
        self.frames_encoded += frames;
        // Az AAC encoder a kért packeteken FELÜL is előre olvas (~2 packet),
        // ezért annyit hagyunk tartalékban, hogy a callback soha ne adjon 0-t.
        const LOOKAHEAD: u32 = 6;
        let max_packets = (avail_frames / 1024) as u32;
        if max_packets < LOOKAHEAD + 2 {
            return Ok(Vec::new());
        }
        let request = (max_packets - LOOKAHEAD).min(16);

        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut scratch = vec![0u8; 64 * 1024];
        let mut descs = vec![PacketDescription::default(); 64];
        for _ in 0..64 {
            let mut packets: u32 = request;
            let mut list = AudioBufferList {
                number_buffers: 1,
                buffers: [AudioBuffer {
                    number_channels: self.channels as u32,
                    data_byte_size: scratch.len() as u32,
                    data: scratch.as_mut_ptr() as *mut c_void,
                }],
            };
            let status = unsafe {
                AudioConverterFillComplexBuffer(
                    self.conv,
                    input_proc,
                    &mut self.feed as *mut Feed as *mut c_void,
                    &mut packets,
                    &mut list,
                    descs.as_mut_ptr(),
                )
            };
            if status != 0 {
                bail!("AudioConverterFillComplexBuffer hiba: {status}");
            }
            if packets == 0 {
                break;
            }
            for d in descs.iter().take(packets as usize) {
                let start = d.start_offset as usize;
                let len = d.data_byte_size as usize;
                if start + len <= scratch.len() {
                    out.push(scratch[start..start + len].to_vec());
                }
            }
            self.packets_encoded += packets as u64;
            if self.feed.pos >= self.feed.samples.len() && (packets as usize) < 64 {
                break;
            }
        }
        if out.is_empty() {
            // Nem hiba: az AAC encoder belső puffere még gyűjt (priming).
            // A minták nem vesznek el, a következő hívásnál kijönnek.
            log::debug!("AAC: {} minta be, packet még nincs (belső puffer)", frames);
        }
        Ok(out)
    }

    /// Az AAC minta-rátája.
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }
}

impl Drop for AacEncoder {
    fn drop(&mut self) {
        if !self.conv.is_null() {
            unsafe {
                AudioConverterDispose(self.conv);
            }
            self.conv = std::ptr::null_mut();
        }
    }
}

fn fourcc_str(code: u32) -> String {
    let b = code.to_be_bytes();
    b.iter()
        .map(|c| if c.is_ascii_graphic() { *c as char } else { '.' })
        .collect()
}

/// Az AudioSpecificConfig szabványos felépítése (AAC-LC).
pub fn audio_specific_config(sample_rate: f64, channels: u32) -> Vec<u8> {
    const RATES: [u32; 13] = [
        96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350,
    ];
    let idx = RATES
        .iter()
        .position(|r| (*r as f64 - sample_rate).abs() < 1.0)
        .unwrap_or(4) as u32; // 44100 az alapérték
    let object_type = 2u32; // AAC-LC
    let channel_config = channels.min(7);
    let bits = (object_type << 11) | (idx << 7) | (channel_config << 3);
    vec![(bits >> 8) as u8, (bits & 0xFF) as u8]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asc_for_48k_stereo() {
        let c = audio_specific_config(48000.0, 2);
        assert_eq!(c, vec![0x11, 0x90], "48k sztereó AAC-LC");
    }

    #[test]
    fn asc_for_44k_mono() {
        let c = audio_specific_config(44100.0, 1);
        assert_eq!(c, vec![0x12, 0x08], "44.1k mono AAC-LC");
    }

    #[test]
    fn asbd_layout() {
        // A C struct mérete 40 bájt (9 mező).
        assert_eq!(std::mem::size_of::<Asbd>(), 40);
        let p = pcm_asbd(48000.0, 2);
        assert_eq!(p.bytes_per_frame, 8);
        assert_eq!(p.frames_per_packet, 1);
        let a = aac_asbd(48000.0, 2);
        assert_eq!(a.frames_per_packet, 1024);
        assert_eq!(a.format_id, FORMAT_AAC);
    }
}

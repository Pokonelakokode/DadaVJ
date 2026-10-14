//! Hardveres H.264 encoder (VTCompressionSession) — raw FFI.
//!
//! Az objc2-nek nincs VideoToolbox bindingje, ezért a C API-t közvetlenül
//! deklaráljuk. A kimenet a callbacken érkezik (CMSampleBuffer), amiből a
//! length-prefixed NALU-kat olvassuk ki — pontosan azt, amit az FLV vár.

use anyhow::{anyhow, bail, Result};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMTime, CMTimeFlags};
use objc2_core_video::CVPixelBuffer;
use objc2_foundation::{NSDictionary, NSNumber, NSString};

const H264_CODEC_TYPE: u32 = 0x6176_6331; // 'avc1'
const NO_ERR: i32 = 0;

type OSStatus = i32;
type VTSession = *mut c_void;
type SampleBuffer = *mut c_void;

type OutputCallback = extern "C" fn(
    ref_con: *mut c_void,
    source_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample_buffer: SampleBuffer,
);

#[link(name = "VideoToolbox", kind = "framework")]
extern "C" {
    fn VTCompressionSessionCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: *const c_void,
        source_image_buffer_attributes: *const c_void,
        compressed_data_allocator: *const c_void,
        output_callback: OutputCallback,
        output_callback_ref_con: *mut c_void,
        compression_session_out: *mut VTSession,
    ) -> OSStatus;

    fn VTSessionSetProperty(session: VTSession, key: *const c_void, value: *const c_void) -> OSStatus;
    fn VTCompressionSessionPrepareToEncodeFrames(session: VTSession) -> OSStatus;
    fn VTCompressionSessionEncodeFrame(
        session: VTSession,
        image_buffer: *const c_void,
        pts: CMTime,
        duration: CMTime,
        frame_properties: *const c_void,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    fn VTCompressionSessionCompleteFrames(session: VTSession, complete_until: CMTime) -> OSStatus;
    fn VTCompressionSessionInvalidate(session: VTSession);
    fn VTCompressionSessionGetPixelBufferPool(session: VTSession) -> *mut c_void;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMSampleBufferGetDataBuffer(sbuf: SampleBuffer) -> *mut c_void;
    fn CMSampleBufferGetFormatDescription(sbuf: SampleBuffer) -> *mut c_void;
    fn CMSampleBufferGetPresentationTimeStamp(sbuf: SampleBuffer) -> CMTime;
    fn CMSampleBufferGetSampleAttachmentsArray(sbuf: SampleBuffer, create: bool) -> *const c_void;
    fn CMBlockBufferGetDataLength(block: *mut c_void) -> usize;
    fn CMBlockBufferCopyDataBytes(
        block: *mut c_void,
        offset: usize,
        length: usize,
        dest: *mut u8,
    ) -> OSStatus;
    fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc: *mut c_void,
        index: usize,
        out_ptr: *mut *const u8,
        out_size: *mut usize,
        out_count: *mut usize,
        out_header_size: *mut i32,
    ) -> OSStatus;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFArrayGetCount(array: *const c_void) -> isize;
    fn CFArrayGetValueAtIndex(array: *const c_void, index: isize) -> *const c_void;
    fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
}

/// Egy kimeneti frame (a NALU-k length-prefixed AVCC formában).
#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub pts_ms: f64,
    pub keyframe: bool,
}

#[derive(Default)]
struct EncoderOutput {
    frames: Mutex<VecDeque<EncodedFrame>>,
    /// AVCDecoderConfigurationRecord (SPS/PPS) — egyszer épül fel.
    avcc: Mutex<Option<Vec<u8>>>,
    errors: Mutex<Vec<String>>,
    /// Hányszor hívta a VideoToolbox a callbacket (diagnosztika).
    callbacks: std::sync::atomic::AtomicU64,
}

/// Hardveres H.264 encoder.
pub struct VideoEncoder {
    session: VTSession,
    output: Arc<EncoderOutput>,
    ref_con: *mut c_void,
    pub size: (u32, u32),
}

// A sessiont csak egy szálon használjuk (a render szálon), a callback viszont
// a VideoToolbox saját szálán fut — ezért a mutexek.
unsafe impl Send for VideoEncoder {}

fn cm_time(ms: f64) -> CMTime {
    CMTime {
        value: (ms * 1000.0).round() as i64,
        timescale: 1_000_000,
        flags: CMTimeFlags::Valid,
        epoch: 0,
    }
}

fn cfstr(s: &str) -> CFRetained<objc2_core_foundation::CFString> {
    objc2_core_foundation::CFString::from_str(s)
}

/// Egy CF-objektum pointere (a C API-hoz).
fn ptr_of<T: objc2_core_foundation::Type>(v: &CFRetained<T>) -> *const c_void {
    CFRetained::as_ptr(v).as_ptr() as *const c_void
}

fn set_prop(session: VTSession, key: &str, value: *const c_void) -> Result<()> {
    let k = cfstr(key);
    let kp = &*k as *const objc2_core_foundation::CFString as *const c_void;
    let status = unsafe { VTSessionSetProperty(session, kp, value) };
    if status != NO_ERR {
        bail!("VTSessionSetProperty({key}) hiba: {status}");
    }
    Ok(())
}

fn as_any<T: objc2::Message>(value: Retained<T>) -> Retained<AnyObject> {
    unsafe { Retained::cast_unchecked(value) }
}

impl VideoEncoder {
    pub fn new(width: u32, height: u32, bitrate: u32, fps: u32) -> Result<Self> {
        let output = Arc::new(EncoderOutput::default());
        let ref_con = Arc::into_raw(Arc::clone(&output)) as *mut c_void;

        // A forrás-pufferek: BGRA + Metal-kompatibilitás (a GPU renderel bele).
        let (fmt_key_s, metal_key_s) = unsafe {
            (
                objc2_core_video::kCVPixelBufferPixelFormatTypeKey.to_string(),
                objc2_core_video::kCVPixelBufferMetalCompatibilityKey.to_string(),
            )
        };
        let fmt_key = NSString::from_str(&fmt_key_s);
        let metal_key = NSString::from_str(&metal_key_s);
        let fmt_value = NSNumber::numberWithUnsignedInt(0x4247_5241); // BGRA
        let metal_value = NSNumber::numberWithBool(true);
        let attrs: Retained<NSDictionary<NSString, AnyObject>> =
            NSDictionary::from_retained_objects(
                &[&*fmt_key, &*metal_key],
                &[as_any(fmt_value), as_any(metal_value)],
            );
        let attrs_ptr = &*attrs as *const NSDictionary<NSString, AnyObject> as *const c_void;

        let mut session: VTSession = std::ptr::null_mut();
        let status = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null(),
                width as i32,
                height as i32,
                H264_CODEC_TYPE,
                std::ptr::null(),
                attrs_ptr,
                std::ptr::null(),
                output_callback,
                ref_con,
                &mut session,
            )
        };
        if status != NO_ERR {
            bail!("VTCompressionSessionCreate hiba: {status}");
        }
        if session.is_null() {
            bail!("üres VTCompressionSession");
        }

        // Valós idejű mód + bitráta + profil.
        let realtime = cfbool(true);
        set_prop(session, "RealTime", ptr_of(&realtime))?;
        let br = cfnum(bitrate);
        set_prop(session, "AverageBitRate", ptr_of(&br))?;
        let expected = cfnum(fps);
        let _ = set_prop(session, "ExpectedFrameRate", ptr_of(&expected));
        let profile = cfstr("H264_High_AutoLevel");
        set_prop(session, "ProfileLevel", ptr_of(&profile))?;

        let status = unsafe { VTCompressionSessionPrepareToEncodeFrames(session) };
        if status != NO_ERR {
            bail!("VTCompressionSessionPrepareToEncodeFrames hiba: {status}");
        }

        Ok(Self {
            session,
            output,
            ref_con,
            size: (width, height),
        })
    }

    /// Egy pixel buffer az encoder saját pool-jából (IOSurface-hátú, NEM másol).
    pub fn pixel_buffer(&self) -> Result<CFRetained<CVPixelBuffer>> {
        let pool = unsafe { VTCompressionSessionGetPixelBufferPool(self.session) };
        if pool.is_null() {
            bail!("az encodernek még nincs pixel buffer pool-ja");
        }
        let pool_ref: &objc2_core_video::CVPixelBufferPool =
            unsafe { &*(pool as *const objc2_core_video::CVPixelBufferPool) };
        let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
        let ptr = std::ptr::NonNull::new(&mut out as *mut *mut CVPixelBuffer)
            .ok_or_else(|| anyhow!("null pointer"))?;
        let status = unsafe {
            objc2_core_video::CVPixelBufferPoolCreatePixelBuffer(None, pool_ref, ptr)
        };
        if status != 0 {
            bail!("CVPixelBufferPoolCreatePixelBuffer: {status}");
        }
        let raw = std::ptr::NonNull::new(out).ok_or_else(|| anyhow!("null pixel buffer"))?;
        Ok(unsafe { CFRetained::from_raw(raw) })
    }

    /// Egy pixel buffer bekéldulázása; a kimenet a callbacken érkezik.
    pub fn encode(&self, buffer: &CVPixelBuffer, pts_ms: f64) -> Result<()> {
        let img = buffer as *const CVPixelBuffer as *const c_void;
        let mut flags: u32 = 0;
        let status = unsafe {
            VTCompressionSessionEncodeFrame(
                self.session,
                img,
                cm_time(pts_ms),
                CMTime {
                    value: 0,
                    timescale: 1,
                    flags: CMTimeFlags::Valid,
                    epoch: 0,
                },
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut flags,
            )
        };
        if status != NO_ERR {
            bail!("VTCompressionSessionEncodeFrame hiba: {status}");
        }
        Ok(())
    }

    /// A callbacken összegyűlt frame-ek kivétele.
    pub fn drain(&self) -> Vec<EncodedFrame> {
        self.output
            .frames
            .lock()
            .map(|mut f| f.drain(..).collect())
            .unwrap_or_default()
    }

    /// Az AVCDecoderConfigurationRecord (SPS/PPS), ha már megvan.
    pub fn avcc(&self) -> Option<Vec<u8>> {
        self.output.avcc.lock().ok().and_then(|a| a.clone())
    }

    /// A callback-hívások száma (diagnosztika).
    pub fn callbacks(&self) -> u64 {
        self.output
            .callbacks
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn errors(&self) -> Vec<String> {
        self.output
            .errors
            .lock()
            .map(|e| e.clone())
            .unwrap_or_default()
    }

    /// A hátralévő frame-ek kikényszerítése és a session lezárása.
    pub fn finish(&mut self) {
        if !self.session.is_null() {
            unsafe {
                VTCompressionSessionCompleteFrames(self.session, cm_time(f64::MAX / 2.0));
                VTCompressionSessionInvalidate(self.session);
            }
            self.session = std::ptr::null_mut();
        }
        if !self.ref_con.is_null() {
            unsafe {
                let _ = Arc::from_raw(self.ref_con as *const EncoderOutput);
            }
            self.ref_con = std::ptr::null_mut();
        }
    }
}

impl Drop for VideoEncoder {
    fn drop(&mut self) {
        self.finish();
    }
}

fn cfbool(v: bool) -> CFRetained<objc2_core_foundation::CFBoolean> {
    unsafe {
        let b = if v {
            objc2_core_foundation::kCFBooleanTrue
        } else {
            objc2_core_foundation::kCFBooleanFalse
        }
        .expect("CFBoolean konstans");
        CFRetained::retain(std::ptr::NonNull::new_unchecked(
            b as *const objc2_core_foundation::CFBoolean as *mut objc2_core_foundation::CFBoolean,
        ))
    }
}

fn cfnum(v: u32) -> CFRetained<objc2_core_foundation::CFNumber> {
    objc2_core_foundation::CFNumber::new_i32(v as i32)
}

extern "C" fn output_callback(
    ref_con: *mut c_void,
    _source_ref_con: *mut c_void,
    status: OSStatus,
    _info_flags: u32,
    sample_buffer: SampleBuffer,
) {
    if ref_con.is_null() {
        return;
    }
    let output: &EncoderOutput = unsafe { &*(ref_con as *const EncoderOutput) };
    output
        .callbacks
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if status != NO_ERR || sample_buffer.is_null() {
        if let Ok(mut e) = output.errors.lock() {
            e.push(format!("encoder status: {status}"));
        }
        return;
    }
    unsafe {
        let pts = CMSampleBufferGetPresentationTimeStamp(sample_buffer);
        let pts_ms = if pts.timescale > 0 {
            pts.value as f64 / pts.timescale as f64 * 1000.0
        } else {
            0.0
        };

        // SPS/PPS egyszer.
        let desc = CMSampleBufferGetFormatDescription(sample_buffer);
        if !desc.is_null() {
            let mut need = false;
            if let Ok(a) = output.avcc.lock() {
                need = a.is_none();
            }
            if need {
                if let Some(avcc) = build_avcc(desc) {
                    if let Ok(mut a) = output.avcc.lock() {
                        if a.is_none() {
                            *a = Some(avcc);
                        }
                    }
                }
            }
        }

        // A NALU-k.
        let block = CMSampleBufferGetDataBuffer(sample_buffer);
        if block.is_null() {
            return;
        }
        let len = CMBlockBufferGetDataLength(block);
        if len == 0 {
            return;
        }
        let mut data = vec![0u8; len];
        let st = CMBlockBufferCopyDataBytes(block, 0, len, data.as_mut_ptr());
        if st != NO_ERR {
            if let Ok(mut e) = output.errors.lock() {
                e.push(format!("CMBlockBufferCopyDataBytes: {st}"));
            }
            return;
        }

        let keyframe = is_keyframe(sample_buffer);
        if let Ok(mut frames) = output.frames.lock() {
            frames.push_back(EncodedFrame {
                data,
                pts_ms,
                keyframe,
            });
        }
    }
}

/// Szinkron (key) frame-e? A NotSync attachment hiánya jelenti a keyframe-et.
unsafe fn is_keyframe(sbuf: SampleBuffer) -> bool {
    let arr = CMSampleBufferGetSampleAttachmentsArray(sbuf, false);
    if arr.is_null() || CFArrayGetCount(arr) <= 0 {
        return true;
    }
    let dict = CFArrayGetValueAtIndex(arr, 0);
    if dict.is_null() {
        return true;
    }
    let key = cfstr("NotSync");
    let kp = &*key as *const objc2_core_foundation::CFString as *const c_void;
    let value = CFDictionaryGetValue(dict, kp);
    value.is_null()
}

/// AVCDecoderConfigurationRecord az SPS/PPS-ből.
unsafe fn build_avcc(desc: *mut c_void) -> Option<Vec<u8>> {
    let mut sps_ptr: *const u8 = std::ptr::null();
    let mut sps_len: usize = 0;
    let mut count: usize = 0;
    let mut header: i32 = 0;
    let st = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc,
        0,
        &mut sps_ptr,
        &mut sps_len,
        &mut count,
        &mut header,
    );
    if st != NO_ERR || sps_ptr.is_null() || sps_len == 0 {
        return None;
    }
    let mut pps_ptr: *const u8 = std::ptr::null();
    let mut pps_len: usize = 0;
    let st2 = CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc,
        1,
        &mut pps_ptr,
        &mut pps_len,
        &mut count,
        &mut header,
    );
    let sps = std::slice::from_raw_parts(sps_ptr, sps_len).to_vec();
    let pps = if st2 == NO_ERR && !pps_ptr.is_null() && pps_len > 0 {
        std::slice::from_raw_parts(pps_ptr, pps_len).to_vec()
    } else {
        Vec::new()
    };
    Some(dada_flv_avcc(&sps, &pps))
}

/// Az avcC felépítése (a dada-stream flv moduljával azonos formátum).
fn dada_flv_avcc(sps: &[u8], pps: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(11 + sps.len() + pps.len());
    out.push(1);
    out.push(sps.first().copied().unwrap_or(0x64));
    out.push(sps.get(1).copied().unwrap_or(0));
    out.push(sps.get(2).copied().unwrap_or(0x28));
    out.push(0xFF);
    out.push(0xE1);
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(pps.len().min(255) as u8);
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    out
}

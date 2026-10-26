//! AVFoundation-alapú videó-dekóder.
//!
//! Az AVAssetReader a VideoToolbox hardveres dekóderét használja, és
//! CVPixelBuffereket ad (alapból IOSurface-hátú, Metal-kompatibilis), így a
//! frame a CPU érintése nélkül vihető GPU-textúrába.

use anyhow::Result;
use std::path::Path;

use anyhow::anyhow;
use objc2::rc::Retained;
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderOutput, AVAssetReaderTrackOutput, AVAssetReaderStatus,
    AVAssetTrack, AVMediaTypeVideo, AVURLAsset,
};
use objc2::runtime::AnyObject;
use objc2_core_foundation::CFRetained;
use objc2_core_media::{CMSampleBufferGetPresentationTimeStamp, CMTime, CMTimeRange};
use objc2_core_video::{
    kCVPixelBufferMetalCompatibilityKey, kCVPixelBufferPixelFormatTypeKey, CVPixelBuffer,
    CVPixelBufferGetHeight, CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSURL};

/// A kért kimeneti pixel-formátumok (OSType / FourCC).
pub const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241; // 'BGRA'
pub const PIXEL_FORMAT_420V: u32 = 0x3432_3076; // '420v' — natív biplanáris YUV, Metal-kompatibilis

/// AVAssetReader output settings: pixel-formátum + Metal-kompatibilitás.
///
/// Enélkül (nil) az AVAssetReader a **tömörített** sample-eket adja vissza,
/// nem képet. A Metal-kompatibilitás kérése biztosítja az IOSurface-hátteret,
/// ami a zero-copy textúra-feltöltés feltétele.
fn output_settings(format: u32) -> Retained<NSDictionary<NSString, AnyObject>> {
    // Az extern static-ok olvasása unsafe (a CFString globálisokra mutatnak).
    let (format_key_str, metal_key_str) = unsafe {
        (
            kCVPixelBufferPixelFormatTypeKey.to_string(),
            kCVPixelBufferMetalCompatibilityKey.to_string(),
        )
    };
    let format_key: Retained<NSString> = NSString::from_str(&format_key_str);
    let metal_key: Retained<NSString> = NSString::from_str(&metal_key_str);
    let format_value = NSNumber::numberWithUnsignedInt(format);
    let metal_value = NSNumber::numberWithBool(true);
    let objects: Vec<Retained<AnyObject>> = vec![
        unsafe { Retained::cast_unchecked(format_value) },
        unsafe { Retained::cast_unchecked(metal_value) },
    ];
    let keys: Vec<&NSString> = vec![&format_key, &metal_key];
    NSDictionary::from_retained_objects(&keys, &objects)
}

/// Formátum-kód (négy karakteres OSType) emberi alakja.
pub fn fourcc(code: u32) -> String {
    let bytes = code.to_be_bytes();
    let s: String = bytes
        .iter()
        .map(|b| {
            if (0x20..0x7f).contains(b) {
                *b as char
            } else {
                '.'
            }
        })
        .collect();
    format!("{s} (0x{code:08x})")
}

/// Egy dekódolt frame: a pixel buffer életben tartja az IOSurface-t, amíg a GPU
/// használja. A hívó felelőssége addig életben tartani, amíg a render be nem fejeződik.
pub struct Frame {
    pub pixel_buffer: CFRetained<CVPixelBuffer>,
    pub width: u32,
    pub height: u32,
    pub pixel_format: u32,
    pub pts_secs: f64,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("size", &(self.width, self.height))
            .field("format", &fourcc(self.pixel_format))
            .field("pts", &self.pts_secs)
            .finish()
    }
}

pub struct VideoReader {
    reader: Retained<AVAssetReader>,
    output: Retained<AVAssetReaderOutput>,
    width: u32,
    height: u32,
    pub nominal_fps: f32,
    pub frame_count: u64,
    /// A videósáv hossza másodpercben.
    pub duration: f64,
}

impl VideoReader {
    /// Megnyit egy videófájlt, és felkészül a frame-ek szekvenciális olvasására.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_pixel_format(path, PIXEL_FORMAT_BGRA)
    }

    /// Megnyitás kért pixel-formátummal (pl. natív 420v a legolcsóbb úthoz).
    pub fn open_with_pixel_format(path: &Path, pixel_format: u32) -> Result<Self> {
        Self::open_impl(path, pixel_format, None)
    }

    /// Megnyitás egy időablakra [start, start+duration) a reverse/bounce
    /// lejátszáshoz. Az AVAssetReader a tartományon belül szekvenciálisan olvas.
    pub fn open_range(
        path: &Path,
        pixel_format: u32,
        start_secs: f64,
        duration_secs: f64,
    ) -> Result<Self> {
        Self::open_impl(
            path,
            pixel_format,
            Some((start_secs.max(0.0), duration_secs.max(0.001))),
        )
    }

    fn open_impl(
        path: &Path,
        pixel_format: u32,
        range: Option<(f64, f64)>,
    ) -> Result<Self> {
        let path_str = path
            .to_str()
            .ok_or_else(|| anyhow!("nem UTF-8 útvonal: {path:?}"))?;
        if !path.exists() {
            return Err(anyhow!("nincs ilyen fájl: {path_str}"));
        }

        unsafe {
            let ns_path = NSString::from_str(path_str);
            let url = NSURL::fileURLWithPath(&ns_path);
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);

            let media_type = AVMediaTypeVideo
                .ok_or_else(|| anyhow!("a videó médiatípus nem elérhető"))?;
            let tracks: Retained<NSArray<AVAssetTrack>> =
                asset.tracksWithMediaType(media_type);
            let track = tracks
                .firstObject()
                .ok_or_else(|| anyhow!("nincs videó-sáv a fájlban"))?;

            let reader = AVAssetReader::assetReaderWithAsset_error(&asset)
                .map_err(|e| anyhow!("AVAssetReader hiba: {e:?}"))?;
            // Pixel-formátum kérése: enélkül tömörített sample-ek jönnének.
            let settings = output_settings(pixel_format);
            let track_output =
                AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                    &track,
                    Some(&settings),
                );
            track_output.setAlwaysCopiesSampleData(false);
            if !reader.canAddOutput(&track_output) {
                return Err(anyhow!("nem adható hozzá a track output"));
            }
            let output: Retained<AVAssetReaderOutput> = track_output.into_super();
            reader.addOutput(&output);
            if let Some((start_secs, duration_secs)) = range {
                let start_time = CMTime::with_seconds(start_secs, 600);
                let duration_time = CMTime::with_seconds(duration_secs, 600);
                reader.setTimeRange(CMTimeRange::new(start_time, duration_time));
            }
            if !reader.startReading() {
                return Err(anyhow!(
                    "startReading sikertelen: {:?}",
                    reader.error().map(|e| format!("{e:?}"))
                ));
            }

            let size = track.naturalSize();
            let nominal_fps = track.nominalFrameRate();
            let range = track.timeRange();
            let duration = if range.duration.timescale != 0 {
                range.duration.value as f64 / range.duration.timescale as f64
            } else {
                0.0
            };

            Ok(Self {
                reader,
                output,
                width: size.width.max(0.0) as u32,
                height: size.height.max(0.0) as u32,
                nominal_fps,
                frame_count: 0,
                duration,
            })
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// A következő frame, vagy None, ha a fájl végére értünk.
    pub fn next_frame(&mut self) -> Result<Option<Frame>> {
        unsafe {
            let Some(sample) = self.output.copyNextSampleBuffer() else {
                // Vagy vége, vagy hiba.
                let status = self.reader.status();
                if status.0 == AVAssetReaderStatus::Failed.0 {
                    return Err(anyhow!(
                        "olvasási hiba: {:?}",
                        self.reader.error().map(|e| format!("{e:?}"))
                    ));
                }
                return Ok(None);
            };

            // A pixel buffer +1 referenciát ad, így a sample buffer eldobása után
            // is életben marad (amíg a GPU használja).
            let Some(pixel_buffer): Option<CFRetained<CVPixelBuffer>> = sample.image_buffer()
            else {
                return Err(anyhow!("a sample bufferben nincs kép"));
            };

            let pts = CMSampleBufferGetPresentationTimeStamp(&sample);
            let pts_secs = if pts.timescale != 0 {
                pts.value as f64 / pts.timescale as f64
            } else {
                0.0
            };

            self.frame_count += 1;
            Ok(Some(Frame {
                width: CVPixelBufferGetWidth(&pixel_buffer) as u32,
                height: CVPixelBufferGetHeight(&pixel_buffer) as u32,
                pixel_format: CVPixelBufferGetPixelFormatType(&pixel_buffer),
                pixel_buffer,
                pts_secs,
            }))
        }
    }

    /// Az olvasó állapota (hibakereséshez).
    pub fn error_text(&self) -> Option<String> {
        unsafe { self.reader.error() }.map(|e| format!("{e:?}"))
    }
}

#[allow(dead_code)]
fn _unused() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranged_reader_stays_inside_requested_window() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.testmedia/small.mov");
        if !path.exists() {
            return;
        }
        let mut reader = VideoReader::open_range(&path, PIXEL_FORMAT_BGRA, 1.0, 0.3)
            .expect("range reader nyílik");
        let mut count = 0;
        while let Some(frame) = reader.next_frame().expect("olvasás") {
            assert!(frame.pts_secs >= 0.95, "frame a range előtt: {}", frame.pts_secs);
            count += 1;
            if count >= 5 {
                break;
            }
        }
        assert!(count > 0, "nincs frame a kért időablakban");
    }
}

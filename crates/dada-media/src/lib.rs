//! Dada VJ — média: dekódolás és GPU-textúrává alakítás.
//!
//! Phase 0 / Spike A2: a kritikus kockázat mérése —
//! VideoToolbox/AVFoundation dekód → CVPixelBuffer → (zero-copy) Metal textúra.

pub mod frame;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "macos")]
pub use macos::reader::{fourcc, Frame, VideoReader};

//! Dada VJ — audio-elemzés.
//!
//! A bemenő hangot nem effektezzük: elemzésre használjuk. Az eredmény kétféle
//! formában megy tovább:
//!   1. GPU-textúrák (waveform / spektrum / spektrogram), amiket bármely shader
//!      fogyaszthat, és
//!   2. modulációs források (sávok, envelope-ok, onset), amiket bármely
//!      paraméterre köthetünk.

pub mod analysis;
pub mod source;

pub use analysis::{
    Analyzer, AudioState, BANDS, FFT_SIZE, SPECTROGRAM_COLS, SPECTROGRAM_ROWS, SPECTRUM_BINS,
    WAVEFORM_SAMPLES,
};
pub use source::{
    AnalyzerSnapshot, AudioDeviceInfo, AudioEngine, ChannelMeter, SourceKind, MAX_CHANNELS,
};

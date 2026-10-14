//! FFT-alapú audio-elemzés: spektrum, sávok, envelope-ok, onset.
//!
//! A nehéz részt (FFT) a render-szálon, hoponként futtatjuk: 2048 pontos FFT
//! 512-es hop-pal 48 kHz-en ~94 elemzés/másodperc, ami bőven elég a vizuálhoz,
//! és így az audio-callback soha nem számol semmit.

use rustfft::{num_complex::Complex, Fft, FftPlanner};
use std::sync::Arc;

/// Az FFT ablaka.
pub const FFT_SIZE: usize = 2048;
/// Ennyi új minta után futtatunk egy FFT-t.
pub const HOP: usize = 512;
/// A megjelenített spektrum felbontása.
pub const SPECTRUM_BINS: usize = 512;
/// A hullámforma-textúra hossza.
pub const WAVEFORM_SAMPLES: usize = 1024;
/// A spektrogram szélessége (idő) és magassága (frekvencia).
pub const SPECTROGRAM_COLS: usize = 512;
pub const SPECTROGRAM_ROWS: usize = 256;

/// A sávok nevei (a modulációs mátrixhoz).
pub const BAND_NAMES: [&str; 6] = ["sub", "bass", "low_mid", "mid", "high_mid", "high"];
/// A sávok száma.
pub const BANDS: usize = 6;

const BAND_EDGES_HZ: [(f32, f32); BANDS] = [
    (20.0, 60.0),
    (60.0, 120.0),
    (120.0, 400.0),
    (400.0, 2000.0),
    (2000.0, 6000.0),
    (6000.0, 16000.0),
];

/// Egy pillanatnyi elemzési eredmény.
#[derive(Debug, Clone)]
pub struct AudioState {
    /// 0..1 közé normalizált spektrum (log frekvencia-tengely).
    pub spectrum: Vec<f32>,
    /// A megjelenítendő hullámforma (-1..1).
    pub waveform: Vec<f32>,
    /// Nyers sávenergiák 0..1 között.
    pub bands: [f32; BANDS],
    /// Envelope-followed sávenergiák (támadás/elengedés simítással).
    pub band_env: [f32; BANDS],
    /// Teljes effektív érték 0..1.
    pub rms: f32,
    /// Csúcsérték 0..1.
    pub peak: f32,
    /// Spektrális fluxus (tranziens-energia).
    pub flux: f32,
    /// Igaz, ha ebben a hopban ütést detektáltunk.
    pub onset: bool,
    /// Detektált tempó (BPM) az onset-intervallumokból, ha van.
    pub tempo_bpm: Option<f32>,
}

impl Default for AudioState {
    fn default() -> Self {
        Self {
            spectrum: vec![0.0; SPECTRUM_BINS],
            waveform: vec![0.0; WAVEFORM_SAMPLES],
            bands: [0.0; BANDS],
            band_env: [0.0; BANDS],
            rms: 0.0,
            peak: 0.0,
            flux: 0.0,
            onset: false,
            tempo_bpm: None,
        }
    }
}

/// A spektrogram CPU-oldali gyűrűje.
///
/// A GPU-textúra 256 (frekvencia) x 512 (idő) méretű, ezért minden új elemzési
/// szelet egy **sor** (256 pixel = 1024 bájt), ami 256-bájtosan igazított, így
/// közvetlenül írható a textúrába. Ez 1 KB/hop a korábbi 512 KB/frame helyett.
pub struct Spectrogram {
    /// Teljes előzmény, soronként (idő x frekvencia).
    pub data: Vec<f32>,
    /// A legfrissebb idő-szelet (frekvencia-tengely).
    pub last_row: Vec<f32>,
    /// Hova írtuk (0..SPECTROGRAM_COLS).
    pub write_row: usize,
}

impl Spectrogram {
    pub fn new() -> Self {
        Self {
            data: vec![0.0; SPECTROGRAM_COLS * SPECTROGRAM_ROWS],
            last_row: vec![0.0; SPECTROGRAM_ROWS],
            write_row: 0,
        }
    }

    /// Új spektrum-szelet beírása (növekvő frekvencia).
    pub fn push_row(&mut self, spectrum: &[f32]) {
        let rows = SPECTROGRAM_ROWS.min(spectrum.len());
        for f in 0..rows {
            self.data[self.write_row * SPECTROGRAM_ROWS + f] = spectrum[f];
            self.last_row[f] = spectrum[f];
        }
        self.write_row = (self.write_row + 1) % SPECTROGRAM_COLS;
    }
}

impl Default for Spectrogram {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Analyzer {
    sample_rate: f32,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    ring: Vec<f32>,
    ring_pos: usize,
    since_hop: usize,
    complex: Vec<Complex<f32>>,
    magnitude: Vec<f32>,
    prev_magnitude: Vec<f32>,
    log_bins: Vec<(usize, usize)>,
    band_bins: [(usize, usize); BANDS],
    flux_mean: f32,
    last_onset_samples: Option<u64>,
    onset_intervals: [f32; 8],
    onset_count: usize,
    onset_idx: usize,
    sample_counter: u64,
    pub state: AudioState,
    pub spectrogram: Spectrogram,
    pub fft_runs: u64,
}

impl Analyzer {
    pub fn new(sample_rate: f32) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::PI * 2.0 * i as f32 / (FFT_SIZE as f32 - 1.0);
                0.5 - 0.5 * x.cos()
            })
            .collect();

        let bin_hz = sample_rate / FFT_SIZE as f32;
        let nyquist = sample_rate / 2.0;
        let min_hz = 30.0f32.max(bin_hz);

        let log_bins: Vec<(usize, usize)> = (0..SPECTRUM_BINS)
            .map(|i| {
                let t0 = i as f32 / SPECTRUM_BINS as f32;
                let t1 = (i + 1) as f32 / SPECTRUM_BINS as f32;
                let f0 = min_hz * (nyquist / min_hz).powf(t0);
                let f1 = min_hz * (nyquist / min_hz).powf(t1);
                let b0 = (f0 / bin_hz).floor().max(1.0) as usize;
                let b1 = (f1 / bin_hz).ceil().max(b0 as f32 + 1.0) as usize;
                (b0.min(FFT_SIZE / 2 - 1), b1.min(FFT_SIZE / 2))
            })
            .collect();

        let band_bins = BAND_EDGES_HZ.map(|(lo, hi)| {
            let b0 = ((lo / bin_hz).floor().max(1.0) as usize).min(FFT_SIZE / 2 - 1);
            let b1 = ((hi / bin_hz).ceil() as usize).clamp(b0 + 1, FFT_SIZE / 2);
            (b0, b1)
        });

        Self {
            sample_rate,
            fft,
            window,
            ring: vec![0.0; FFT_SIZE],
            ring_pos: 0,
            since_hop: 0,
            complex: vec![Complex::default(); FFT_SIZE],
            magnitude: vec![0.0; FFT_SIZE / 2],
            prev_magnitude: vec![0.0; FFT_SIZE / 2],
            log_bins,
            band_bins,
            flux_mean: 0.0,
            last_onset_samples: None,
            onset_intervals: [0.0; 8],
            onset_count: 0,
            onset_idx: 0,
            sample_counter: 0,
            state: AudioState::default(),
            spectrogram: Spectrogram::new(),
            fft_runs: 0,
        }
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Új hangminták etetése. Hoponként lefuttatja az FFT-t és frissíti az állapotot.
    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            self.ring[self.ring_pos] = s;
            self.ring_pos = (self.ring_pos + 1) % FFT_SIZE;
            self.since_hop += 1;
            self.sample_counter += 1;
            if self.since_hop >= HOP {
                self.since_hop = 0;
                self.analyze_hop();
            }
        }
        self.update_waveform();
        self.update_levels();
    }

    fn analyze_hop(&mut self) {
        let mut sum_sq = 0.0f32;
        for i in 0..FFT_SIZE {
            let idx = (self.ring_pos + i) % FFT_SIZE;
            let s = self.ring[idx];
            sum_sq += s * s;
            self.complex[i] = Complex::new(s * self.window[i], 0.0);
        }
        self.state.rms = (sum_sq / FFT_SIZE as f32).sqrt();
        self.fft.process(&mut self.complex);
        self.fft_runs += 1;

        let norm = 2.0 / FFT_SIZE as f32;
        let mut flux = 0.0f32;
        for i in 0..FFT_SIZE / 2 {
            let m = self.complex[i].norm() * norm;
            self.magnitude[i] = m;
            let d = m - self.prev_magnitude[i];
            if d > 0.0 {
                flux += d;
            }
            self.prev_magnitude[i] = m;
        }
        self.state.flux = flux;

        for (i, (b0, b1)) in self.log_bins.iter().enumerate() {
            let mut acc = 0.0f32;
            for b in *b0..*b1 {
                acc = acc.max(self.magnitude[b]);
            }
            let db = 20.0 * (acc.max(1e-7)).log10();
            self.state.spectrum[i] = ((db + 80.0) / 80.0).clamp(0.0, 1.0);
        }

        for (i, (b0, b1)) in self.band_bins.iter().enumerate() {
            let mut acc = 0.0f32;
            for b in *b0..*b1 {
                acc += self.magnitude[b];
            }
            let mean = acc / (*b1 - *b0).max(1) as f32;
            let db = 20.0 * (mean.max(1e-7)).log10();
            self.state.bands[i] = ((db + 70.0) / 70.0).clamp(0.0, 1.0);
        }

        for i in 0..BANDS {
            let target = self.state.bands[i];
            let cur = self.state.band_env[i];
            let a = if target > cur { 0.55 } else { 0.06 };
            self.state.band_env[i] = cur + (target - cur) * a;
        }

        self.flux_mean = self.flux_mean * 0.95 + flux * 0.05;
        let threshold = self.flux_mean * 2.2 + 1e-5;
        let is_onset = flux > threshold && flux > 0.02;
        self.state.onset = is_onset;
        if is_onset {
            if let Some(prev) = self.last_onset_samples {
                let dt = (self.sample_counter - prev) as f32 / self.sample_rate;
                if dt > 0.15 && dt < 2.0 {
                    self.onset_intervals[self.onset_idx % 8] = dt;
                    self.onset_idx += 1;
                    self.onset_count = (self.onset_count + 1).min(8);
                    if self.onset_count >= 4 {
                        let sum: f32 = self.onset_intervals.iter().sum();
                        let mean = sum / self.onset_count as f32;
                        if mean > 0.0 {
                            self.state.tempo_bpm = Some(60.0 / mean);
                        }
                    }
                }
            }
            self.last_onset_samples = Some(self.sample_counter);
        }

        self.spectrogram.push_row(&self.state.spectrum);
    }

    fn update_waveform(&mut self) {
        let n = WAVEFORM_SAMPLES.min(FFT_SIZE);
        let start = self.ring_pos;
        for i in 0..n {
            let idx = (start + FFT_SIZE - n + i) % FFT_SIZE;
            self.state.waveform[i] = self.ring[idx];
        }
    }

    fn update_levels(&mut self) {
        let mut peak = 0.0f32;
        let mut sum = 0.0f32;
        for i in 0..WAVEFORM_SAMPLES {
            let v = self.state.waveform[i].abs();
            if v > peak {
                peak = v;
            }
            sum += v * v;
        }
        self.state.peak = peak;
        self.state.rms = (sum / WAVEFORM_SAMPLES as f32).sqrt();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_bin_for_hz(hz: f32, sample_rate: f32) -> usize {
        let nyquist = sample_rate / 2.0;
        let min_hz = 30.0f32;
        let t = (hz / min_hz).ln() / (nyquist / min_hz).ln();
        (t * SPECTRUM_BINS as f32) as usize
    }

    #[test]
    fn detects_sine_peak() {
        let sr = 48000.0;
        let mut a = Analyzer::new(sr);
        let hz = 1000.0f32;
        let samples: Vec<f32> = (0..FFT_SIZE * 2)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / sr).sin() * 0.7)
            .collect();
        a.push(&samples);
        let expected = render_bin_for_hz(hz, sr);
        let (argmax, _) = a
            .state
            .spectrum
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.partial_cmp(y.1).unwrap())
            .unwrap();
        assert!(
            (argmax as i32 - expected as i32).abs() <= 12,
            "argmax={argmax}, vart={expected}"
        );
        assert!(a.state.band_env[3] > 0.3, "mid sav: {}", a.state.band_env[3]);
        assert!(a.state.peak > 0.5);
    }

    #[test]
    fn silence_is_quiet() {
        let mut a = Analyzer::new(48000.0);
        a.push(&vec![0.0f32; FFT_SIZE * 2]);
        assert!(a.state.rms < 1e-3);
        assert!(a.state.band_env.iter().all(|b| *b < 0.05), "{:?}", a.state.band_env);
        assert!(a.state.spectrum.iter().all(|s| *s < 0.05));
    }

    #[test]
    fn low_frequency_lands_in_sub_or_bass() {
        let sr = 48000.0;
        let mut a = Analyzer::new(sr);
        let hz = 50.0f32;
        let samples: Vec<f32> = (0..FFT_SIZE * 3)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / sr).sin() * 0.9)
            .collect();
        a.push(&samples);
        assert!(
            a.state.band_env[0] > 0.3 || a.state.band_env[1] > 0.3,
            "sub={:.2} bass={:.2}",
            a.state.band_env[0],
            a.state.band_env[1]
        );
    }

    #[test]
    fn spectrogram_wraps() {
        let mut sp = Spectrogram::new();
        let col = vec![0.5f32; SPECTRUM_BINS];
        for _ in 0..(SPECTROGRAM_COLS + 3) {
            sp.push_row(&col);
        }
        assert_eq!(sp.write_row, 3);
        assert!(sp.last_row.iter().all(|v| (*v - 0.5).abs() < 1e-6));
    }
}

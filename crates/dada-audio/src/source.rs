//! Hangforrások: valódi bemenet (cpal) és szintetikus teszt-forrás.
//!
//! A szintetikus forrás azért kell, mert így a teljes elemzési és vizuális lánc
//! **mikrofonengedély és hardver nélkül** is determinisztikusan tesztelhető.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::analysis::Analyzer;

/// Honnan jön a hang.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceKind {
    /// Szintetikus teszt-jel (dobgép-szerű), hardver nélkül.
    Synthetic,
    /// Valódi bemenet a megadott eszközről (None = alapértelmezett).
    Input { device: Option<String> },
}

impl Default for SourceKind {
    fn default() -> Self {
        SourceKind::Synthetic
    }
}

struct Shared {
    analyzer: Mutex<Analyzer>,
    incoming: Mutex<Vec<f32>>,
    error: Mutex<Option<String>>,
    samples_in: AtomicU64,
    running: AtomicBool,
}

impl Shared {
    fn new(sample_rate: f32) -> Self {
        Self {
            analyzer: Mutex::new(Analyzer::new(sample_rate)),
            incoming: Mutex::new(Vec::with_capacity(8192)),
            error: Mutex::new(None),
            samples_in: AtomicU64::new(0),
            running: AtomicBool::new(false),
        }
    }
}

/// Az audio-motor: forrás + elemzés.
///
/// A render-szál hívja a process metódust frame-enként; az audio-szál csak
/// másol. Ez a spike egyszerűsítése: a későbbi verzióban a minták lock-free
/// gyűrűn mennek át, és a nehéz elemzés külön szálon fut.
pub struct AudioEngine {
    shared: Arc<Shared>,
    kind: SourceKind,
    sample_rate: f32,
    _stream: Option<cpal::Stream>,
    synthetic_phase: f64,
    synthetic_n: u64,
    /// A szintetikus forrás indulási ideje (valós idejű ütemezéshez).
    synth_start: std::time::Instant,
}

impl AudioEngine {
    /// Szintetikus forrás létrehozása (teszthez, demóhoz).
    pub fn synthetic(sample_rate: f32) -> Self {
        Self {
            shared: Arc::new(Shared::new(sample_rate)),
            kind: SourceKind::Synthetic,
            sample_rate,
            _stream: None,
            synthetic_phase: 0.0,
            synthetic_n: 0,
            synth_start: std::time::Instant::now(),
        }
    }

    /// Valódi bemenet megnyitása.
    pub fn input(sample_rate: f32, device_name: Option<&str>) -> Result<Self> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

        let host = cpal::default_host();
        let device = match device_name {
            Some(name) => host
                .input_devices()?
                .find(|d| d.name().map(|n| n == name).unwrap_or(false))
                .ok_or_else(|| anyhow!("nincs ilyen bemenet: {name}"))?,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow!("nincs alapértelmezett bemenet"))?,
        };
        let actual_name = device.name().unwrap_or_else(|_| "?".into());
        let config = device.default_input_config()?;
        let actual_rate = config.sample_rate() as f32;
        let _ = sample_rate;

        let shared = Arc::new(Shared::new(actual_rate));
        let shared_cb = Arc::clone(&shared);
        let shared_err = Arc::clone(&shared);
        let channels = config.channels() as usize;

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                if let Ok(mut buf) = shared_cb.incoming.try_lock() {
                    if buf.len() < (1 << 20) {
                        if channels <= 1 {
                            buf.extend_from_slice(data);
                        } else {
                            for frame in data.chunks(channels) {
                                buf.push(frame[0]);
                            }
                        }
                    }
                }
                shared_cb
                    .samples_in
                    .fetch_add(data.len() as u64, Ordering::Relaxed);
            },
            move |err| {
                if let Ok(mut e) = shared_err.error.lock() {
                    *e = Some(format!("audio stream hiba: {err}"));
                }
            },
            None,
        )?;
        stream.play()?;
        log::info!("audio bemenet: {actual_name} @ {actual_rate} Hz");

        Ok(Self {
            shared,
            kind: SourceKind::Input {
                device: device_name.map(|s| s.to_string()),
            },
            sample_rate: actual_rate,
            _stream: Some(stream),
            synthetic_phase: 0.0,
            synthetic_n: 0,
            synth_start: std::time::Instant::now(),
        })
    }

    pub fn kind(&self) -> &SourceKind {
        &self.kind
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|e| e.clone())
    }

    /// Frame-enként hívandó: átveszi az új mintákat és lefuttatja az elemzést.
    pub fn process(&mut self) -> AnalyzerSnapshot {
        self.process_at(None)
    }

    /// Mint a process, de a szintetikus forrás órája explicit (teszthez).
    pub fn process_at(&mut self, clock: Option<f64>) -> AnalyzerSnapshot {
        // A szintetikus generálást a lock előtt végezzük, hogy ne ütközzön
        // a self.shared kölcsönzésével.
        let synthetic = self.kind == SourceKind::Synthetic;
        let mut synth_buf: Vec<f32> = Vec::new();
        if synthetic {
            // Valós idejű ütemezés: annyi mintát generálunk, amennyi a forrás
            // indulása óta eltelt. Így a szintetikus jel ugyanúgy viselkedik,
            // mint egy valódi bemenet (nem függ a render fps-étől).
            let elapsed = clock.unwrap_or_else(|| self.synth_start.elapsed().as_secs_f64());
            let target = (elapsed * self.sample_rate as f64) as u64;
            let want = target.saturating_sub(self.synthetic_n).min(8192) as usize;
            if want > 0 {
                synth_buf.resize(want, 0.0);
                self.fill_synthetic(&mut synth_buf);
            }
        }

        let taken = if synthetic {
            None
        } else {
            match self.shared.incoming.try_lock() {
                Ok(mut incoming) => {
                    if incoming.is_empty() {
                        None
                    } else {
                        Some(std::mem::take(&mut *incoming))
                    }
                }
                Err(_) => None,
            }
        };

        let Ok(mut analyzer) = self.shared.analyzer.lock() else {
            return AnalyzerSnapshot::default();
        };
        // A nyers mono minták megőrzése (a hang-streamhez) — az elemzés
        // ugyanezeket a mintákat kapja.
        let mono = if synthetic {
            if !synth_buf.is_empty() {
                analyzer.push(&synth_buf);
            }
            synth_buf
        } else {
            match taken {
                Some(samples) => {
                    analyzer.push(&samples);
                    samples
                }
                None => Vec::new(),
            }
        };
        let mut snap = snapshot(&analyzer);
        snap.mono = mono;
        snap
    }

    /// A szintetikus jel a bemenetre generálódik (nem a hangszóróra): a hang
    /// nem szólal meg, csak a vizuál kap jelet. 120 BPM-es mini-dobgép.
    fn fill_synthetic(&mut self, out: &mut [f32]) {
        let sr = self.sample_rate as f64;
        for s in out.iter_mut() {
            let t = self.synthetic_n as f64 / sr;
            let beat = (t * 2.0).fract();
            let bar = (t * 0.5).fract();

            let kick_env = (-beat * 18.0).exp();
            let kick = (2.0 * std::f64::consts::PI * 55.0 * t).sin() * kick_env * 0.9;

            let bass_note = if ((t * 4.0) as i64) % 2 == 0 { 82.0 } else { 110.0 };
            let bass_env = (-(t * 4.0).fract() * 6.0).exp();
            let bass = (2.0 * std::f64::consts::PI * bass_note * t).sin() * bass_env * 0.35;

            let hat_phase = (t * 4.0).fract();
            let hat_env = (-hat_phase * 60.0).exp();
            let noise =
                ((self.synthetic_n.wrapping_mul(2654435761) >> 8) as f64 / 8388608.0) - 1.0;
            let hat = noise * hat_env * 0.18;

            let sweep = (2.0 * std::f64::consts::PI * (400.0 + 600.0 * bar) * t).sin() * 0.12;

            *s = (kick + bass + hat + sweep) as f32;
            self.synthetic_n += 1;
        }
    }

    pub fn samples_in(&self) -> u64 {
        self.shared.samples_in.load(Ordering::Relaxed)
    }
}

/// Pillanatkép az elemzési állapotról (a render-szál ezt használja).
#[derive(Debug, Clone, Default)]
pub struct AnalyzerSnapshot {
    pub state: crate::analysis::AudioState,
    /// A legfrissebb spektrogram-sor (frekvencia-tengely, SPECTROGRAM_ROWS elem).
    pub spectrogram_row: Vec<f32>,
    /// Hányadik sorba került (0..SPECTROGRAM_COLS).
    pub spectrogram_row_index: usize,
    pub fft_runs: u64,
    /// A most feldolgozott nyers mono minták (a hang-stream kódolásához).
    pub mono: Vec<f32>,
}

impl AnalyzerSnapshot {
    pub fn is_empty(&self) -> bool {
        self.state.spectrum.iter().all(|v| *v == 0.0)
            && self.state.band_env.iter().all(|v| *v == 0.0)
    }
}

fn snapshot(analyzer: &Analyzer) -> AnalyzerSnapshot {
    AnalyzerSnapshot {
        state: analyzer.state.clone(),
        spectrogram_row: analyzer.spectrogram.last_row.clone(),
        spectrogram_row_index: analyzer.spectrogram.write_row,
        fft_runs: analyzer.fft_runs,
        mono: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_source_produces_signal() {
        let mut engine = AudioEngine::synthetic(48000.0);
        let mut snap = AnalyzerSnapshot::default();
        // 1 másodpercnyi feldolgozás.
        for i in 0..100 {
            snap = engine.process_at(Some(i as f64 / 60.0));
        }
        assert!(snap.fft_runs > 50, "fft futások: {}", snap.fft_runs);
        assert!(snap.state.peak > 0.1, "csúcs: {}", snap.state.peak);
        let max_band = snap.state.band_env.iter().cloned().fold(0.0f32, f32::max);
        assert!(max_band > 0.2, "sávok: {:?}", snap.state.band_env);
        assert!(snap.spectrogram_row.iter().any(|v| *v > 0.0));
    }

    #[test]
    fn synthetic_has_kick_onsets() {
        let mut engine = AudioEngine::synthetic(48000.0);
        let mut onsets = 0;
        for i in 0..200 {
            let snap = engine.process_at(Some(i as f64 / 60.0));
            if snap.state.onset {
                onsets += 1;
            }
        }
        assert!(onsets >= 3, "onsetek: {onsets}");
    }
}

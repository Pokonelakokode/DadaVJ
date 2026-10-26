//! Hangforrások: valódi bemenet (cpal) és szintetikus teszt-forrás.
//!
//! A szintetikus forrás azért kell, mert így a teljes elemzési és vizuális lánc
//! **mikrofonengedély és hardver nélkül** is determinisztikusan tesztelhető.

use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::analysis::Analyzer;

/// Maximálisan külön követett bemeneti csatornák száma (pl. Scarlett 18i8 = 20 csatorna Loopbackkel együtt).
pub const MAX_CHANNELS: usize = 32;

/// Egy audio bemeneti eszköz adatai.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioDeviceInfo {
    pub name: String,
    pub channels: u16,
}

/// Egyetlen bemeneti csatorna (sáv) pillanatnyi elemzése.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelMeter {
    pub rms: f32,
    pub peak: f32,
    pub low: f32,
    pub mid: f32,
    pub high: f32,
    pub onset: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct ChannelTracker {
    lp_low: f32,
    lp_mid: f32,
    env_rms: f32,
    env_low: f32,
    env_mid: f32,
    env_high: f32,
    prev_energy: f32,
    cooldown: u32,
}

impl ChannelTracker {
    fn process_chunk(&mut self, samples: &[f32]) -> ChannelMeter {
        if samples.is_empty() {
            self.env_rms *= 0.92;
            self.env_low *= 0.92;
            self.env_mid *= 0.92;
            self.env_high *= 0.92;
            return ChannelMeter {
                rms: self.env_rms,
                peak: self.env_rms,
                low: self.env_low,
                mid: self.env_mid,
                high: self.env_high,
                onset: false,
            };
        }
        let mut sum_sq = 0.0f32;
        let mut peak = 0.0f32;
        let mut low_sq = 0.0f32;
        let mut mid_sq = 0.0f32;
        let mut high_sq = 0.0f32;

        // Egyszerű, nagyon gyors 1-pólusú szűrők (~200 Hz és ~2000 Hz @ 48 kHz)
        for &x in samples {
            let ax = x.abs();
            if ax > peak {
                peak = ax;
            }
            sum_sq += x * x;
            self.lp_low += 0.026 * (x - self.lp_low);
            self.lp_mid += 0.22 * (x - self.lp_mid);
            let low = self.lp_low;
            let mid = self.lp_mid - self.lp_low;
            let high = x - self.lp_mid;
            low_sq += low * low;
            mid_sq += mid * mid;
            high_sq += high * high;
        }
        let inv = 1.0 / (samples.len() as f32);
        let rms = (sum_sq * inv).sqrt().min(1.0);
        let low = ((low_sq * inv).sqrt() * 3.2).min(1.0);
        let mid = ((mid_sq * inv).sqrt() * 3.5).min(1.0);
        let high = ((high_sq * inv).sqrt() * 4.5).min(1.0);

        self.env_rms = if rms > self.env_rms {
            rms
        } else {
            self.env_rms * 0.85 + rms * 0.15
        };
        self.env_low = if low > self.env_low {
            low
        } else {
            self.env_low * 0.85 + low * 0.15
        };
        self.env_mid = if mid > self.env_mid {
            mid
        } else {
            self.env_mid * 0.85 + mid * 0.15
        };
        self.env_high = if high > self.env_high {
            high
        } else {
            self.env_high * 0.85 + high * 0.15
        };

        if self.cooldown > 0 {
            self.cooldown -= 1;
        }
        let energy = rms + low * 0.8;
        let onset = self.cooldown == 0 && energy > 0.08 && energy > self.prev_energy * 1.45 + 0.04;
        if onset {
            self.cooldown = 5;
        }
        self.prev_energy = self.prev_energy * 0.8 + energy * 0.2;

        ChannelMeter {
            rms: self.env_rms,
            peak: peak.min(1.0),
            low: self.env_low,
            mid: self.env_mid,
            high: self.env_high,
            onset,
        }
    }
}

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
    /// Csatornánkénti bejövő minták.
    channel_incoming: Mutex<Vec<Vec<f32>>>,
    error: Mutex<Option<String>>,
    samples_in: AtomicU64,
    running: AtomicBool,
}

impl Shared {
    fn new(sample_rate: f32, channels: usize) -> Self {
        let ch = channels.clamp(1, MAX_CHANNELS);
        Self {
            analyzer: Mutex::new(Analyzer::new(sample_rate)),
            channel_incoming: Mutex::new((0..ch).map(|_| Vec::with_capacity(4096)).collect()),
            error: Mutex::new(None),
            samples_in: AtomicU64::new(0),
            running: AtomicBool::new(false),
        }
    }
}

/// Az audio-motor: forrás + elemzés (többcsatornás és sztereó pár támogatással).
pub struct AudioEngine {
    shared: Arc<Shared>,
    kind: SourceKind,
    sample_rate: f32,
    num_channels: usize,
    /// A fő vizuálhoz/FFT-hez kiválasztott csatornák (üres = összes csatorna mixe, 1 elem = mono, 2 elem = sztereó pár).
    selected_channels: Vec<usize>,
    trackers: Vec<ChannelTracker>,
    last_meters: Vec<ChannelMeter>,
    _stream: Option<cpal::Stream>,
    synthetic_phase: f64,
    synthetic_n: u64,
    /// A szintetikus forrás indulási ideje (valós idejű ütemezéshez).
    synth_start: std::time::Instant,
}

impl AudioEngine {
    /// Szintetikus forrás létrehozása (teszthez, demóhoz — 4 külön szintetikus sávval).
    pub fn synthetic(sample_rate: f32) -> Self {
        let num_channels = 4;
        Self {
            shared: Arc::new(Shared::new(sample_rate, num_channels)),
            kind: SourceKind::Synthetic,
            sample_rate,
            num_channels,
            selected_channels: Vec::new(),
            trackers: vec![ChannelTracker::default(); num_channels],
            last_meters: vec![ChannelMeter::default(); num_channels],
            _stream: None,
            synthetic_phase: 0.0,
            synthetic_n: 0,
            synth_start: std::time::Instant::now(),
        }
    }

    /// Valódi bemenet megnyitása (az összes csatornával).
    pub fn input(sample_rate: f32, device_name: Option<&str>) -> Result<Self> {
        Self::input_with_channels(sample_rate, device_name, Vec::new())
    }

    /// Valódi bemenet megnyitása megadott fő csatornával (None = összes mixe).
    pub fn input_with_channel(
        sample_rate: f32,
        device_name: Option<&str>,
        selected_channel: Option<usize>,
    ) -> Result<Self> {
        Self::input_with_channels(
            sample_rate,
            device_name,
            selected_channel.into_iter().collect(),
        )
    }

    /// Valódi bemenet megnyitása megadott fő csatornákkal (üres = összes mixe, 1 = mono, 2 = sztereó pár).
    pub fn input_with_channels(
        sample_rate: f32,
        device_name: Option<&str>,
        selected_channels: Vec<usize>,
    ) -> Result<Self> {
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

        let hw_channels = (config.channels() as usize).max(1);
        let tracked_channels = hw_channels.min(MAX_CHANNELS);
        let shared = Arc::new(Shared::new(actual_rate, tracked_channels));
        let shared_cb = Arc::clone(&shared);
        let shared_err = Arc::clone(&shared);

        let stream = device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                if let Ok(mut ch_bufs) = shared_cb.channel_incoming.try_lock() {
                    let cap_ok = ch_bufs
                        .first()
                        .map(|b| b.len() < (1 << 18))
                        .unwrap_or(false);
                    if cap_ok {
                        if hw_channels <= 1 {
                            if let Some(b0) = ch_bufs.get_mut(0) {
                                b0.extend_from_slice(data);
                            }
                        } else {
                            for frame in data.chunks(hw_channels) {
                                for (c, slot) in ch_bufs.iter_mut().enumerate() {
                                    if let Some(&sample) = frame.get(c) {
                                        slot.push(sample);
                                    }
                                }
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
        let filtered_channels: Vec<usize> = selected_channels
            .into_iter()
            .filter(|&c| c < tracked_channels)
            .collect();
        log::info!(
            "audio bemenet: {actual_name} @ {actual_rate} Hz ({hw_channels} csatorna, fő={:?})",
            filtered_channels
        );

        Ok(Self {
            shared,
            kind: SourceKind::Input {
                device: device_name.map(|s| s.to_string()),
            },
            sample_rate: actual_rate,
            num_channels: tracked_channels,
            selected_channels: filtered_channels,
            trackers: vec![ChannelTracker::default(); tracked_channels],
            last_meters: vec![ChannelMeter::default(); tracked_channels],
            _stream: Some(stream),
            synthetic_phase: 0.0,
            synthetic_n: 0,
            synth_start: std::time::Instant::now(),
        })
    }

    /// Elérhető audio bemeneti eszközök felsorolása.
    pub fn list_devices() -> Vec<String> {
        Self::list_devices_detailed()
            .into_iter()
            .map(|d| d.name)
            .collect()
    }

    /// Elérhető audio bemeneti eszközök felsorolása csatornaszámmal együtt.
    pub fn list_devices_detailed() -> Vec<AudioDeviceInfo> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let host = cpal::default_host();
        let Ok(devices) = host.input_devices() else {
            return Vec::new();
        };
        devices
            .filter_map(|d| {
                let name = d.name().ok()?;
                let channels = d.default_input_config().map(|c| c.channels()).unwrap_or(1);
                Some(AudioDeviceInfo { name, channels })
            })
            .collect()
    }

    pub fn kind(&self) -> &SourceKind {
        &self.kind
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn num_channels(&self) -> usize {
        self.num_channels
    }

    pub fn selected_channel(&self) -> Option<usize> {
        self.selected_channels.first().copied()
    }

    pub fn selected_channels(&self) -> &[usize] {
        &self.selected_channels
    }

    /// Kanonikus csatorna-specifikáció a UI számára: "-1" (Mix), "10" (Mono CH11), "10-11" (Sztereó CH11-12).
    pub fn selected_channel_spec(&self) -> String {
        Self::format_channel_spec(&self.selected_channels)
    }

    pub fn format_channel_spec(channels: &[usize]) -> String {
        if channels.is_empty() {
            "-1".to_string()
        } else {
            channels
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join("-")
        }
    }

    /// Csatorna-specifikáció értelmezése (0-indexelt: "-1" -> [], "10" -> [10], "10-11" -> [10, 11]).
    pub fn parse_channel_spec(spec: &str) -> Vec<usize> {
        let s = spec.trim();
        if s.is_empty() || s == "-1" || s.eq_ignore_ascii_case("mix") {
            return Vec::new();
        }
        s.split(|c: char| c == '-' || c == ',' || c == '+' || c == '/')
            .filter_map(|p| p.trim().parse::<usize>().ok())
            .filter(|&c| c < MAX_CHANNELS)
            .collect()
    }

    pub fn set_selected_channel(&mut self, channel: Option<usize>) {
        self.set_selected_channels(channel.into_iter().collect());
    }

    pub fn set_selected_channels(&mut self, channels: Vec<usize>) {
        self.selected_channels = channels
            .into_iter()
            .filter(|&c| c < self.num_channels)
            .collect();
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
        let synthetic = self.kind == SourceKind::Synthetic;
        let mut synth_buf: Vec<f32> = Vec::new();
        let mut synth_ch: [Vec<f32>; 4] = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
        if synthetic {
            let elapsed = clock.unwrap_or_else(|| self.synth_start.elapsed().as_secs_f64());
            let target = (elapsed * self.sample_rate as f64) as u64;
            let want = target.saturating_sub(self.synthetic_n).min(8192) as usize;
            if want > 0 {
                synth_buf.resize(want, 0.0);
                for ch in &mut synth_ch {
                    ch.resize(want, 0.0);
                }
                self.fill_synthetic_stems(&mut synth_buf, &mut synth_ch);
            }
        }

        let taken_channels: Option<Vec<Vec<f32>>> = if synthetic {
            None
        } else {
            match self.shared.channel_incoming.try_lock() {
                Ok(mut ch_in) => {
                    if ch_in.iter().all(|v| v.is_empty()) {
                        None
                    } else {
                        Some(ch_in.iter_mut().map(std::mem::take).collect())
                    }
                }
                Err(_) => None,
            }
        };

        if synthetic {
            if !synth_buf.is_empty() {
                for (i, ch) in synth_ch.iter().enumerate() {
                    if let (Some(tr), Some(m)) =
                        (self.trackers.get_mut(i), self.last_meters.get_mut(i))
                    {
                        *m = tr.process_chunk(ch);
                    }
                }
            }
        } else if let Some(ref ch_vecs) = taken_channels {
            for (i, ch) in ch_vecs.iter().enumerate() {
                if let (Some(tr), Some(m)) = (self.trackers.get_mut(i), self.last_meters.get_mut(i))
                {
                    *m = tr.process_chunk(ch);
                }
            }
        }

        let Ok(mut analyzer) = self.shared.analyzer.lock() else {
            return AnalyzerSnapshot::default();
        };

        let mono = if synthetic {
            let valid: Vec<&Vec<f32>> = self
                .selected_channels
                .iter()
                .filter_map(|&c| synth_ch.get(c))
                .collect();
            let chosen = if valid.is_empty() {
                synth_buf
            } else if valid.len() == 1 {
                valid[0].clone()
            } else {
                let len = valid[0].len();
                let scale = 1.0 / (valid.len() as f32).sqrt();
                let mut mix = vec![0.0f32; len];
                for ch in valid {
                    for (i, &s) in ch.iter().take(len).enumerate() {
                        mix[i] += s * scale;
                    }
                }
                mix
            };
            if !chosen.is_empty() {
                analyzer.push(&chosen);
            }
            chosen
        } else {
            match taken_channels {
                Some(ch_vecs) => {
                    let valid: Vec<&Vec<f32>> = self
                        .selected_channels
                        .iter()
                        .filter_map(|&c| ch_vecs.get(c))
                        .collect();
                    let chosen = if valid.len() == 1 {
                        valid[0].clone()
                    } else if valid.len() >= 2 {
                        let len = valid.first().map(|v| v.len()).unwrap_or(0);
                        let scale = 1.0 / (valid.len() as f32).sqrt();
                        let mut mix = vec![0.0f32; len];
                        for ch in valid {
                            for (i, &s) in ch.iter().take(len).enumerate() {
                                mix[i] += s * scale;
                            }
                        }
                        mix
                    } else {
                        let len = ch_vecs.first().map(|v| v.len()).unwrap_or(0);
                        if ch_vecs.len() <= 1 {
                            ch_vecs.into_iter().next().unwrap_or_default()
                        } else {
                            let n_ch = ch_vecs.len() as f32;
                            let mut mix = vec![0.0f32; len];
                            for ch in &ch_vecs {
                                for (i, &s) in ch.iter().take(len).enumerate() {
                                    mix[i] += s;
                                }
                            }
                            let scale = 1.0 / n_ch.sqrt();
                            for s in &mut mix {
                                *s *= scale;
                            }
                            mix
                        }
                    };
                    if !chosen.is_empty() {
                        analyzer.push(&chosen);
                    }
                    chosen
                }
                None => Vec::new(),
            }
        };

        let mut snap = snapshot(&analyzer);
        snap.mono = mono;
        snap.channels = self.last_meters.clone();
        snap.num_channels = self.num_channels;
        snap.selected_channel = self.selected_channels.first().copied();
        snap.selected_channels = self.selected_channels.clone();
        snap
    }

    /// A szintetikus jel a bemenetre generálódik (4 sáv: 1=Kick, 2=Bass, 3=Hat, 4=Sweep).
    fn fill_synthetic_stems(&mut self, out: &mut [f32], stems: &mut [Vec<f32>; 4]) {
        let sr = self.sample_rate as f64;
        for (i, s) in out.iter_mut().enumerate() {
            let t = self.synthetic_n as f64 / sr;
            let beat = (t * 2.0).fract();
            let bar = (t * 0.5).fract();

            let kick_env = (-beat * 18.0).exp();
            let kick = (2.0 * std::f64::consts::PI * 55.0 * t).sin() * kick_env * 0.9;

            let bass_note = if ((t * 4.0) as i64) % 2 == 0 {
                82.0
            } else {
                110.0
            };
            let bass_env = (-(t * 4.0).fract() * 6.0).exp();
            let bass = (2.0 * std::f64::consts::PI * bass_note * t).sin() * bass_env * 0.35;

            let hat_phase = (t * 4.0).fract();
            let hat_env = (-hat_phase * 60.0).exp();
            let noise = ((self.synthetic_n.wrapping_mul(2654435761) >> 8) as f64 / 8388608.0) - 1.0;
            let hat = noise * hat_env * 0.18;

            let sweep = (2.0 * std::f64::consts::PI * (400.0 + 600.0 * bar) * t).sin() * 0.12;

            stems[0][i] = kick as f32;
            stems[1][i] = bass as f32;
            stems[2][i] = hat as f32;
            stems[3][i] = sweep as f32;
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
    /// Csatornánkénti (multi-input) jelszintek és sávok.
    pub channels: Vec<ChannelMeter>,
    pub num_channels: usize,
    pub selected_channel: Option<usize>,
    pub selected_channels: Vec<usize>,
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
        channels: Vec::new(),
        num_channels: 0,
        selected_channel: None,
        selected_channels: Vec::new(),
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

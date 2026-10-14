//! Zenei óra: MIDI clock (24 PPQN) helyreállítás és a transport idővonala.
//!
//! A nyers MIDI clock jitteres (tipikusan ±1–3 ms), ezért nem közvetlenül
//! hajtjuk vele a vizuált. Ehelyett az utolsó néhány tickre egyenest
//! illesztünk (tick-szám -> monoton másodperc), a tempót a meredekségből
//! olvassuk, a fázist pedig az utolsó tickhez horgonyozva interpoláljuk.
//! Így egy késve érkező tick nem okoz látható remegést.

use std::collections::VecDeque;

/// MIDI clock felbontás: 24 tick / negyed.
pub const PPQN: u32 = 24;

/// Értelmes tempóhatárok. Ezen kívül eső becslést glitch-nek tekintünk.
pub const MIN_BPM: f64 = 20.0;
pub const MAX_BPM: f64 = 400.0;

fn clamp_bpm(bpm: f64) -> f64 {
    bpm.clamp(MIN_BPM, MAX_BPM)
}

/// A MIDI clock-ból tempót és fázist becslő PLL.
///
/// Időbélyegként monoton másodperceket vár (a hívó származtatja pl.
/// `Instant`-ból vagy CoreAudio host time-ból), így a logika tesztelhető.
#[derive(Debug, Clone)]
pub struct ClockPll {
    window: usize,
    points: VecDeque<(f64, f64)>,
    seconds_per_tick: f64,
    last_tick: Option<(f64, f64)>,
    last_error_ms: f64,
}

impl ClockPll {
    /// `window`: hány tickre illesztünk (48 = 2 negyed, 96 = 4 negyed).
    pub fn new(window: usize, initial_bpm: f64) -> Self {
        let window = window.max(8);
        Self {
            window,
            points: VecDeque::with_capacity(window + 1),
            seconds_per_tick: 60.0 / clamp_bpm(initial_bpm) / PPQN as f64,
            last_tick: None,
            last_error_ms: 0.0,
        }
    }

    /// Nem törli a tempót, csak a fázist igazítja (Start / Continue).
    pub fn anchor(&mut self, tick: f64, t: f64) {
        self.last_tick = Some((tick, t));
    }

    /// Teljes újraindítás (Stop vagy új dal): tempó vissza a kiindulóra.
    pub fn reset(&mut self, initial_bpm: f64) {
        self.points.clear();
        self.last_tick = None;
        self.last_error_ms = 0.0;
        self.seconds_per_tick = 60.0 / clamp_bpm(initial_bpm) / PPQN as f64;
    }

    /// Egy clock tick fogadása. `tick` a tick sorszáma (24/negyed),
    /// `t` a monoton idő másodpercben.
    pub fn on_tick(&mut self, tick: f64, t: f64) {
        if let Some((prev_tick, prev_t)) = self.last_tick {
            // Elfajuló vagy visszafelé ugró időt (pl. SPP) ne vegyünk figyelembe.
            if t <= prev_t || tick <= prev_tick {
                self.last_tick = Some((tick, t));
                self.points.clear();
                return;
            }
        }
        self.last_tick = Some((tick, t));
        self.points.push_back((tick, t));
        while self.points.len() > self.window {
            self.points.pop_front();
        }
        self.recompute();
    }

    fn recompute(&mut self) {
        if self.points.len() < 4 {
            return;
        }
        // Lineáris regresszió: y = a + b x, ahol x = tick, y = idő.
        let n = self.points.len() as f64;
        let mean_x = self.points.iter().map(|p| p.0).sum::<f64>() / n;
        let mean_y = self.points.iter().map(|p| p.1).sum::<f64>() / n;
        let mut sxx = 0.0;
        let mut sxy = 0.0;
        for (x, y) in &self.points {
            let dx = x - mean_x;
            sxx += dx * dx;
            sxy += dx * (y - mean_y);
        }
        if sxx <= f64::EPSILON {
            return;
        }
        let slope = sxy / sxx; // másodperc / tick
        if !slope.is_finite() || slope <= 0.0 {
            return;
        }
        let spb = clamp_bpm(60.0 / (slope * PPQN as f64)) * PPQN as f64;
        let new_spt = 60.0 / spb / PPQN as f64;
        // Enyhe simítás: a regresszió már simít, de egy kiugró tick ne rántson el.
        self.seconds_per_tick = self.seconds_per_tick * 0.7 + new_spt * 0.3;

        if let Some((tick, t)) = self.last_tick {
            let predicted = self.predicted_time(tick);
            self.last_error_ms = (t - predicted) * 1000.0;
        }
    }

    /// A jelenlegi tempó becslés.
    pub fn tempo_bpm(&self) -> f64 {
        clamp_bpm(60.0 / (self.seconds_per_tick * PPQN as f64))
    }

    /// Másodperc / negyed.
    pub fn seconds_per_beat(&self) -> f64 {
        self.seconds_per_tick * PPQN as f64
    }

    /// Be van-e már zárva a PLL (van elég tickünk).
    pub fn is_locked(&self) -> bool {
        self.points.len() >= 8.min(self.window)
    }

    /// Milyen tick-sorszámnál járnánk `t` időpontban a jelenlegi becslés szerint.
    pub fn predicted_tick(&self, t: f64) -> Option<f64> {
        let (tick, t0) = self.last_tick?;
        Some(tick + (t - t0) / self.seconds_per_tick)
    }

    /// Melyik tick-hez tartozik `t` időpont a jelenlegi becslés szerint.
    pub fn predicted_time(&self, tick: f64) -> f64 {
        match self.last_tick {
            Some((last_tick, t0)) => t0 + (tick - last_tick) * self.seconds_per_tick,
            None => 0.0,
        }
    }

    /// Zenei pozíció negyedekben `t` időpontban.
    pub fn beats_at(&self, t: f64) -> Option<f64> {
        self.predicted_tick(t).map(|tick| tick / PPQN as f64)
    }

    /// Az utolsó tick pozícióhibája ms-ban (előjeles).
    pub fn last_error_ms(&self) -> f64 {
        self.last_error_ms
    }
}

/// A transport idővonala: zenei pozíció (negyed) és tempó.
#[derive(Debug, Clone)]
pub struct Transport {
    playing: bool,
    bpm: f64,
    anchor_beats: f64,
    anchor_time: f64,
}

impl Transport {
    pub fn new(initial_bpm: f64) -> Self {
        Self {
            playing: false,
            bpm: clamp_bpm(initial_bpm),
            anchor_beats: 0.0,
            anchor_time: 0.0,
        }
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn bpm(&self) -> f64 {
        self.bpm
    }

    pub fn seconds_per_beat(&self) -> f64 {
        60.0 / self.bpm
    }

    pub fn beats_at(&self, t: f64) -> f64 {
        if !self.playing {
            return self.anchor_beats;
        }
        self.anchor_beats + (t - self.anchor_time) / self.seconds_per_beat()
    }

    pub fn play(&mut self, t: f64) {
        if !self.playing {
            self.anchor_time = t;
            self.playing = true;
        }
    }

    pub fn stop(&mut self, t: f64) {
        if self.playing {
            self.anchor_beats = self.beats_at(t);
            self.anchor_time = t;
            self.playing = false;
        }
    }

    /// Tempóváltás ugrás nélkül: a pozíciót az új tempóval horgonyozzuk újra.
    pub fn set_bpm(&mut self, bpm: f64, t: f64) {
        let beats = self.beats_at(t);
        self.bpm = clamp_bpm(bpm);
        self.anchor_beats = beats;
        self.anchor_time = t;
    }

    /// Ugrás egy zenei pozícióra (kézi beavatkozás, SPP).
    pub fn seek(&mut self, beats: f64, t: f64) {
        self.anchor_beats = beats;
        self.anchor_time = t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Szintetikus clock: adott BPM-mel, megadott jitterrel generál tickeket.
    fn feed(pll: &mut ClockPll, bpm: f64, ticks: u32, jitter_ms: f64) {
        let spt = 60.0 / bpm / PPQN as f64;
        // Determinisztikus, de nem triviális jitter (nem periodikus a mintán belül).
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        for i in 0..ticks {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let r = ((seed >> 33) as f64 / (1u64 << 31) as f64) - 1.0; // [-1, 1]
            let t = (i as f64 + 1.0) * spt + r * jitter_ms / 1000.0;
            pll.on_tick((i + 1) as f64, t);
        }
    }

    #[test]
    fn recovers_tempo_at_128_bpm_with_jitter() {
        let mut pll = ClockPll::new(96, 120.0);
        feed(&mut pll, 128.0, 24 * 8, 2.0);
        assert!(
            (pll.tempo_bpm() - 128.0).abs() < 0.5,
            "tempo = {}",
            pll.tempo_bpm()
        );
    }

    #[test]
    fn recovers_tempo_at_174_bpm_with_jitter() {
        let mut pll = ClockPll::new(96, 120.0);
        feed(&mut pll, 174.0, 24 * 8, 3.0);
        assert!(
            (pll.tempo_bpm() - 174.0).abs() < 1.0,
            "tempo = {}",
            pll.tempo_bpm()
        );
    }

    #[test]
    fn interpolates_phase_between_ticks() {
        let mut pll = ClockPll::new(48, 120.0);
        // 120 BPM: negyed = 0.5 s, tick = 1/48 s.
        for i in 0..=48 {
            pll.on_tick(i as f64, i as f64 / 48.0);
        }
        // A 48. tick 1.0 s-nál van -> pontosan 2 negyed.
        let beats = pll.beats_at(1.0).unwrap();
        assert!((beats - 2.0).abs() < 0.02, "beats = {beats}");
        // Fél tickkel később ~1/48 negyeddel több.
        let beats_half = pll.beats_at(1.0 + 1.0 / 96.0).unwrap();
        assert!((beats_half - beats - 1.0 / 48.0).abs() < 0.01);
    }

    #[test]
    fn rejects_backwards_time() {
        let mut pll = ClockPll::new(48, 120.0);
        for i in 0..20 {
            pll.on_tick(i as f64, i as f64 * 0.1);
        }
        let before = pll.tempo_bpm();
        pll.on_tick(5.0, 0.2); // visszaugrás
        assert!((pll.tempo_bpm() - before).abs() < 1e-9);
    }

    #[test]
    fn transport_advances_with_tempo() {
        let mut tr = Transport::new(120.0);
        tr.seek(0.0, 100.0);
        tr.play(100.0);
        assert!((tr.beats_at(101.0) - 2.0).abs() < 1e-9);
        tr.set_bpm(60.0, 101.0);
        assert!((tr.beats_at(102.0) - 3.0).abs() < 1e-9);
        tr.stop(102.0);
        assert!((tr.beats_at(200.0) - 3.0).abs() < 1e-9);
    }
}

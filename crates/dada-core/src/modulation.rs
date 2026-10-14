//! Modulációs mátrix: bármely forrás -> bármely cél.
//!
//! Források: MIDI note (envelope-pal), MIDI CC, audio-sávok, audio onset,
//! tempó-szinkron LFO, clock-fázis.
//! Célok: deck-opacity, deck-következő-klip (trigger), shader-paraméter,
//! audio-réteg opacity, master opacity.
//!
//! A modul GPU- és hardvermentes: a hívó tölti fel a pillanatnyi
//! forrásértékekkel, és alkalmazza a kimeneteket. Így determinisztikusan
//! tesztelhető.

use std::f32::consts::TAU;

/// Audio-sávok száma (a dada-audio sávjaival egyezik).
pub const AUDIO_BANDS: usize = 6;

/// Envelope-görbe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    Linear,
    Exponential,
}

/// Az envelope időzítése. A tipikus VJ-eset: attack 0, hold 0, decay 150 ms.
#[derive(Debug, Clone, Copy)]
pub struct EnvelopeSpec {
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub decay_ms: f32,
    pub curve: Curve,
    /// Ha igaz, egy új ütés újratölti a lecsengést közben is.
    pub retrigger: bool,
}

impl Default for EnvelopeSpec {
    fn default() -> Self {
        Self {
            attack_ms: 0.0,
            hold_ms: 0.0,
            decay_ms: 150.0,
            curve: Curve::Exponential,
            retrigger: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Attack,
    Hold,
    Decay,
}

/// Envelope-generátor: 0..1 értéket ad.
#[derive(Debug, Clone, Copy)]
pub struct Envelope {
    pub value: f32,
    phase: Phase,
    elapsed_ms: f32,
    pub triggers: u64,
}

impl Default for Envelope {
    fn default() -> Self {
        Self {
            value: 0.0,
            phase: Phase::Idle,
            elapsed_ms: 0.0,
            triggers: 0,
        }
    }
}

impl Envelope {
    pub fn trigger(&mut self, spec: &EnvelopeSpec) {
        if self.phase != Phase::Idle && !spec.retrigger {
            return;
        }
        self.phase = Phase::Attack;
        self.elapsed_ms = 0.0;
        self.triggers += 1;
        if spec.attack_ms <= 0.0 {
            self.value = 1.0;
            self.phase = if spec.hold_ms > 0.0 {
                Phase::Hold
            } else {
                Phase::Decay
            };
        }
    }

    pub fn release(&mut self) {
        if self.phase != Phase::Idle {
            self.phase = Phase::Decay;
        }
    }

    pub fn is_active(&self) -> bool {
        self.phase != Phase::Idle
    }

    pub fn advance(&mut self, dt_ms: f32, spec: &EnvelopeSpec) -> f32 {
        self.elapsed_ms += dt_ms;
        match self.phase {
            Phase::Idle => self.value = 0.0,
            Phase::Attack => {
                let a = spec.attack_ms.max(0.001);
                let t = (self.elapsed_ms / a).clamp(0.0, 1.0);
                self.value = t;
                if t >= 1.0 {
                    self.phase = Phase::Hold;
                    self.elapsed_ms = 0.0;
                }
            }
            Phase::Hold => {
                self.value = 1.0;
                if self.elapsed_ms >= spec.hold_ms {
                    self.phase = Phase::Decay;
                    self.elapsed_ms = 0.0;
                }
            }
            Phase::Decay => {
                let d = spec.decay_ms.max(1.0);
                let t = (self.elapsed_ms / d).clamp(0.0, 1.0);
                self.value = match spec.curve {
                    Curve::Linear => 1.0 - t,
                    Curve::Exponential => (1.0 - t) * (1.0 - t),
                };
                if t >= 1.0 {
                    self.phase = Phase::Idle;
                    self.value = 0.0;
                }
            }
        }
        self.value
    }
}

/// Honnan jön a moduláció.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModSource {
    Note { note: u8 },
    Cc { cc: u8 },
    AudioBand(usize),
    AudioRms,
    AudioOnset,
    /// Tempó-szinkron LFO, periódus ütemekben.
    Lfo { period_beats: f64 },
    /// A jelenlegi negyed tört része (0..1).
    ClockPhase,
}

/// Hova megy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModTarget {
    /// Hozzáadódik a deck alap-opacity-jához (0..amount).
    DeckOpacity(usize),
    /// Egyszeri trigger: a deck a következő klipre lép (kvantáltan).
    DeckNextClip(usize),
    ShaderParam(usize),
    AudioOpacity,
    MasterOpacity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModMode {
    /// A forrás eseménye triggereli az envelope-ot.
    Trigger,
    /// Folytonos érték (CC, audio-sáv, LFO).
    Continuous,
}

#[derive(Debug, Clone)]
pub struct ModRoute {
    pub source: ModSource,
    pub target: ModTarget,
    pub amount: f32,
    pub spec: EnvelopeSpec,
    pub mode: ModMode,
    pub env: Envelope,
    /// A pillanatnyi forrásérték (0..1).
    pub value: f32,
    pub hits: u64,
}

impl ModRoute {
    pub fn new(source: ModSource, target: ModTarget) -> Self {
        let mode = match source {
            ModSource::Note { .. } | ModSource::AudioOnset => ModMode::Trigger,
            _ => ModMode::Continuous,
        };
        Self {
            source,
            target,
            amount: 1.0,
            spec: EnvelopeSpec::default(),
            mode,
            env: Envelope::default(),
            value: 0.0,
            hits: 0,
        }
    }

    pub fn with_amount(mut self, amount: f32) -> Self {
        self.amount = amount;
        self
    }

    pub fn with_spec(mut self, spec: EnvelopeSpec) -> Self {
        self.spec = spec;
        self
    }

    /// A moduláció pillanatnyi hozzájárulása (0..amount).
    pub fn contribution(&self) -> f32 {
        self.value * self.amount
    }
}

/// A források pillanatnyi értékei.
#[derive(Debug, Clone)]
pub struct ModInputs {
    pub bands: [f32; AUDIO_BANDS],
    pub rms: f32,
    pub onset: bool,
    /// (note, velocity) ebben a frame-ben.
    pub notes: Vec<(u8, u8)>,
    pub note_offs: Vec<u8>,
    /// (cc, value 0..127) ebben a frame-ben.
    pub ccs: Vec<(u8, u8)>,
    pub dt_ms: f32,
    pub beat: f64,
    pub bpm: f64,
}

impl Default for ModInputs {
    fn default() -> Self {
        Self {
            bands: [0.0; AUDIO_BANDS],
            rms: 0.0,
            onset: false,
            notes: Vec::new(),
            note_offs: Vec::new(),
            ccs: Vec::new(),
            dt_ms: 16.7,
            beat: 0.0,
            bpm: 120.0,
        }
    }
}

/// A mátrix kimenete egy frame-re.
#[derive(Debug, Clone, Default)]
pub struct ModOutputs {
    pub continuous: Vec<(ModTarget, f32)>,
    pub triggers: Vec<ModTarget>,
}

impl ModOutputs {
    /// Egy cél folytonos értéke (0, ha nincs).
    pub fn value(&self, target: ModTarget) -> f32 {
        self.continuous
            .iter()
            .filter(|(t, _)| *t == target)
            .map(|(_, v)| *v)
            .fold(0.0f32, f32::max)
    }

    pub fn has_trigger(&self, target: ModTarget) -> bool {
        self.triggers.contains(&target)
    }
}

/// A modulációs mátrix.
#[derive(Debug, Clone, Default)]
pub struct ModMatrix {
    pub routes: Vec<ModRoute>,
}

impl ModMatrix {
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    pub fn add(&mut self, route: ModRoute) -> usize {
        self.routes.push(route);
        self.routes.len() - 1
    }

    pub fn uses_audio(&self) -> bool {
        self.routes.iter().any(|r| {
            matches!(
                r.source,
                ModSource::AudioBand(_) | ModSource::AudioRms | ModSource::AudioOnset
            )
        })
    }

    /// Egy frame léptetése és a kimenetek kiszámítása.
    pub fn tick(&mut self, inputs: &ModInputs) -> ModOutputs {
        let mut out = ModOutputs::default();
        for route in self.routes.iter_mut() {
            let before = route.env.triggers;
            let mut fired = false;

            match route.source {
                ModSource::Note { note } => {
                    let mut triggered = false;
                    if inputs.notes.iter().any(|(n, v)| *n == note && *v > 0) {
                        route.env.trigger(&route.spec);
                        route.hits += 1;
                        triggered = true;
                    }
                    if inputs.note_offs.contains(&note) {
                        route.env.release();
                    }
                    // Az ütés frame-jében a csúcsot adjuk ki (ne csengjen le azonnal).
                    route.value = if triggered {
                        route.env.value
                    } else {
                        route.env.advance(inputs.dt_ms, &route.spec)
                    };
                }
                ModSource::Cc { cc } => {
                    if let Some((_, v)) = inputs.ccs.iter().rev().find(|(c, _)| *c == cc) {
                        let target = *v as f32 / 127.0;
                        route.value += (target - route.value) * 0.35;
                    }
                }
                ModSource::AudioBand(i) => {
                    route.value = inputs.bands.get(i).copied().unwrap_or(0.0);
                }
                ModSource::AudioRms => {
                    route.value += (inputs.rms - route.value) * 0.4;
                }
                ModSource::AudioOnset => {
                    if inputs.onset {
                        route.env.trigger(&route.spec);
                        route.hits += 1;
                        route.value = route.env.value;
                    } else {
                        route.value = route.env.advance(inputs.dt_ms, &route.spec);
                    }
                }
                ModSource::Lfo { period_beats } => {
                    let phase = if period_beats > 0.0 {
                        (inputs.beat / period_beats).fract()
                    } else {
                        0.0
                    };
                    // 0..1 között billegő szinusz (ne negatív legyen).
                    route.value = ((phase * TAU as f64).sin() as f32) * 0.5 + 0.5;
                }
                ModSource::ClockPhase => {
                    route.value = inputs.beat.fract() as f32;
                }
            }

            fired = route.env.triggers > before;

            if fired && route.mode == ModMode::Trigger {
                out.triggers.push(route.target);
            }
            // A folytonos értéket mindig kiadjuk (a trigger-módú célok
            // figyelmen kívül hagyják), hogy a opacity-szerű célok működjenek.
            out.continuous.push((route.target, route.contribution()));
        }
        out
    }

    /// A deck-opacity modulációja (a bázis fölé adódik).
    pub fn deck_opacity_delta(out: &ModOutputs, deck: usize) -> f32 {
        out.value(ModTarget::DeckOpacity(deck))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> ModInputs {
        ModInputs {
            dt_ms: 100.0,
            beat: 4.0,
            ..Default::default()
        }
    }

    #[test]
    fn kick_bumps_opacity_then_decays() {
        let mut m = ModMatrix::new();
        m.add(
            ModRoute::new(ModSource::Note { note: 36 }, ModTarget::DeckOpacity(1)).with_spec(
                EnvelopeSpec {
                    attack_ms: 0.0,
                    hold_ms: 0.0,
                    decay_ms: 200.0,
                    curve: Curve::Linear,
                    retrigger: true,
                },
            ),
        );
        let mut inp = inputs();
        inp.notes = vec![(36, 100)];
        let out = m.tick(&inp);
        let v = ModOutputs::value(&out, ModTarget::DeckOpacity(1));
        assert!(v > 0.9, "v = {v}");

        let mut inp2 = inputs();
        inp2.notes = vec![];
        let out2 = m.tick(&inp2);
        let v2 = ModOutputs::value(&out2, ModTarget::DeckOpacity(1));
        assert!((v2 - 0.5).abs() < 0.05, "v2 = {v2}");

        let out3 = m.tick(&inputs());
        let v3 = ModOutputs::value(&out3, ModTarget::DeckOpacity(1));
        assert!(v3 < 0.01, "v3 = {v3}");
    }

    #[test]
    fn other_note_does_not_trigger() {
        let mut m = ModMatrix::new();
        m.add(ModRoute::new(
            ModSource::Note { note: 36 },
            ModTarget::DeckOpacity(0),
        ));
        let mut inp = inputs();
        inp.notes = vec![(38, 100)];
        let out = m.tick(&inp);
        assert_eq!(ModOutputs::value(&out, ModTarget::DeckOpacity(0)), 0.0);
    }

    #[test]
    fn cc_is_continuous_and_smoothed() {
        let mut m = ModMatrix::new();
        m.add(ModRoute::new(
            ModSource::Cc { cc: 1 },
            ModTarget::ShaderParam(0),
        ));
        let mut inp = inputs();
        inp.ccs = vec![(1, 127)];
        let a = ModOutputs::value(&m.tick(&inp), ModTarget::ShaderParam(0));
        let b = ModOutputs::value(&m.tick(&inp), ModTarget::ShaderParam(0));
        assert!(a > 0.0 && a < 1.0, "a = {a}");
        assert!(b > a, "a={a} b={b}");
        assert!(b < 1.0);
    }

    #[test]
    fn audio_band_drives_target() {
        let mut m = ModMatrix::new();
        m.add(ModRoute::new(
            ModSource::AudioBand(1),
            ModTarget::AudioOpacity,
        ));
        let mut inp = inputs();
        inp.bands[1] = 0.8;
        let out = m.tick(&inp);
        assert!((ModOutputs::value(&out, ModTarget::AudioOpacity) - 0.8).abs() < 1e-6);
        assert!(m.uses_audio());
    }

    #[test]
    fn onset_triggers_next_clip() {
        let mut m = ModMatrix::new();
        m.add(ModRoute::new(
            ModSource::AudioOnset,
            ModTarget::DeckNextClip(0),
        ));
        let mut inp = inputs();
        inp.onset = true;
        let out = m.tick(&inp);
        assert!(out.has_trigger(ModTarget::DeckNextClip(0)));
        let out2 = m.tick(&inputs());
        assert!(!out2.has_trigger(ModTarget::DeckNextClip(0)));
    }

    #[test]
    fn lfo_is_tempo_synced() {
        let mut m = ModMatrix::new();
        m.add(ModRoute::new(
            ModSource::Lfo {
                period_beats: 4.0,
            },
            ModTarget::MasterOpacity,
        ));
        let mut vals = Vec::new();
        for b in 0..8 {
            let mut inp = inputs();
            inp.beat = b as f64;
            vals.push(ModOutputs::value(&m.tick(&inp), ModTarget::MasterOpacity));
        }
        // 4 ütemes periódus: 0.5 -> 1.0 -> 0.5 -> 0.0 (uni-poláris szinusz).
        assert!((vals[0] - 0.5).abs() < 0.01, "{vals:?}");
        assert!((vals[1] - 1.0).abs() < 0.01, "{vals:?}");
        assert!((vals[2] - 0.5).abs() < 0.01, "{vals:?}");
        assert!(vals[3] < 0.01, "{vals:?}");
        assert!((vals[4] - vals[0]).abs() < 0.05);
    }

    #[test]
    fn amount_scales_contribution() {
        let mut m = ModMatrix::new();
        m.add(
            ModRoute::new(ModSource::AudioRms, ModTarget::MasterOpacity)
                .with_amount(0.25),
        );
        let mut inp = inputs();
        inp.rms = 1.0;
        let out = m.tick(&inp);
        let v = ModOutputs::value(&out, ModTarget::MasterOpacity);
        assert!(v <= 0.25 + 1e-6 && v > 0.0, "v = {v}");
    }
}

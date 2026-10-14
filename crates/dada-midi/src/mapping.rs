//! Modulációs források: envelope-generátor és mapping.

pub use dada_core::modulation::{Curve, Envelope, EnvelopeSpec};

/// Mi váltja ki a modulációt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Note { note: u8, channel: Option<u8> },
    AnyNote,
    Cc { cc: u8, threshold: u8 },
    /// Az audio-elemzés onsetje (ugyanaz a mechanizmus, más forrásból).
    AudioOnset,
}

/// Egy forrás -> cél kapcsolat.
#[derive(Debug, Clone)]
pub struct Mapping {
    /// Melyik MIDI-portról (0 = az első). Audio-forrásnál figyelmen kívül.
    pub source: usize,
    pub trigger: Trigger,
    /// A cél paraméter neve (pl. "shader.param.0" vagy "layer.fx.invert").
    pub target: String,
    /// Mennyi a hozzájárulás (0..1).
    pub amount: f32,
    pub spec: EnvelopeSpec,
    pub env: Envelope,
    pub hits: u64,
}

impl Mapping {
    pub fn new(source: usize, trigger: Trigger, target: impl Into<String>, amount: f32) -> Self {
        Self {
            source,
            trigger,
            target: target.into(),
            amount,
            spec: EnvelopeSpec::default(),
            env: Envelope::default(),
            hits: 0,
        }
    }

    pub fn with_spec(mut self, spec: EnvelopeSpec) -> Self {
        self.spec = spec;
        self
    }

    /// Az aktuális modulációs érték (0..amount).
    pub fn value(&self) -> f32 {
        self.env.value * self.amount
    }

    /// Illeszkedik-e az üzenet erre a mappingre.
    pub fn matches(&self, source: usize, msg: &crate::message::MidiMessage) -> bool {
        use crate::message::MidiMessage::*;
        if self.source != source {
            return false;
        }
        match (self.trigger, msg) {
            (Trigger::Note { note, channel }, NoteOn { note: n, channel: ch, velocity }) => {
                *n == note && *velocity > 0 && channel.map(|c| c == *ch).unwrap_or(true)
            }
            (Trigger::Note { note, .. }, NoteOff { note: n, .. }) => *n == note,
            (Trigger::AnyNote, NoteOn { velocity, .. }) => *velocity > 0,
            (Trigger::AnyNote, NoteOff { .. }) => true,
            (Trigger::Cc { cc, threshold }, ControlChange { cc: c, value, .. }) => {
                *c == cc && *value >= threshold
            }
            _ => false,
        }
    }

    /// Üzenet feldolgozása (trigger vagy release).
    pub fn process(&mut self, source: usize, msg: &crate::message::MidiMessage) {
        use crate::message::MidiMessage::*;
        if !self.matches(source, msg) {
            return;
        }
        match msg {
            NoteOn { .. } | ControlChange { .. } => {
                self.hits += 1;
                self.env.trigger(&self.spec);
            }
            NoteOff { .. } => self.env.release(),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instant_attack_then_decay() {
        let spec = EnvelopeSpec {
            attack_ms: 0.0,
            hold_ms: 0.0,
            decay_ms: 100.0,
            curve: Curve::Linear,
            retrigger: true,
        };
        let mut env = Envelope::default();
        env.trigger(&spec);
        assert!((env.value - 1.0).abs() < 1e-6);
        env.advance(50.0, &spec);
        assert!((env.value - 0.5).abs() < 0.02, "{}", env.value);
        env.advance(60.0, &spec);
        assert_eq!(env.value, 0.0);
        assert!(!env.is_active());
    }

    #[test]
    fn exponential_decay_is_faster_at_start() {
        let spec = EnvelopeSpec {
            attack_ms: 0.0,
            hold_ms: 0.0,
            decay_ms: 100.0,
            curve: Curve::Exponential,
            retrigger: true,
        };
        let mut env = Envelope::default();
        env.trigger(&spec);
        env.advance(50.0, &spec);
        // (1-0.5)^2 = 0.25
        assert!((env.value - 0.25).abs() < 0.02, "{}", env.value);
    }

    #[test]
    fn attack_ramps() {
        let spec = EnvelopeSpec {
            attack_ms: 100.0,
            hold_ms: 0.0,
            decay_ms: 100.0,
            curve: Curve::Linear,
            retrigger: true,
        };
        let mut env = Envelope::default();
        env.trigger(&spec);
        env.advance(30.0, &spec);
        assert!((env.value - 0.3).abs() < 0.02, "{}", env.value);
    }

    #[test]
    fn note_mapping_triggers_only_on_its_note() {
        use crate::message::MidiMessage;
        let mut m = Mapping::new(0, Trigger::Note { note: 36, channel: None }, "invert", 1.0);
        m.process(0, &MidiMessage::NoteOn { channel: 0, note: 38, velocity: 100 });
        assert_eq!(m.hits, 0);
        m.process(0, &MidiMessage::NoteOn { channel: 0, note: 36, velocity: 100 });
        assert_eq!(m.hits, 1);
        assert!(m.value() > 0.9);
        // Másik portról nem szabad triggerelni.
        m.process(1, &MidiMessage::NoteOn { channel: 0, note: 36, velocity: 100 });
        assert_eq!(m.hits, 1);
    }

    #[test]
    fn cc_threshold_mapping() {
        use crate::message::MidiMessage;
        let mut m = Mapping::new(0, Trigger::Cc { cc: 1, threshold: 64 }, "hue", 0.5);
        m.process(0, &MidiMessage::ControlChange { channel: 0, cc: 1, value: 10 });
        assert_eq!(m.hits, 0);
        m.process(0, &MidiMessage::ControlChange { channel: 0, cc: 1, value: 80 });
        assert_eq!(m.hits, 1);
        assert!((m.value() - 0.5).abs() < 1e-6);
    }

    #[test]
    fn retrigger_false_ignores_hits_while_active() {
        use crate::message::MidiMessage;
        let spec = EnvelopeSpec {
            decay_ms: 1000.0,
            retrigger: false,
            ..Default::default()
        };
        let mut m = Mapping::new(0, Trigger::AnyNote, "x", 1.0).with_spec(spec);
        m.process(0, &MidiMessage::NoteOn { channel: 0, note: 1, velocity: 100 });
        m.process(0, &MidiMessage::NoteOn { channel: 0, note: 1, velocity: 100 });
        assert_eq!(m.hits, 2); // a hit számol, de az envelope nem tölt újra
        assert_eq!(m.env.triggers, 1);
    }
}

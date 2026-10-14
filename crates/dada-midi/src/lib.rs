//! Dada VJ — MIDI: több forrás, clock-dekód, note/CC, envelope-vezérelt moduláció.

pub mod engine;
pub mod mapping;
pub mod message;

pub use engine::{synthetic_clock, Event, MidiEngine, MidiPortInfo};
pub use mapping::{Curve, Envelope, EnvelopeSpec, Mapping, Trigger};
pub use message::{MidiMessage, MidiParser};

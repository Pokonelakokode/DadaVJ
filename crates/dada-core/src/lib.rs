//! Dada VJ — mag: zenei idő, transport, MIDI clock helyreállítás.
//!
//! Ez a crate szándékosan nem függ semmilyen platform-specifikus dologtól,
//! így gyorsan és determinisztikusan tesztelhető.

pub mod clock;
pub mod deck;
pub mod layer;
pub mod modulation;
pub mod quantize;

pub use clock::{ClockPll, Transport, PPQN};
pub use deck::{
    Autopilot, AutopilotMode, BlendMode, ChromaMode, Clip, ClipSource, Deck, DeckEvent, LoopMode,
    ManualNextMode, Mixer, Queued,
};
pub use layer::LayerOrder;
pub use modulation::{
    ChannelModInput, Curve, Envelope, EnvelopeSpec, ModInputs, ModMatrix, ModOutputs, ModRoute,
    ModSource, ModTarget,
};
pub use quantize::{grid_floor, next_grid, next_grid_strict, Quantize};

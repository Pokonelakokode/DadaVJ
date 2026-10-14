//! Dada VJ — mag: zenei idő, transport, MIDI clock helyreállítás.
//!
//! Ez a crate szándékosan nem függ semmilyen platform-specifikus dologtól,
//! így gyorsan és determinisztikusan tesztelhető.

pub mod clock;
pub mod quantize;

pub use clock::{ClockPll, Transport, PPQN};
pub use quantize::{next_grid, next_grid_strict, Quantize};

//! Dada VJ — shader effekt- és generátor-motor (ISF-kompatibilis).

pub mod effect;
pub mod glsl_probe;
pub mod isf;

pub use effect::Effect;
pub use isf::{IsfShader, Metadata, Param, ParamKind};

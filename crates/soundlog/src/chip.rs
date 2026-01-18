//! Chip utilities and specifications used by VGM parsing and serialization.
//!
//! This module re-exports chip specification types and provides helpers
//! such as frequency-number conversions in the `fnumber` submodule.
pub mod fnumber;
mod spec;

pub use spec::*;

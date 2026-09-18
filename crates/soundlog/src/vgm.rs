//! VGM utilities and document handling used by this crate.
//!
//! This module exposes the VGM document and header types and re-exports
//! submodules for command parsing/serialization and the GD3/extra-header
//! handling utilities.

/// VGM sample rate used for wait-sample timing and header defaults.
pub const VGM_SAMPLE_RATE: u32 = 44_100;

pub mod callback_stream;
pub mod command;
pub mod detail;
mod document;
pub mod header;
pub mod parser;
pub mod stream;

pub use callback_stream::{VgmCallbackStream, WriteCallbackTarget};
pub use document::{VgmBuilder, VgmDocument};
pub use header::{VgmExtraHeader, VgmHeader, VgmHeaderField};
pub use stream::{VgmCommandGenerator, VgmStream};

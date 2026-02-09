//! VGM utilities and document handling used by this crate.
//!
//! This module exposes the VGM document and header types and re-exports
//! submodules for command parsing/serialization and the GD3/extra-header
//! handling utilities.
pub mod command;
pub mod detail;
mod document;
mod header;
pub mod parser;
pub mod stream;

pub use document::{VgmBuilder, VgmDocument};
pub use header::{VgmExtraHeader, VgmHeader, VgmHeaderField};
pub use stream::VgmStream;

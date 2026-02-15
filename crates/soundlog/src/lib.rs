#![doc = include_str!("../README.md")]
mod binutil;
pub mod chip;
pub mod meta;
pub mod vgm;

pub use binutil::ParseError;
pub use vgm::command::*;
pub use vgm::{VgmBuilder, VgmCallbackStream, VgmDocument, VgmExtraHeader, VgmHeader, VgmStream};

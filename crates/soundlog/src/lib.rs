#![doc = include_str!("../README.md")]
mod binutil;
pub mod chip;
#[cfg(feature = "mdx")]
pub mod mdx;
pub mod meta;
pub mod vgm;

pub use binutil::ParseError;
pub use vgm::command::*;
pub use vgm::stream::StreamResult as VgmStreamResult;
pub use vgm::{
    VgmBuilder, VgmCallbackStream, VgmCommandGenerator, VgmDocument, VgmExtraHeader, VgmHeader,
    VgmStream,
};

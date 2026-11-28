#![doc = include_str!("../README.md")]
mod binutil;
pub mod chip;
mod error;
pub mod execution;
#[cfg(feature = "mdx")]
pub mod mdx;
pub mod meta;
pub mod vgm;
#[cfg(feature = "xgm")]
pub mod xgm;

pub use error::ParseError;
pub use execution::ExecutionEvent;
pub use vgm::command::*;
pub use vgm::stream::StreamResult as VgmStreamResult;
pub use vgm::{
    VgmBuilder, VgmCallbackStream, VgmCommandGenerator, VgmDocument, VgmExtraHeader, VgmHeader,
    VgmParseLimits, VgmSeekOutcome, VgmStream,
};

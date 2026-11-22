//! XGM v1 documents, typed commands, and lazy VGM playback.
//!
//! XGM2 is a different format and is not supported by this module.

pub mod command;
pub mod convert;
pub mod document;

pub use command::{XgmCommand, XgmRegisterWrite};
pub use convert::{XgmPlaybackOptions, XgmVgmGenerator};
pub use document::{XgmBuilder, XgmDocument, XgmSample, XgmTiming, XgmTrack};

//! XGM v1 documents, typed commands, and lazy VGM playback.
//!
//! XGM2 is a different format and is not supported by this module.

pub mod command;
pub mod convert;
pub mod document;
pub mod header;

pub use command::{XgmCommand, XgmRegisterWrite};
pub use convert::{XgmLoopMarker, XgmPlaybackError, XgmPlaybackOptions, XgmVgmGenerator};
pub use document::{XgmBuilder, XgmDocument, XgmParseLimits, XgmTrack};
pub use header::{XGM_SAMPLE_COUNT, XgmHeader, XgmSample, XgmTiming};

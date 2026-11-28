//! S98 v1–v3 documents, typed commands, and lazy VGM playback.
//!
//! The format is based on the [S98 v3 specification][spec]. Compressed files
//! are not supported. Metadata is retained as bytes, without assuming an
//! encoding. Serialization preserves its contents, not original file offsets
//! or padding.
//!
//! [spec]: https://github.com/rururutan/s98spec3/blob/master/s98spec3-en.md

pub mod convert;
pub mod document;

pub use convert::{S98LoopMarker, S98PlaybackError, S98PlaybackOptions, S98VgmGenerator};
pub use document::{
    S98Builder, S98Command, S98Device, S98DeviceType, S98Document, S98Header, S98ParseLimits,
    S98Port, S98Version,
};

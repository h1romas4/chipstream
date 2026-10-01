//! Parse MXDRV MML source and compile it into a typed MDX document.
//!
//! Use [`parse`] to turn source text into an [`MmlDocument`], inspect or format
//! that syntax tree with [`format_tree`], then pass it to
//! [`compile::compile`] to produce a [`soundlog`] MDX document. Parsing and
//! compilation are separate so callers can validate or inspect the parsed
//! commands before generating output.

pub mod compile;
#[cfg(feature = "source-map")]
pub mod frontend;

mod mml;

pub use compile::{CompileError, compile, locate_compile_error, locate_source_command};
pub use mml::{
    Accidental, MmlCommand, MmlDocument, MmlLength, MmlTrack, MmlVoice, ParseError, SourcePosition,
    format_tree, parse,
};

//! Parse MXDRV MML source and compile it into a typed MDX document.
//!
//! Use [`parse`] to turn source text into an [`MmlDocument`], inspect or format
//! that syntax tree with [`format_tree`], then pass it to
//! [`compile::compile`] to produce a [`soundlog`] MDX document. Parsing and
//! compilation are separate so callers can validate or inspect the parsed
//! commands before generating output.

pub mod compat;
pub mod compile;

mod mml;

pub use compile::{CompileError, compile};
pub use mml::{
    Accidental, MmlCommand, MmlDocument, MmlLength, MmlTrack, MmlVoice, ParseError, format_tree,
    parse,
};

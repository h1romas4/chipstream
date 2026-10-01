//! Opt-in source ownership and compilation artifacts, independent of MML dialect.
//!
//! Available with the `source-map` feature; the default feature set is empty.
//! Dialect-specific entry points, such as [`crate::mdx::frontend`], use these
//! types without introducing a shared MML AST, filesystem access, or GUI dependencies.
//!
//! # Ownership
//!
//! [`SourceFile`] borrows an immutable source snapshot without copying its text.
//! [`MappedAst`] keeps a dialect's AST and syntax ranges read-only together.
//! [`Compiled`] keeps the generated document and its [`SourceMap`] together;
//! the AST and syntax ranges may be dropped after compilation. The source owner
//! must outlive both artifacts. Maps are never embedded in soundlog's documents.
//!
//! Use [`MappedAst::into_ast`] or [`Compiled::into_document`] to discard the map
//! before editing. [`Compiled::into_parts`] retains the output map, but subsequent
//! document edits can invalidate its coordinates.
//!
//! # Memory And Coordinates
//!
//! Ordinary parse/compile APIs do not collect maps or line indexes, even when
//! this feature is enabled. Mapped APIs allocate separate syntax and output maps;
//! their ranges use compact [`crate::source::Span`] UTF-8 byte coordinates.
//! [`SourceFile::position`] scans without allocation. Construct a [`LineIndex`]
//! explicitly for repeated lookups or zero-based LSP UTF-16 coordinates.
//!
//! [`crate::diagnostic::Diagnostic`] holds a structured error without copying
//! the offending source line. Source excerpts and display formatting belong to callers.
//! Feature separation does not imply `no_std` support or guarantee a particular
//! embedded target's binary size or peak memory consumption.

mod artifact;
mod map;
mod source;

pub use artifact::{Compiled, MappedAst};
pub use map::SourceMap;
pub use source::{LineIndex, SourceFile, SourcePosition};

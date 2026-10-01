//! MDX-specific syntax locations, stored separately from the ordinary AST.

use super::super::mml::MmlSourceMap;
use crate::source::Span;

pub use super::super::mml::CommandSource;

/// Source ranges of a voice declaration and its numeric parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSource {
    /// The complete voice declaration.
    pub span: Span,
    /// The voice number, excluding the `@` prefix.
    pub number: Span,
    /// Parameter ranges in the declaration's source order.
    pub parameters: Vec<Span>,
}

/// A numeric argument associated with its enclosing command range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArgumentSource {
    /// The enclosing command, or closing delimiter and count for a repeat.
    pub command: Span,
    /// The numeric argument's UTF-8 byte range.
    pub span: Span,
}

/// MDX syntax ranges stored separately from the ordinary MML AST.
///
/// Tracks and voices follow AST order, while numeric arguments follow source
/// order. Command entries retain nested repeat bodies and both delimiters.
#[derive(Debug)]
pub struct SyntaxMap {
    inner: MmlSourceMap,
}

impl SyntaxMap {
    pub(super) fn new(inner: MmlSourceMap) -> Self {
        Self { inner }
    }

    pub(super) fn inner(&self) -> &MmlSourceMap {
        &self.inner
    }

    /// Commands in AST track order, including nested repeats.
    ///
    /// `track` is an AST track index, not an MDX channel index. Returns `None`
    /// for an absent track; repeated channel lines are merged as in the AST.
    pub fn commands(&self, track: usize) -> Option<&[CommandSource]> {
        self.inner.get(track).map(Vec::as_slice)
    }

    /// Return the last title directive's range, or `None` when absent.
    pub fn title(&self) -> Option<Span> {
        self.inner.title
    }

    /// Return the last PDX filename directive's range, or `None` when absent.
    pub fn pcm_file(&self) -> Option<Span> {
        self.inner.pcm_file
    }

    /// Borrow voice declaration ranges in AST and output tone-bank order.
    pub fn voices(&self) -> &[VoiceSource] {
        &self.inner.voices
    }

    /// Numeric arguments in source order, associated with their command range.
    pub fn arguments(&self) -> &[ArgumentSource] {
        &self.inner.arguments
    }
}

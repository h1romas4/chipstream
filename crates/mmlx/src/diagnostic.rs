//! Source-independent diagnostics. Rendering source excerpts belongs to callers.

use std::fmt;

use crate::source::Span;

/// The significance of a source diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A failure that prevents the requested operation from succeeding.
    Error,
    /// A non-fatal issue that callers may display alongside successful output.
    Warning,
}

/// A diagnostic without a copy of the input source or its lines.
///
/// [`std::fmt::Display`] renders only the message. Callers own source snapshots
/// and decide how to display excerpts, line numbers, and highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// A machine-readable identifier, such as `mmlx.invalid-value`.
    pub code: &'static str,
    /// Whether this diagnostic is an error or a warning.
    pub severity: Severity,
    /// A human-readable explanation without a copied source excerpt.
    pub message: String,
    /// The primary UTF-8 source range, or `None` for unlocated failures.
    pub span: Option<Span>,
}

impl Diagnostic {
    /// Construct an error-severity diagnostic with an optional source range.
    pub fn error(code: &'static str, message: impl Into<String>, span: Option<Span>) -> Self {
        Self {
            code,
            severity: Severity::Error,
            message: message.into(),
            span,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Diagnostic {}

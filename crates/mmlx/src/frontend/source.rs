//! Borrowed source snapshots and optional indexes.

use crate::source::Span;

/// One-based coordinates; columns count Unicode scalar values, not UTF-16 units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    /// One-based source line, separated by LF or CRLF line endings.
    pub line_number: usize,
    /// One-based Unicode scalar-value column; not a byte offset or display width.
    pub column: usize,
}

/// Borrows source text without copying it or constructing a line index.
#[derive(Debug, Clone, Copy)]
pub struct SourceFile<'source> {
    text: &'source str,
}

impl<'source> SourceFile<'source> {
    /// Borrow a source snapshot, returning `None` for inputs over `u32::MAX` bytes.
    ///
    /// Does not allocate, copy text, or construct a line index.
    pub fn new(text: &'source str) -> Option<Self> {
        (text.len() <= u32::MAX as usize).then_some(Self { text })
    }

    /// Return the original borrowed UTF-8 source text.
    pub fn text(self) -> &'source str {
        self.text
    }

    /// Resolve an offset by scanning, without allocating an index.
    ///
    /// Returns `None` for offsets beyond the source or inside a UTF-8 character.
    /// The end-of-input offset is valid. Columns count Unicode scalar values.
    pub fn position(self, offset: usize) -> Option<SourcePosition> {
        let prefix = self.text.get(..offset)?;
        let line_number = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = prefix.rfind('\n').map_or(0, |index| index + 1);
        Some(SourcePosition {
            line_number,
            column: prefix[line_start..].chars().count() + 1,
        })
    }

    /// Borrow a range, returning `None` for invalid bounds or UTF-8 boundaries.
    pub fn slice(self, span: Span) -> Option<&'source str> {
        self.text.get(span.range())
    }

    /// Build an index only when repeated position lookups are needed.
    ///
    /// Allocates a vector of 32-bit line starts while continuing to borrow the
    /// same source snapshot. Newlines are identified by LF, including CRLF.
    pub fn line_index(self) -> LineIndex<'source> {
        let mut starts = vec![0];
        starts.extend(
            self.text
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some((index + 1) as u32)),
        );
        LineIndex {
            source: self,
            starts,
        }
    }
}

/// An index tied to the exact borrowed source snapshot that created it.
#[derive(Debug)]
pub struct LineIndex<'source> {
    source: SourceFile<'source>,
    starts: Vec<u32>,
}

impl LineIndex<'_> {
    /// Resolve a byte offset to one-based line and Unicode scalar-value column.
    ///
    /// Uses binary search over line starts, then scans the current line.
    /// Returns `None` outside the source or inside a UTF-8 character; EOF is valid.
    pub fn position(&self, offset: usize) -> Option<SourcePosition> {
        let prefix = self.source.text.get(..offset)?;
        let line = self
            .starts
            .partition_point(|start| *start as usize <= offset)
            - 1;
        Some(SourcePosition {
            line_number: line + 1,
            column: prefix[self.starts[line] as usize..].chars().count() + 1,
        })
    }

    /// Return zero-based LSP line and UTF-16 column coordinates.
    ///
    /// The input is a UTF-8 byte offset. Returns `None` outside the source or
    /// inside a UTF-8 character; the end-of-input offset is valid.
    pub fn utf16_position(&self, offset: usize) -> Option<(usize, usize)> {
        let prefix = self.source.text.get(..offset)?;
        let line = self
            .starts
            .partition_point(|start| *start as usize <= offset)
            - 1;
        Some((
            line,
            prefix[self.starts[line] as usize..].encode_utf16().count(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{SourceFile, SourcePosition};

    #[test]
    fn borrowed_text_and_optional_index_agree_on_unicode_crlf_and_eof() {
        let text = "a\u{65e5}\u{1f3b5}\r\nb\n";
        let source = SourceFile::new(text).unwrap();
        assert_eq!(source.text().as_ptr(), text.as_ptr());
        let index = source.line_index();
        for offset in 0..=text.len() {
            assert_eq!(source.position(offset), index.position(offset));
        }
        assert_eq!(
            source.position(8),
            Some(SourcePosition {
                line_number: 1,
                column: 4
            })
        );
        assert_eq!(index.utf16_position(8), Some((0, 4)));
        assert_eq!(index.utf16_position(10), Some((1, 0)));
        assert_eq!(index.utf16_position(text.len()), Some((2, 0)));
        assert_eq!(index.utf16_position(2), None);
        assert_eq!(source.position(text.len() + 1), None);
    }

    #[test]
    fn empty_input_has_one_line() {
        let source = SourceFile::new("").unwrap();
        assert_eq!(
            source.position(0),
            Some(SourcePosition {
                line_number: 1,
                column: 1
            })
        );
        assert_eq!(source.line_index().utf16_position(0), Some((0, 0)));
    }
}

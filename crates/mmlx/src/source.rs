//! Compact UTF-8 byte coordinates shared by MML frontends.

/// A half-open byte range. Sources larger than `u32::MAX` are not supported.
///
/// Stores two 32-bit offsets in eight bytes. Offsets address UTF-8 bytes, not
/// character columns. Construction checks ordering and representability;
/// slicing checks source bounds and UTF-8 boundaries separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    start: u32,
    end: u32,
}

impl Span {
    /// Construct a range, rejecting reversed or unrepresentable offsets.
    ///
    /// Empty ranges are valid. This does not check against a particular source.
    pub fn new(start: usize, end: usize) -> Option<Self> {
        (start <= end).then_some(Self {
            start: u32::try_from(start).ok()?,
            end: u32::try_from(end).ok()?,
        })
    }

    /// Return the inclusive starting byte offset.
    pub const fn start(self) -> usize {
        self.start as usize
    }

    /// Return the exclusive ending byte offset.
    pub const fn end(self) -> usize {
        self.end as usize
    }

    /// Convert the span to a half-open range suitable for checked string slicing.
    pub fn range(self) -> std::ops::Range<usize> {
        self.start()..self.end()
    }

    /// Slice a source, checking its length and UTF-8 boundaries.
    ///
    /// Returns `None` for out-of-bounds ranges or offsets inside UTF-8 characters.
    pub fn text(self, source: &str) -> Option<&str> {
        source.get(self.range())
    }
}

#[cfg(test)]
mod tests {
    use super::Span;

    #[test]
    fn compact_half_open_ranges_validate_boundaries() {
        assert_eq!(std::mem::size_of::<Span>(), 8);
        assert_eq!(Span::new(1, 3).unwrap().text("abcd"), Some("bc"));
        assert_eq!(Span::new(4, 4).unwrap().text("abcd"), Some(""));
        assert_eq!(Span::new(3, 1), None);
        assert_eq!(Span::new(0, 5).unwrap().text("abcd"), None);
        if usize::BITS > 32 {
            assert_eq!(Span::new(0, u32::MAX as usize + 1), None);
        }
    }

    #[test]
    fn ranges_are_utf8_bytes_not_character_columns() {
        let source = "a\u{65e5}b";
        assert_eq!(Span::new(1, 4).unwrap().text(source), Some("\u{65e5}"));
        assert_eq!(Span::new(1, 2).unwrap().text(source), None);
    }
}

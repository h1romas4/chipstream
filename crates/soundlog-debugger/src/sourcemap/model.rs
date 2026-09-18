//! Format-independent source structures shared by parsers and the GUI.

/// A half-open byte range: `[start, start + len)`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ByteRange {
    pub start: usize,
    pub len: usize,
}

impl ByteRange {
    pub const fn new(start: usize, len: usize) -> Self {
        Self { start, len }
    }

    pub fn end(self) -> usize {
        self.start.saturating_add(self.len)
    }

    pub fn contains(self, offset: usize) -> bool {
        offset >= self.start && offset < self.end()
    }
}

/// Identifies which byte representation a mapped range belongs to.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ByteCoordinateSpace {
    Original,
    Display,
    Logical,
}

/// A range together with the byte representation it addresses.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MappedRange {
    pub space: ByteCoordinateSpace,
    pub range: ByteRange,
}

impl MappedRange {
    pub const fn original(start: usize, len: usize) -> Self {
        Self {
            space: ByteCoordinateSpace::Original,
            range: ByteRange::new(start, len),
        }
    }

    /// Returns the range usable by a viewer over original or display bytes.
    /// Logical ranges need a separate coordinate transform before they can be
    /// applied to a raw byte viewer.
    pub const fn hex_range(self) -> Option<ByteRange> {
        match self.space {
            ByteCoordinateSpace::Original | ByteCoordinateSpace::Display => Some(self.range),
            ByteCoordinateSpace::Logical => None,
        }
    }
}

/// Stable identifier for a source-tree node.
pub type NodeId = u64;

/// A format-independent node shown in the source tree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceNode {
    pub id: NodeId,
    pub label: String,
    pub detail: String,
    pub copy_text: Option<String>,
    pub range: Option<MappedRange>,
    pub children: Vec<SourceNode>,
}

impl SourceNode {
    pub fn new(id: NodeId, label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            detail: detail.into(),
            copy_text: None,
            range: None,
            children: Vec::new(),
        }
    }

    pub fn with_range(mut self, range: MappedRange) -> Self {
        self.range = Some(range);
        self
    }

    pub fn with_copy_text(mut self, text: impl Into<String>) -> Self {
        self.copy_text = Some(text.into());
        self
    }

    pub fn with_children(mut self, children: Vec<SourceNode>) -> Self {
        self.children = children;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::{ByteCoordinateSpace, ByteRange, MappedRange, SourceNode};

    #[test]
    fn byte_range_uses_half_open_boundaries() {
        let range = ByteRange::new(4, 3);

        assert_eq!(range.end(), 7);
        assert!(!range.contains(3));
        assert!(range.contains(4));
        assert!(range.contains(6));
        assert!(!range.contains(7));
    }

    #[test]
    fn source_node_keeps_mapped_range_and_children() {
        let child = SourceNode::new(2, "field", "value");
        let node = SourceNode::new(1, "header", "Header")
            .with_range(MappedRange::original(0, 16))
            .with_children(vec![child]);

        assert_eq!(node.range.unwrap().space, ByteCoordinateSpace::Original);
        assert_eq!(node.range.unwrap().range, ByteRange::new(0, 16));
        assert_eq!(node.children[0].id, 2);
    }

    #[test]
    fn logical_ranges_require_a_coordinate_transform_for_hex_view() {
        let logical = MappedRange {
            space: ByteCoordinateSpace::Logical,
            range: ByteRange::new(4, 2),
        };
        let display = MappedRange {
            space: ByteCoordinateSpace::Display,
            range: ByteRange::new(8, 3),
        };

        assert_eq!(logical.hex_range(), None);
        assert_eq!(display.hex_range(), Some(ByteRange::new(8, 3)));
    }
}

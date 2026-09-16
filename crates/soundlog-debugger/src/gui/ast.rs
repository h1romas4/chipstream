use crate::sourcemap::{ByteRange, MappedRange, RawBinaryAdapter, SourceNode};

/// AST node representation used by the debugger tree.
#[derive(Clone, Debug)]
pub struct AstNode {
    pub title: String,
    pub detail: String,
    pub children: Vec<AstNode>,
    pub lazy_count: Option<usize>,
    pub lazy_start: Option<usize>,
    pub lazy_track: Option<usize>,
    pub byte_range: Option<ByteRange>,
    pub mapped_range: Option<MappedRange>,
}

impl AstNode {
    pub fn new(title: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            detail: detail.into(),
            children: Vec::new(),
            lazy_count: None,
            lazy_start: None,
            lazy_track: None,
            byte_range: None,
            mapped_range: None,
        }
    }

    pub fn with_children(mut self, children: Vec<AstNode>) -> Self {
        self.children = children;
        self
    }

    #[allow(dead_code)]
    pub fn with_lazy(mut self, count: usize) -> Self {
        self.lazy_count = Some(count);
        self
    }

    pub fn with_lazy_range(mut self, start: usize, count: usize) -> Self {
        self.lazy_count = Some(count);
        self.lazy_start = Some(start);
        self
    }

    pub fn with_lazy_track(mut self, track: usize) -> Self {
        self.lazy_track = Some(track);
        self
    }

    pub fn with_byte_range(mut self, start: usize, len: usize) -> Self {
        let range = ByteRange::new(start, len);
        self.byte_range = Some(range);
        self.mapped_range = Some(MappedRange::original(start, len));
        self
    }
}

/// Messages sent from background workers to the UI.
pub enum AstBuildMessage {
    Full {
        generation: u64,
        nodes: Vec<AstNode>,
    },
    Partial {
        generation: u64,
        path: Vec<usize>,
        start: usize,
        nodes: Vec<AstNode>,
    },
    Diff {
        generation: u64,
        diffs: Vec<(usize, usize)>,
        rebuilt_bytes: Vec<u8>,
    },
    Error {
        generation: u64,
        message: String,
    },
}

pub(crate) fn source_node_to_ast(node: SourceNode) -> AstNode {
    let mut ast = AstNode::new(node.label, node.detail);
    if let Some(mapped) = node.range {
        ast.mapped_range = Some(mapped);
        ast.byte_range = mapped.hex_range();
    }
    ast.children = node.children.into_iter().map(source_node_to_ast).collect();
    ast
}

pub(crate) fn build_raw_binary_nodes(bytes: &[u8], parse_error: &str) -> Vec<AstNode> {
    let children = RawBinaryAdapter::default()
        .parse(bytes)
        .into_iter()
        .map(source_node_to_ast)
        .collect();
    let detail = format!(
        "{} bytes; structured format parse failed: {}",
        bytes.len(),
        parse_error
    );
    let mut root = AstNode::new("Raw Binary", detail).with_children(children);
    if !bytes.is_empty() {
        root.byte_range = Some(ByteRange::new(0, bytes.len()));
    }
    vec![root]
}

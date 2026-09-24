use super::AstNode;
use std::collections::HashMap;

pub struct LazyLoadState {
    pub loaded_nodes: HashMap<String, Vec<AstNode>>,
    pending_requests: HashMap<String, bool>,
}

pub(crate) struct ResolvedLazyRequest {
    pub absolute_start: usize,
    pub mdx_track: Option<usize>,
}

impl LazyLoadState {
    pub(crate) fn new() -> Self {
        Self {
            loaded_nodes: HashMap::new(),
            pending_requests: HashMap::new(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.loaded_nodes.clear();
        self.pending_requests.clear();
    }

    pub(crate) fn is_pending(&self, path: &[usize]) -> bool {
        self.pending_requests
            .get(&path_key(path))
            .copied()
            .unwrap_or(false)
    }

    pub(crate) fn begin_request(&mut self, path: &[usize]) -> bool {
        if self.is_pending(path) {
            return false;
        }
        self.pending_requests.insert(path_key(path), true);
        true
    }

    pub(crate) fn complete_request(&mut self, path: &[usize]) {
        self.pending_requests.remove(&path_key(path));
    }

    pub(crate) fn loaded(&self, path: &[usize]) -> Vec<AstNode> {
        self.loaded_nodes
            .get(&path_key(path))
            .cloned()
            .unwrap_or_default()
    }

    pub(crate) fn apply_partial(
        &mut self,
        path: &[usize],
        start: usize,
        nodes: Vec<AstNode>,
    ) -> usize {
        let key = path_key(path);
        let mut entry = self.loaded_nodes.remove(&key).unwrap_or_default();
        if start == entry.len() {
            entry.extend(nodes);
        } else if start < entry.len() {
            for (index, node) in (start..).zip(nodes) {
                if index < entry.len() {
                    entry[index] = node;
                } else {
                    entry.push(node);
                }
            }
        } else {
            for _ in 0..(start - entry.len()) {
                entry.push(AstNode::new("<placeholder>", ""));
            }
            entry.extend(nodes);
        }
        let length = entry.len();
        self.loaded_nodes.insert(key, entry);
        self.complete_request(path);
        length
    }
}

pub(crate) fn resolve_request(
    path: &[usize],
    start: usize,
    roots: &[AstNode],
) -> ResolvedLazyRequest {
    let mut base_start = 0usize;
    let mut mdx_track = None;
    let mut nodes = roots;
    for index in path {
        let Some(node) = nodes.get(*index) else {
            break;
        };
        if let Some(lazy_start) = node.lazy_start {
            base_start = lazy_start;
        }
        if let Some(track) = node.lazy_track {
            mdx_track = Some(track);
        }
        nodes = &node.children;
    }
    ResolvedLazyRequest {
        absolute_start: base_start.saturating_add(start),
        mdx_track,
    }
}

pub(crate) fn path_key(path: &[usize]) -> String {
    path.iter()
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

#[cfg(test)]
mod tests {
    use super::{LazyLoadState, path_key};
    use crate::gui::AstNode;

    #[test]
    fn pending_requests_are_deduplicated_until_completed() {
        let mut state = LazyLoadState::new();
        let path = [1, 2];

        assert!(!state.is_pending(&path));
        assert!(state.begin_request(&path));
        assert!(state.is_pending(&path));
        assert!(!state.begin_request(&path));
        state.complete_request(&path);
        assert!(!state.is_pending(&path));
        assert!(state.begin_request(&path));
    }

    #[test]
    fn partial_results_append_replace_and_fill_gaps() {
        let mut state = LazyLoadState::new();
        let path = [0];

        assert_eq!(
            state.apply_partial(&path, 0, vec![AstNode::new("a", "")]),
            1
        );
        assert_eq!(
            state.apply_partial(&path, 0, vec![AstNode::new("b", "")]),
            1
        );
        assert_eq!(
            state.apply_partial(&path, 2, vec![AstNode::new("c", "")]),
            3
        );

        let nodes = state.loaded(&path);
        assert_eq!(nodes[0].title, "b");
        assert_eq!(nodes[1].title, "<placeholder>");
        assert_eq!(nodes[2].title, "c");
        assert_eq!(path_key(&path), "0");
        assert!(!state.is_pending(&path));
    }

    #[test]
    fn clear_removes_loaded_nodes_and_pending_requests() {
        let mut state = LazyLoadState::new();
        let path = [3];
        state.begin_request(&path);
        state.apply_partial(&path, 0, vec![AstNode::new("node", "")]);
        state.begin_request(&[4]);

        state.clear();

        assert!(state.loaded_nodes.is_empty());
        assert!(!state.is_pending(&path));
        assert!(!state.is_pending(&[4]));
    }
}

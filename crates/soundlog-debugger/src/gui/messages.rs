use super::AstNode;
use super::ast::AstBuildMessage;
use super::state::UiState;
use eframe::egui;

pub(super) fn apply_message(state: &mut UiState, ctx: &egui::Context, message: AstBuildMessage) {
    match message {
        AstBuildMessage::Full { generation, nodes } => {
            if generation != state.parse_generation {
                return;
            }
            state.ast_root = nodes;
            state.lazy.clear();
            state.ast_building = false;
            state.push_event("received: full ast".to_string());
        }
        AstBuildMessage::Partial {
            generation,
            path,
            start,
            nodes,
        } => {
            if generation != state.parse_generation {
                return;
            }
            let nodes_count = nodes.len();
            let path_key = crate::gui::lazy::path_key(&path);
            let new_len = state.lazy.apply_partial(&path, start, nodes);
            state.push_event(format!(
                "recv partial: path={path:?} start={start} nodes={nodes_count}"
            ));
            state.push_event(format!("inserted: {path_key} now {new_len} items"));
        }
        AstBuildMessage::Diff {
            generation,
            diffs,
            rebuilt_bytes,
        } => {
            if generation != state.parse_generation {
                return;
            }
            state.hex_viewer.set_diff_ranges(diffs);
            state
                .hex_viewer
                .set_rebuilt_bytes(Some(rebuilt_bytes.clone()));
            state.rebuilt_bytes = Some(rebuilt_bytes);
            ctx.request_repaint();
            state.push_event("received: diff ranges".to_string());
        }
        AstBuildMessage::Error {
            generation,
            message,
        } => {
            if generation != state.parse_generation {
                return;
            }
            state.ast_root = vec![AstNode::new("Parse Error", message)];
            state.ast_building = false;
            state.lazy.clear();
            state.push_event("received: parse error".to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::apply_message;
    use crate::gui::{AstBuildMessage, AstNode, UiState};
    use eframe::egui;

    #[test]
    fn applies_current_generation_messages_to_ui_state() {
        let ctx = egui::Context::default();
        let mut state = UiState::new_empty();
        state.parse_generation = 7;
        state.ast_building = true;
        state
            .lazy
            .loaded_nodes
            .insert("0".to_string(), vec![AstNode::new("old", "old")]);

        apply_message(
            &mut state,
            &ctx,
            AstBuildMessage::Full {
                generation: 7,
                nodes: vec![AstNode::new("root", "current")],
            },
        );
        assert_eq!(state.ast_root[0].title, "root");
        assert!(state.lazy.loaded_nodes.is_empty());
        assert!(!state.ast_building);

        apply_message(
            &mut state,
            &ctx,
            AstBuildMessage::Partial {
                generation: 7,
                path: vec![0],
                start: 0,
                nodes: vec![AstNode::new("child", "current")],
            },
        );
        assert_eq!(state.lazy.loaded(&[0])[0].title, "child");

        apply_message(
            &mut state,
            &ctx,
            AstBuildMessage::Diff {
                generation: 7,
                diffs: vec![(2, 3)],
                rebuilt_bytes: vec![9, 8, 7],
            },
        );
        assert_eq!(state.rebuilt_bytes, Some(vec![9, 8, 7]));
        assert_eq!(state.hex_viewer.diff_ranges(), &[(2, 3)]);
    }

    #[test]
    fn ignores_stale_generation_messages() {
        let ctx = egui::Context::default();
        let mut state = UiState::new_empty();
        state.parse_generation = 2;
        state.ast_root = vec![AstNode::new("current", "current")];
        state.rebuilt_bytes = Some(vec![1]);

        apply_message(
            &mut state,
            &ctx,
            AstBuildMessage::Full {
                generation: 1,
                nodes: vec![AstNode::new("stale", "stale")],
            },
        );
        apply_message(
            &mut state,
            &ctx,
            AstBuildMessage::Diff {
                generation: 1,
                diffs: vec![(0, 0)],
                rebuilt_bytes: vec![2],
            },
        );

        assert_eq!(state.ast_root[0].title, "current");
        assert_eq!(state.rebuilt_bytes, Some(vec![1]));
        assert!(state.hex_viewer.diff_ranges().is_empty());
    }

    #[test]
    fn applies_current_generation_error_and_clears_lazy_state() {
        let ctx = egui::Context::default();
        let mut state = UiState::new_empty();
        state.parse_generation = 3;
        state
            .lazy
            .loaded_nodes
            .insert("0".to_string(), vec![AstNode::new("loaded", "loaded")]);

        apply_message(
            &mut state,
            &ctx,
            AstBuildMessage::Error {
                generation: 3,
                message: "parse failed".to_string(),
            },
        );

        assert_eq!(state.ast_root[0].title, "Parse Error");
        assert_eq!(state.ast_root[0].detail, "parse failed");
        assert!(state.lazy.loaded_nodes.is_empty());
        assert!(!state.ast_building);
    }
}

use super::AstNode;
use super::state::UiState;
use eframe::egui;

/// Render the top-level AST snapshot while keeping recursive tree drawing behind
/// the tree module boundary.
pub(crate) fn render_ast_tree(ui: &mut egui::Ui, nodes: &[AstNode], state: &mut UiState) {
    for (index, node) in nodes.iter().enumerate() {
        draw_ast_node(ui, node, vec![index], state);
    }
}

pub(crate) fn draw_ast_node(
    ui: &mut egui::Ui,
    node: &AstNode,
    path: Vec<usize>,
    state: &mut UiState,
) {
    use egui::CollapsingHeader;

    let display_title = node.title.lines().next().unwrap_or(&node.title).to_string();

    if let Some(total) = node.lazy_count {
        if node.lazy_start.is_some() {
            CollapsingHeader::new(
                egui::RichText::new(&display_title).size(state.hex_viewer.font_size()),
            )
            .default_open(total <= 100)
            .show(ui, |ui| {
                ui.add_space(4.0);
                let children = state.lazy.loaded(&path);
                if children.is_empty() {
                    let pending = state.lazy.is_pending(&path);
                    if pending {
                        ui.label("Loading...");
                    } else {
                        let key = path_key(&path);
                        if !state.enqueued_requests.contains_key(&key) {
                            state.deferred_loads.push((path.clone(), 0, total));
                            state.enqueued_requests.insert(key, true);
                        }
                        ui.label("Loading...");
                    }
                } else {
                    for (idx, child) in children.into_iter().enumerate() {
                        let mut child_path = path.clone();
                        child_path.push(idx);
                        draw_ast_node(ui, &child, child_path, state);
                    }
                }
            });
            return;
        }

        CollapsingHeader::new(
            egui::RichText::new(&display_title).size(state.hex_viewer.font_size()),
        )
        .default_open(total <= 100)
        .show(ui, |ui| {
            ui.add_space(4.0);
            let children = state.lazy.loaded(&path);
            let loaded = children.len();
            for (idx, child) in children.into_iter().enumerate() {
                let mut child_path = path.clone();
                child_path.push(idx);
                draw_ast_node(ui, &child, child_path, state);
            }

            if loaded < total {
                let pending = state.lazy.is_pending(&path);
                let button_label = format!("Show more ({}/{})", loaded, total);
                if pending {
                    ui.label(button_label);
                } else if ui.button(button_label).clicked() {
                    let key = path_key(&path);
                    if !state.enqueued_requests.contains_key(&key) {
                        state
                            .deferred_loads
                            .push((path.clone(), loaded, state.lazy_chunk_size));
                        state.enqueued_requests.insert(key, true);
                    }
                }
            }
        });
        return;
    }

    if node.children.is_empty() {
        let selected = state
            .selected_ast
            .as_ref()
            .map(|selected_path| *selected_path == path)
            .unwrap_or(false);
        let label_str = if path.len() >= 2
            && (path[0] == 0
                || state
                    .ast_root
                    .get(path[0])
                    .map(|root| root.title == "GD3")
                    .unwrap_or(false))
        {
            let detail_first = node.detail.lines().next().unwrap_or(&node.detail).trim();
            format!("{}: {}", display_title, detail_first)
        } else {
            display_title.clone()
        };
        let display_label = {
            let max_chars = 120usize;
            if label_str.chars().count() > max_chars {
                let mut label = label_str.chars().take(max_chars).collect::<String>();
                label.push_str("...");
                label
            } else {
                label_str.clone()
            }
        };
        let title_text = egui::RichText::new(display_label).size(state.hex_viewer.font_size());
        let response = ui.selectable_label(selected, title_text);
        response.clone().context_menu(|ui| {
            if ui.button("Copy").clicked() {
                let label_clone = label_str.clone();
                ui.ctx().output_mut(|output| {
                    output
                        .commands
                        .push(egui::OutputCommand::CopyText(label_clone));
                });
                state.push_event(format!("copied: {}", label_str));
                ui.close();
            }
        });

        if state.pending_focus.as_ref() == Some(&path) {
            state.last_focused_widget = Some(response.id);
            ui.ctx()
                .memory_mut(|memory| memory.request_focus(response.id));
        }
        if response.has_focus() && state.pending_focus.as_ref() == Some(&path) {
            ui.scroll_to_rect(response.rect, Some(egui::Align::Center));
            state.pending_focus = None;
        }
        if response.has_focus() && !selected {
            apply_node_selection(state, ui.ctx(), &path, node, response.rect);
        }
        if response.clicked() {
            state.last_focused_widget = Some(response.id);
            response.request_focus();
            apply_node_selection(state, ui.ctx(), &path, node, response.rect);
        }
    } else {
        CollapsingHeader::new(egui::RichText::new(&node.title).size(state.hex_viewer.font_size()))
            .default_open(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                for (index, child) in node.children.iter().enumerate() {
                    let mut child_path = path.clone();
                    child_path.push(index);
                    draw_ast_node(ui, child, child_path, state);
                }
            });
    }
}

pub(crate) fn handle_keyboard_selection(
    state: &mut UiState,
    ctx: &egui::Context,
    input: &egui::InputState,
    total: usize,
) {
    if !(input.key_pressed(egui::Key::ArrowUp) || input.key_pressed(egui::Key::ArrowDown))
        || total == 0
    {
        return;
    }

    let current = state
        .selected_ast
        .as_ref()
        .and_then(|path| path.first().copied());
    let next = if input.key_pressed(egui::Key::ArrowUp) {
        match current {
            Some(0) => Some(0),
            Some(index) => Some(index.saturating_sub(1)),
            None => Some(total.saturating_sub(1)),
        }
    } else {
        match current {
            Some(index) if index + 1 < total => Some(index + 1),
            Some(_) => Some(total.saturating_sub(1)),
            None => Some(0),
        }
    };

    let Some(index) = next else {
        return;
    };

    state.selected_ast = Some(vec![index]);
    state.pending_focus = Some(vec![index]);
    ctx.request_repaint();

    state.hex_viewer.clear_selection_range();
    state.hex_viewer.clear_reference_markers();
    state.hex_viewer.clear_outline_ranges();
    state.hex_viewer.set_selection_outline_enabled(true);

    if let Some(node) = state.ast_root.get(index) {
        if let Some((start, end)) = selection_range(node, state.bytes.len()) {
            state.hex_viewer.set_selection_range(start, end);
            state.hex_viewer.set_reference_markers(vec![start]);
            state.hex_viewer.set_pending_scroll_to(start, end);
        }
    }
}

pub(crate) fn selection_range(node: &AstNode, bytes_len: usize) -> Option<(usize, usize)> {
    match node.byte_range {
        Some(range) if range.start < bytes_len && range.len > 0 => {
            let end = range
                .start
                .saturating_add(range.len)
                .saturating_sub(1)
                .min(bytes_len.saturating_sub(1));
            Some((range.start, end))
        }
        _ => parse_address_from_detail(&node.detail)
            .filter(|address| *address < bytes_len)
            .map(|address| (address, address)),
    }
}

pub(crate) fn apply_node_selection(
    state: &mut UiState,
    ctx: &egui::Context,
    path: &[usize],
    node: &AstNode,
    rect: egui::Rect,
) {
    state.selected_ast = Some(path.to_vec());
    state.last_selected_ast_rect = Some(rect);
    ctx.request_repaint();

    state.hex_viewer.clear_selection_range();
    state.hex_viewer.clear_reference_markers();
    state.hex_viewer.clear_outline_ranges();
    state.hex_viewer.set_selection_outline_enabled(true);

    let mut applied = false;
    if path.len() >= 2
        && let Some(top) = state.ast_root.get(path[0])
        && (top.title == "Header" || top.title == "GD3")
        && let Some(range) = top.byte_range
        && range.start < state.bytes.len()
        && range.len > 0
    {
        let end = range
            .start
            .saturating_add(range.len)
            .saturating_sub(1)
            .min(state.bytes.len().saturating_sub(1));
        state.hex_viewer.set_selection_range(range.start, end);
        state.hex_viewer.set_reference_markers(vec![range.start]);
        state.hex_viewer.set_pending_scroll_to(range.start, end);
        state
            .hex_viewer
            .set_fill_only_ranges(vec![(range.start, end)]);
        state.hex_viewer.set_selection_outline_enabled(false);

        if let Some(child_range) = node.byte_range {
            let child_end = child_range.end().saturating_sub(1);
            state
                .hex_viewer
                .set_outline_ranges(vec![(child_range.start, child_end)]);
            state
                .hex_viewer
                .set_reference_markers(vec![child_range.start]);
            state
                .hex_viewer
                .set_pending_scroll_to(child_range.start, child_end);
        }
        applied = true;
    }

    if !applied {
        if let Some((start, end)) = selection_range(node, state.bytes.len()) {
            state.hex_viewer.set_selection_range(start, end);
            state.hex_viewer.set_reference_markers(vec![start]);
            state.hex_viewer.set_pending_scroll_to(start, end);
        }
    }
}

pub(crate) fn path_key(path: &[usize]) -> String {
    path.iter()
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// Parse the first hexadecimal address, or the first decimal sequence, from a
/// node detail string for legacy nodes without an explicit byte range.
pub(crate) fn parse_address_from_detail(detail: &str) -> Option<usize> {
    let trimmed = detail.trim();

    if let Some(position) = trimmed.find("0x") {
        let hex = trimmed[position + 2..]
            .chars()
            .take_while(|character| character.is_ascii_hexdigit())
            .collect::<String>();
        if !hex.is_empty() {
            if let Ok(value) = usize::from_str_radix(&hex, 16) {
                return Some(value);
            }
        }
    }

    if let Some(position) = trimmed.find(|character: char| character.is_ascii_digit()) {
        let decimal = trimmed[position..]
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect::<String>();
        if let Ok(value) = decimal.parse::<usize>() {
            return Some(value);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{parse_address_from_detail, path_key, selection_range};
    use crate::gui::AstNode;

    #[test]
    fn path_key_joins_nested_indices() {
        assert_eq!(path_key(&[1, 2, 3]), "1.2.3");
        assert_eq!(path_key(&[]), "");
    }

    #[test]
    fn parses_hex_before_decimal_addresses() {
        assert_eq!(parse_address_from_detail("offset 0x2a"), Some(42));
        assert_eq!(parse_address_from_detail("byte 17"), Some(17));
        assert_eq!(parse_address_from_detail("no address"), None);
    }

    #[test]
    fn selection_range_prefers_explicit_range_and_clamps_to_bytes() {
        let node = AstNode::new("field", "offset 0x2a").with_byte_range(3, 20);
        assert_eq!(selection_range(&node, 10), Some((3, 9)));

        let fallback = AstNode::new("field", "offset 0x2a");
        assert_eq!(selection_range(&fallback, 64), Some((42, 42)));

        let empty = AstNode::new("field", "offset 0x2a").with_byte_range(70, 2);
        assert_eq!(selection_range(&empty, 64), Some((42, 42)));
        assert_eq!(selection_range(&AstNode::new("field", "unknown"), 64), None);
    }
}

/*! UI module for soundlog-gui

Two-pane layout:
- left: VGM AST tree (supports lazy-loading of many command children)
- right: binary hex viewer (painter-based)

Strategy:
- On initial parse (background) we build a lightweight AST that contains a
  `Commands` node annotated with the total number of commands but *without*
  allocating child widgets/strings for each command.
- When the user expands `Commands` (or presses "Show more"), the UI requests
  a chunk of command nodes to be generated in a background thread. The
  background worker reparses (from bytes) and formats only the requested
  range into `AstNode`s, then sends them back to the UI which appends them
  into an in-memory chunk for incremental display.

This avoids doing large string allocation and widget construction on the UI
thread all at once and keeps the UI responsive for very large VGM files.
*/

use crate::gui::lazy::{LazyLoadState, resolve_request};
use crate::gui::loader::{spawn_children_parse, spawn_initial_parse};
use crate::gui::messages::apply_message;
use crate::gui::tree::{handle_keyboard_selection, path_key, render_ast_tree};
use crate::gui::{AstBuildMessage, AstNode, HexViewer};
use eframe::egui;

#[cfg(test)]
use crate::gui::ast::source_node_to_ast;

use std::collections::HashMap;
use std::fs;
use std::mem;
use std::sync::mpsc;

/// UI state holding AST, raw bytes and supporting maps for lazy-loading.
pub struct UiState {
    pub ast_root: Vec<AstNode>,
    pub bytes: Vec<u8>,
    pub selected_ast: Option<Vec<usize>>,
    /// The last observed selected AST label rect (widget coords). Used to
    /// scroll the left pane so keyboard-driven selection is visible.
    pub last_selected_ast_rect: Option<egui::Rect>,
    /// When keyboard navigation changes selection, this stores the path that
    /// should receive focus/scroll. Cleared once applied in the drawing pass.
    pub pending_focus: Option<Vec<usize>>,
    /// Last widget Id that requested focus by keyboard/interaction. Used to
    /// re-apply focus (for example to suppress TAB-based focus changes).
    pub last_focused_widget: Option<egui::Id>,
    pub hex_viewer: HexViewer,
    /// If a background parse produced rebuilt/serialized bytes (used to detect diffs),
    /// keep them here so UI components can access both original (`bytes`) and rebuilt bytes.
    pub rebuilt_bytes: Option<Vec<u8>>,

    /// Channel receiver to accept background build messages (full or partial).
    pub ast_build_rx: Option<mpsc::Receiver<AstBuildMessage>>,
    /// Channel sender to be cloned and used by background tasks.
    pub ast_build_tx: Option<mpsc::Sender<AstBuildMessage>>,

    /// Whether an initial parse is in progress.
    pub ast_building: bool,

    /// Monotonically increasing identifier for the currently loaded byte source.
    pub parse_generation: u64,

    /// For lazy nodes (keyed by path string like "0" or "1.2"), store the already
    /// loaded child nodes in display order (appended as partial chunks arrive).
    pub(crate) lazy: LazyLoadState,

    /// Chunk size for lazy loading (number of commands to request per click).
    pub lazy_chunk_size: usize,

    /// Deferred loads collected during UI drawing. These are executed after
    /// drawing to avoid multiple mutable borrows during recursive rendering.
    /// Each tuple is (path, start_relative, count).
    pub deferred_loads: Vec<(Vec<usize>, usize, usize)>,

    /// Temporary set of enqueued requests to prevent duplicate deferred loads.
    pub enqueued_requests: HashMap<String, bool>,
}

impl UiState {
    #[allow(dead_code)]
    pub fn new_with_placeholders() -> Self {
        let ast_root = vec![
            AstNode::new("VGM Header", "Header fields and metadata").with_children(vec![
                AstNode::new("Ident", "VgmIdent: 'Vgm '"),
                AstNode::new("Version", "0x00000150"),
            ]),
            AstNode::new("Commands", "No commands loaded").with_lazy(0),
        ];

        let bytes = (0u8..=255u8).collect::<Vec<u8>>();

        Self {
            ast_root,
            bytes,
            selected_ast: None,
            last_selected_ast_rect: None,
            pending_focus: None,
            last_focused_widget: None,
            hex_viewer: HexViewer::new(),
            rebuilt_bytes: None,
            ast_build_rx: None,
            ast_build_tx: None,
            ast_building: false,
            parse_generation: 0,
            lazy: LazyLoadState::new(),
            lazy_chunk_size: 200,
            deferred_loads: Vec::new(),
            enqueued_requests: HashMap::new(),
        }
    }

    pub fn new_empty() -> Self {
        Self {
            ast_root: Vec::new(),
            bytes: Vec::new(),
            selected_ast: None,
            last_selected_ast_rect: None,
            pending_focus: None,
            last_focused_widget: None,
            hex_viewer: HexViewer::new(),
            rebuilt_bytes: None,
            ast_build_rx: None,
            ast_build_tx: None,
            ast_building: false,
            parse_generation: 0,
            lazy: LazyLoadState::new(),
            lazy_chunk_size: 200,
            deferred_loads: Vec::new(),
            enqueued_requests: HashMap::new(),
        }
    }

    /// Push an event string into the recent_events buffer (kept as a no-op in
    /// non-debug builds).
    #[allow(dead_code)]
    pub(super) fn push_event(&mut self, _ev: impl Into<String>) {
        // Intentionally left empty: UI-level event logging removed for release build.
    }

    /// Kick off initial parse in background. This will produce a lightweight
    /// AST where the `Commands` node has `lazy_count = Some(total)`.
    pub fn populate_from_bytes(&mut self, bytes: &[u8]) {
        let input_changed = self.bytes != bytes;
        if input_changed {
            self.ast_root.clear();
            self.lazy.clear();
            self.rebuilt_bytes = None;
            self.selected_ast = None;
            self.pending_focus = None;
            self.last_selected_ast_rect = None;
            self.last_focused_widget = None;
            self.deferred_loads.clear();
            self.enqueued_requests.clear();
            self.hex_viewer.reset_document_state();
        }
        self.bytes = bytes.to_vec();

        // Keep the existing worker for repeated UI-frame calls with the same input.
        if self.ast_building && !input_changed {
            return;
        }

        self.parse_generation = self.parse_generation.wrapping_add(1);
        let generation = self.parse_generation;

        // Create a channel for background parse results if not already present.
        let (tx, rx) = mpsc::channel::<AstBuildMessage>();
        self.ast_build_rx = Some(rx);
        self.ast_build_tx = Some(tx.clone());
        self.ast_building = true;

        spawn_initial_parse(self.bytes.clone(), generation, tx);
    }

    /// Request a chunk of children for the node identified by `path`.
    /// - `start` is the first command index to build (relative to the bucket if
    ///   the node is a bucket; otherwise absolute).
    /// - `count` is how many commands to format.
    ///
    /// This spawns a background worker which reparses the source bytes and produces
    /// formatted `AstNode`s for the specified range. Results are sent via the
    /// shared sender stored in `ast_build_tx`. Note: the `start` in the
    /// `AstBuildMessage::Partial` is the *relative* offset within the bucket so
    /// the UI can insert the returned chunk at the correct position; the
    /// background worker will use absolute indices for parsing.
    pub fn request_children(&mut self, path: Vec<usize>, start: usize, count: usize) {
        // Build a stable key from path for bookkeeping.
        let path_key = path
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(".");

        // Avoid duplicate concurrent requests for the same path.
        if self.lazy.is_pending(&path) {
            self.push_event(format!("request skipped (pending): {}", path_key));
            return;
        }

        // Ensure we have bytes to parse and a sender to send results.
        if self.bytes.is_empty() {
            self.push_event("request skipped: no bytes".to_string());
            return;
        }
        let tx_opt = self.ast_build_tx.clone();
        if tx_opt.is_none() {
            self.push_event("request skipped: no tx".to_string());
            return;
        }
        let tx = tx_opt.unwrap();

        // Mark a pending request.
        self.lazy.begin_request(&path);
        self.push_event(format!(
            "request: {} start={} count={}",
            path_key, start, count
        ));

        // Clone bytes to move into thread.
        let data = self.bytes.clone();
        let generation = self.parse_generation;

        // Determine base absolute start for this path (if the node corresponds to a bucket).
        // If the node at `path` has a `lazy_start`, treat the provided `start` as
        // relative to that bucket; otherwise `start` is absolute.
        let resolved = resolve_request(&path, start, &self.ast_root);
        // Keep the relative start for returning in the Partial message.
        let relative_start = start;
        // Compute absolute start for parsing.
        let absolute_start = resolved.absolute_start;

        spawn_children_parse(
            data,
            generation,
            tx,
            path,
            relative_start,
            absolute_start,
            count,
            resolved.mdx_track,
        );
    }
}

/// Top-level UI entry called each frame.
pub fn show_ui(state: &mut UiState, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
    let ctx = ui.ctx().clone();

    // Native file drops provide a path. Load the first dropped file and send it
    // through the same parse/reset path used by the initial document.
    if let Some(path) = ctx.input(|input| {
        input
            .raw
            .dropped_files
            .first()
            .map(|file| file.path().to_path_buf())
    }) && let Ok(bytes) = fs::read(&path)
    {
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        state.populate_from_bytes(&bytes);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "soundlog debuger - {file_name}"
        )));
        ctx.request_repaint();
    }

    // If we have bytes but no AST yet, start initial populate.
    if state.ast_root.is_empty() && !state.bytes.is_empty() {
        let bytes_clone = state.bytes.clone();
        state.populate_from_bytes(&bytes_clone);
    }

    // Poll any background messages (drain all available messages).
    // To avoid borrow conflicts we first drain messages into a local Vec while
    // holding the receiver, then put the receiver back and process the messages
    // (which mutates `state`) afterwards.
    if let Some(rx) = state.ast_build_rx.take() {
        let mut msgs = Vec::new();
        let mut keep_rx = true;
        loop {
            match rx.try_recv() {
                Ok(msg) => {
                    msgs.push(msg);
                }
                Err(mpsc::TryRecvError::Empty) => {
                    // No more messages right now.
                    break;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Channel closed; do not put receiver back.
                    keep_rx = false;
                    break;
                }
            }
        }

        // Put the receiver back if it's still usable.
        if keep_rx {
            state.ast_build_rx = Some(rx);
        } else {
            state.ast_build_rx = None;
        }

        // Apply collected messages after releasing the receiver borrow.
        for msg in msgs {
            apply_message(state, &ctx, msg);
        }
    }

    egui::Panel::bottom("status_bar")
        .exact_size(32.0)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if state.ast_building {
                    ui.add_space(10.0);
                    ui.colored_label(ui.visuals().selection.bg_fill, "Parsing...");
                }
                state.hex_viewer.show_status_bar(ui);
            });
        });

    // Left sidebar AST
    egui::Panel::left("ast_panel")
        .resizable(false)
        // Reduce default left panel width so the hex viewer on the right is more visible.
        .default_size(240.0)
        // Keep the left panel width fixed so clicking inside doesn't cause the separator
        // to jump when internal content briefly changes size.
        .min_size(240.0)
        .max_size(240.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Add 8px top padding in the left pane.
                    ui.add_space(8.0);

                    // Clone the top-level AST into a snapshot to avoid holding an
                    // immutable borrow of `state.ast_root` while `render_ast_tree`
                    // may mutably borrow `state`. Cloning only the top-level
                    // nodes avoids borrow conflicts during recursive drawing.
                    let ast_snapshot = state.ast_root.clone();

                    // Keyboard navigation: Up/Down to move selection between top-level AST nodes.
                    // When selection changes, update the hex viewer selection similarly to a click.
                    // Use `ctx.input()` here so keyboard events are taken from the application
                    // context (not the local UI), which improves reliability for left-pane navigation.
                    let input = ctx.input(|i| i.clone());
                    let total = ast_snapshot.len();

                    // Tab pressed: schedule re-focus of currently selected AST path (strong suppression of Tab focus).
                    if input.key_pressed(egui::Key::Tab) {
                        // Use the currently selected AST path as the pending focus so Tab
                        // will not move focus to other UI elements. This strongly suppresses
                        // Tab-driven focus changes by re-asserting the selection as the target.
                        state.pending_focus = state.selected_ast.clone();
                        // Ensure the pending focus is applied promptly.
                        ctx.request_repaint();
                    }

                    // Diff navigation shortcuts: 'n' -> next, 'p' -> prev
                    if input.key_pressed(egui::Key::N) && state.hex_viewer.has_diffs() {
                        state.hex_viewer.next_diff();
                        ctx.request_repaint();
                    }
                    if input.key_pressed(egui::Key::P) && state.hex_viewer.has_diffs() {
                        state.hex_viewer.prev_diff();
                        ctx.request_repaint();
                    }

                    handle_keyboard_selection(state, &ctx, &input, total);

                    render_ast_tree(ui, &ast_snapshot, state);
                    // If an AST node set a last_selected_ast_rect during drawing (keyboard-driven
                    // navigation or click), scroll the left panel so the selected node is visible.
                    if let Some(r) = state.last_selected_ast_rect.take() {
                        ui.scroll_to_rect(r, Some(egui::Align::Center));
                    }
                });

            ui.with_layout(egui::Layout::bottom_up(egui::Align::Center), |ui| {
                ui.add_space(8.0);
            });
        });

    // Right: hex viewer
    egui::CentralPanel::default().show(ui, |ui| {
        let output = egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let row_height = (ui
                    .text_style_height(&egui::TextStyle::Monospace)
                    .max(state.hex_viewer.font_size())
                    + 6.0)
                    .max(18.0);
                ui.add_space(row_height);
                // Ensure the HexViewer always has access to the ORIGINAL file bytes so
                // its diff tooltip can display the true Original values even when the
                // viewer is asked to render the rebuilt bytes.
                state
                    .hex_viewer
                    .set_original_bytes(Some(state.bytes.clone()));

                // Prefer showing the rebuilt/serialized bytes in the right pane when available.
                // The background parse/serializer supplies `rebuilt_bytes` via AstBuildMessage::Diff.
                if let Some(rb) = state.rebuilt_bytes.as_ref() {
                    state.hex_viewer.show(ui, rb);
                } else {
                    state.hex_viewer.show(ui, &state.bytes);
                }
            });
        state.hex_viewer.paint_header(ui, output.inner_rect);
    });

    // If the HexViewer recorded a byte click, consume it here and focus the corresponding
    // AST node in the left pane (if a mapping exists). The HexViewer now exposes the last
    // clicked byte via `take_last_clicked_byte()` so we avoid using temporary egui storage.
    //
    // We consume the clicked index (if any), locate an AST node whose byte_range covers the clicked
    // offset (searching top-level AST nodes first, then any loaded lazy children), and then
    // set `state.selected_ast` / `state.pending_focus` so the left pane focuses that node.
    if let Some(clicked) = state.hex_viewer.take_last_clicked_byte() {
        let mut found_path: Option<Vec<usize>> = None;

        // 1) Check top-level AST nodes (e.g., Header, Commands, GD3) for a byte_range that covers the click.
        for (i, node) in state.ast_root.iter().enumerate() {
            if let Some(range) = node.byte_range {
                if range.contains(clicked) {
                    found_path = Some(vec![i]);
                    break;
                }
            }
        }

        // 2) If not found, search loaded lazy nodes (buckets) where each entry contains command AstNodes
        //    with their own byte_range. The loaded_lazy_nodes keys are path strings like "1.0".
        if found_path.is_none() {
            'outer: for (key, nodes) in state.lazy.loaded_nodes.iter() {
                // Parse key into a path Vec<usize> (e.g. "1.0" -> vec![1,0])
                let base_path: Vec<usize> = if key.is_empty() {
                    Vec::new()
                } else {
                    key.split('.')
                        .filter_map(|s| s.parse::<usize>().ok())
                        .collect::<Vec<usize>>()
                };

                for (idx, n) in nodes.iter().enumerate() {
                    if let Some(range) = n.byte_range {
                        if range.contains(clicked) {
                            let mut full_path = base_path.clone();
                            full_path.push(idx);
                            found_path = Some(full_path);
                            break 'outer;
                        }
                    }
                }
            }
        }

        // If we found a matching path, set selection + pending_focus so the left pane
        // highlights and scrolls to the matching command/node. Also update hex highlights.
        if let Some(path) = found_path {
            state.selected_ast = Some(path.clone());
            state.pending_focus = Some(path.clone());
            // Also update hex viewer selection and markers to reflect the clicked byte.
            state.hex_viewer.clear_selection_range();
            state.hex_viewer.set_selection_range(clicked, clicked);
            state.hex_viewer.set_reference_markers(vec![clicked]);
            state.hex_viewer.set_pending_scroll_to(clicked, clicked);
            // Repaint to ensure the left pane observes focus change promptly.
            ctx.request_repaint();
        }
    }

    // Drain deferred loads queued during drawing to avoid nested mutable borrows.
    if !state.deferred_loads.is_empty() {
        let mut to_process = Vec::new();
        mem::swap(&mut to_process, &mut state.deferred_loads);
        for (path, start, count) in to_process {
            let key = path_key(&path);
            // Remove enqueued marker so request_children can set pending_requests and proceed.
            state.enqueued_requests.remove(&key);
            state.request_children(path, start, count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AstNode, UiState, source_node_to_ast};
    use crate::gui::loader::compute_diff_ranges;
    use crate::sourcemap::{ByteCoordinateSpace, ByteRange, MappedRange, SourceNode, VgmAdapter};
    use soundlog::VgmBuilder;
    use soundlog::mdx::command::MdxRest;
    use soundlog::mdx::document::MdxBuilder;
    use soundlog::meta::Gd3;
    use soundlog::vgm::command::WaitSamples;
    use std::time::Duration;

    fn sample_vgm_bytes() -> Vec<u8> {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(735));
        builder.set_gd3(Gd3 {
            track_name_en: Some("Phase 1".to_string()),
            game_name_en: Some("GUI contract".to_string()),
            ..Default::default()
        });
        let document = builder.finalize();
        (&document).into()
    }

    #[test]
    fn empty_state_starts_without_document_data() {
        let state = UiState::new_empty();

        assert!(state.ast_root.is_empty());
        assert!(state.bytes.is_empty());
        assert!(state.rebuilt_bytes.is_none());
        assert!(!state.ast_building);
    }

    #[test]
    fn vgm_nodes_keep_header_command_and_gd3_ranges() {
        let bytes = sample_vgm_bytes();
        let document = soundlog::VgmDocument::try_from(bytes.as_slice()).unwrap();
        let header = source_node_to_ast(VgmAdapter::header_node(&document));
        let gd3 = source_node_to_ast(VgmAdapter::gd3_node(&document).expect("sample contains GD3"));

        assert_eq!(
            header.byte_range,
            Some(ByteRange::new(0, document.sourcemap()[0].0))
        );
        assert_eq!(header.children[0].title, "Ident");
        assert_eq!(header.children[0].byte_range, Some(ByteRange::new(0, 4)));
        assert_eq!(gd3.title, "GD3");
        assert!(gd3.byte_range.is_some());
        assert_eq!(gd3.children[0].title, "Track name (EN)");
        assert!(gd3.children[0].byte_range.unwrap().len > 0);
    }

    #[test]
    fn vgm_command_sourcemap_matches_serialized_command_ranges() {
        let bytes = sample_vgm_bytes();
        let document = soundlog::VgmDocument::try_from(bytes.as_slice()).unwrap();
        let ranges = document.sourcemap();

        assert_eq!(document.commands.len(), 2);
        assert_eq!(ranges.len(), document.commands.len());
        for (offset, length) in ranges {
            assert!(length > 0);
            assert!(offset + length <= bytes.len());
        }
    }

    #[test]
    fn diff_ranges_group_adjacent_changes_and_cover_length_changes() {
        assert_eq!(compute_diff_ranges(b"abcdef", b"abXYef"), vec![(2, 3)]);
        assert_eq!(
            compute_diff_ranges(b"abcdef", b"abXdefZ"),
            vec![(2, 2), (6, 6)]
        );
        assert_eq!(compute_diff_ranges(b"abcdef", b"abc"), vec![(3, 5)]);
        assert_eq!(compute_diff_ranges(b"abc", b"abcdef"), vec![(3, 5)]);
        assert!(compute_diff_ranges(b"same", b"same").is_empty());
        assert!(compute_diff_ranges(b"", b"").is_empty());
    }

    #[test]
    fn ast_nodes_preserve_lazy_range_and_byte_range_metadata() {
        let node = AstNode::new("Commands", "2 commands")
            .with_lazy_range(10, 2)
            .with_byte_range(32, 8);

        assert_eq!(node.lazy_count, Some(2));
        assert_eq!(node.lazy_start, Some(10));
        assert_eq!(node.byte_range, Some(ByteRange::new(32, 8)));
    }

    #[test]
    fn logical_source_ranges_are_not_sent_to_the_hex_viewer() {
        let node = SourceNode::new(1, "logical", "command").with_range(MappedRange {
            space: ByteCoordinateSpace::Logical,
            range: ByteRange::new(12, 3),
        });

        let ast = source_node_to_ast(node);

        assert_eq!(
            ast.mapped_range,
            Some(MappedRange {
                space: ByteCoordinateSpace::Logical,
                range: ByteRange::new(12, 3),
            })
        );
        assert_eq!(ast.byte_range, None);
    }

    #[test]
    fn invalid_vgm_bytes_fall_back_to_raw_binary_nodes() {
        let nodes = crate::gui::ast::build_raw_binary_nodes(&[1, 2, 3], "invalid VGM");

        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].title, "Raw Binary");
        assert!(nodes[0].detail.contains("invalid VGM"));
        assert_eq!(nodes[0].byte_range, Some(ByteRange::new(0, 3)));
        assert_eq!(nodes[0].children.len(), 1);
        assert_eq!(nodes[0].children[0].byte_range, Some(ByteRange::new(0, 3)));
    }

    #[test]
    fn populate_from_bytes_builds_mdx_nodes_after_vgm_rejection() {
        let mut builder = MdxBuilder::new();
        builder.add_mdx_command(0, MdxRest::new(12).unwrap());
        let bytes = builder.finalize().unwrap().to_bytes();

        let mut state = UiState::new_empty();
        state.populate_from_bytes(&bytes);
        let receiver = state.ast_build_rx.take().unwrap();

        let full = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
        match full {
            super::AstBuildMessage::Full { generation, nodes } => {
                assert_eq!(generation, 1);
                assert_eq!(nodes[0].title, "Header");
                assert_eq!(nodes[1].title, "Tone data");
                assert_eq!(nodes[2].title, "Track 0");
                assert_eq!(nodes[2].lazy_count, Some(2));
                assert_eq!(nodes[2].lazy_track, Some(0));
                assert!(nodes[2].children.is_empty());
                assert_eq!(nodes[3].title, "Track 1");
                assert_eq!(nodes[3].detail, "0 commands");
                assert!(nodes[3].lazy_count.is_none());
                assert!(nodes[3].lazy_track.is_none());
                state.ast_root = nodes;
            }
            message => panic!(
                "expected MDX Full message, got {:?}",
                message_type(&message)
            ),
        }

        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            super::AstBuildMessage::Diff { generation, .. } => assert_eq!(generation, 1),
            message => panic!(
                "expected MDX Diff message, got {:?}",
                message_type(&message)
            ),
        }
        state.request_children(vec![2], 0, 2);
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            super::AstBuildMessage::Partial {
                generation, nodes, ..
            } => {
                assert_eq!(generation, 1);
                assert_eq!(nodes[0].title, "0: Rest(MdxRest { ticks: 12 })");
                assert_eq!(nodes[1].title, "1: EndOfTrack(MdxEndOfTrack)");
                assert!(nodes[0].byte_range.is_some());
            }
            message => panic!(
                "expected MDX Partial message, got {:?}",
                message_type(&message)
            ),
        }
    }

    #[test]
    fn replacing_input_during_parse_advances_generation() {
        let mut first_builder = MdxBuilder::new();
        first_builder.add_mdx_command(0, MdxRest::new(12).unwrap());
        let first = first_builder.finalize().unwrap().to_bytes();

        let mut second_builder = MdxBuilder::new();
        second_builder.add_mdx_command(0, MdxRest::new(24).unwrap());
        let second = second_builder.finalize().unwrap().to_bytes();

        let mut state = UiState::new_empty();
        state.populate_from_bytes(&first);
        assert_eq!(state.parse_generation, 1);
        state.populate_from_bytes(&second);
        assert_eq!(state.parse_generation, 2);

        let receiver = state.ast_build_rx.take().unwrap();
        match receiver.recv_timeout(Duration::from_secs(1)).unwrap() {
            super::AstBuildMessage::Full { generation, .. } => assert_eq!(generation, 2),
            message => panic!(
                "expected current-generation Full message, got {:?}",
                message_type(&message)
            ),
        }
    }

    fn message_type(message: &super::AstBuildMessage) -> &'static str {
        match message {
            super::AstBuildMessage::Full { .. } => "Full",
            super::AstBuildMessage::Partial { .. } => "Partial",
            super::AstBuildMessage::Diff { .. } => "Diff",
            super::AstBuildMessage::Error { .. } => "Error",
        }
    }
}

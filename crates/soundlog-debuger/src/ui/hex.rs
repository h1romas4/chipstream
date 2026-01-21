//! Painter-based HexViewer component for soundlog-gui.
//!
//! This module implements a small, self-contained hex viewer widget which
//! renders its contents using `egui::Painter`. It supports:
//!  - fixed bytes-per-line layout (configurable),
//!  - painter-based drawing of offsets, hex bytes and ASCII column,
//!  - click-to-select a single byte (highlighted),
//!  - range selection outline and reference markers (added).
//!
//! The widget is intentionally lightweight and does not (yet) implement:
//!  - keyboard selection/drag selection,
//!  - highly optimized rendering of extremely large buffers.
//!
//! Usage example:
//! ```ignore
//! let mut viewer = hex_viewer::HexViewer::new();
//! viewer.show(ui, &bytes);
//! if let Some(idx) = viewer.selected() { /* use selected byte idx */ }
//! viewer.set_selection_range(0x100, 0x1FF);
//! viewer.set_reference_markers(vec![0x120, 0x180]);
//! ```
#![allow(clippy::manual_div_ceil)]
use eframe::egui;

/// Stateful painter-based hex viewer.
pub struct HexViewer {
    /// Bytes shown per line.
    bytes_per_line: usize,
    /// Font size used for drawing.
    font_size: f32,
    /// Currently selected global byte index (if any).
    selected: Option<usize>,
    /// Optional selected byte range (inclusive start, inclusive end).
    selection_range: Option<(usize, usize)>,
    /// Offsets to mark as references (displayed as markers in the margin).
    reference_markers: Vec<usize>,
    /// Outline-only ranges (inclusive start, inclusive end) drawn as overlay strokes.
    outline_ranges: Vec<(usize, usize)>,
    /// Ranges that should be fill-only (no stroke) even if selection outlines are enabled.
    fill_only_ranges: Vec<(usize, usize)>,
    /// Ranges that represent differences between original and parsed VGM bytes;
    /// these will be drawn as a red overlay stroke to show mismatches.
    diff_ranges: Vec<(usize, usize)>,
    /// Currently selected diff index for NEXT/PREV navigation (index into `diff_ranges`).
    current_diff_idx: Option<usize>,
    /// Control whether selection ranges receive a faint outline stroke in addition to the fill.
    selection_outline_enabled: bool,
    /// Optional pending scroll request: when set, `show()` will attempt to
    /// scroll the surrounding ScrollArea so the requested byte range is visible.
    pending_scroll_to: Option<(usize, usize)>,
    /// If true, the pending scroll (when present) should align to the top (0.0)
    /// of the ScrollArea instead of centering the target rect. Used when we
    /// want the viewer top to be 0x0 (for example immediately after diff detection).
    pending_scroll_align_top: bool,
    /// The last computed selection rect in widget coordinates (if any).
    /// This can be consumed by the caller to coordinate scrolling if desired.
    last_selection_rect: Option<egui::Rect>,
    /// Optional original bytes (file bytes) kept so the tooltip can always show
    /// the true Original bytes even when the viewer is displaying rebuilt bytes.
    original_bytes: Option<Vec<u8>>,
    /// Last clicked byte index (set when the user clicks a byte cell). Cleared
    /// when consumed via `take_last_clicked_byte()`.
    last_clicked_byte: Option<usize>,
    /// Optional rebuilt/serialized bytes produced by the background parser so
    /// the viewer can display both Original and Rebuilt data in tooltips.
    rebuilt_bytes: Option<Vec<u8>>,
}

impl HexViewer {
    /// Create a new HexViewer with default settings.
    pub fn new() -> Self {
        Self {
            bytes_per_line: 16,
            font_size: 12.0,
            selected: None,
            selection_range: None,
            reference_markers: Vec::new(),
            outline_ranges: Vec::new(),
            fill_only_ranges: Vec::new(),
            diff_ranges: Vec::new(),
            current_diff_idx: None,
            selection_outline_enabled: true,
            pending_scroll_to: None,
            pending_scroll_align_top: false,
            last_selection_rect: None,
            // Initialize the original bytes field so tooltips can reference the true
            // original file bytes even when the viewer is asked to display rebuilt bytes.
            original_bytes: None,
            rebuilt_bytes: None,
            last_clicked_byte: None,
        }
    }

    /// Set the original bytes (file bytes) so the tooltip can always show Original.
    /// This should be called by the UI layer with the true file bytes even when the
    /// viewer is asked to display rebuilt bytes.
    #[allow(dead_code)]
    pub fn set_original_bytes(&mut self, bytes: Option<Vec<u8>>) {
        self.original_bytes = bytes;
    }

    /// Set the rebuilt/serialized bytes so the viewer can reference them in tooltips.
    #[allow(dead_code)]
    pub fn set_rebuilt_bytes(&mut self, bytes: Option<Vec<u8>>) {
        self.rebuilt_bytes = bytes;
    }

    /// Consume and return the last clicked byte index, if any.
    #[allow(dead_code)]
    pub fn take_last_clicked_byte(&mut self) -> Option<usize> {
        self.last_clicked_byte.take()
    }

    /// Set the number of bytes per line.
    #[allow(dead_code)]
    pub fn with_bytes_per_line(mut self, bpl: usize) -> Self {
        self.bytes_per_line = bpl.max(1);
        self
    }

    /// Set the font size used for rendering.
    #[allow(dead_code)]
    pub fn with_font_size(mut self, size: f32) -> Self {
        if size > 0.0 {
            self.font_size = size;
        }
        self
    }

    /// Return the configured font size so other UI parts can match rendering.
    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Returns the currently selected byte index (if any).
    #[allow(dead_code)]
    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Set an inclusive selection range. If `end < start` the values will be swapped.
    pub fn set_selection_range(&mut self, start: usize, end: usize) {
        if end >= start {
            self.selection_range = Some((start, end));
        } else {
            self.selection_range = Some((end, start));
        }
    }

    /// Clear any selection range.
    pub fn clear_selection_range(&mut self) {
        self.selection_range = None;
    }

    /// Set a list of absolute offsets to mark in the margin as references.
    pub fn set_reference_markers(&mut self, refs: Vec<usize>) {
        self.reference_markers = refs;
    }

    /// Clear reference markers.
    pub fn clear_reference_markers(&mut self) {
        self.reference_markers.clear();
    }

    /// Set explicit outline-only ranges to draw as overlay strokes.
    /// Each entry is (start_inclusive, end_inclusive).
    pub fn set_outline_ranges(&mut self, outlines: Vec<(usize, usize)>) {
        self.outline_ranges = outlines;
    }

    /// Clear outline-only ranges.
    pub fn clear_outline_ranges(&mut self) {
        self.outline_ranges.clear();
    }

    /// Set ranges that should be fill-only (no stroke drawn for selection).
    /// Each entry is (start_inclusive, end_inclusive).
    pub fn set_fill_only_ranges(&mut self, fills: Vec<(usize, usize)>) {
        self.fill_only_ranges = fills;
    }

    /// Clear fill-only ranges.
    #[allow(dead_code)]
    pub fn clear_fill_only_ranges(&mut self) {
        self.fill_only_ranges.clear();
    }

    /// Set ranges that represent diffs between the original file bytes and the
    /// parsed/serialized bytes. These ranges are drawn as a red overlay stroke
    /// to indicate mismatched bytes.
    pub fn set_diff_ranges(&mut self, diffs: Vec<(usize, usize)>) {
        self.diff_ranges = diffs;
        // Reset current diff index to first diff if any diffs are present.
        if self.diff_ranges.is_empty() {
            self.current_diff_idx = None;
            // Clear any previous visual marks when there are no diffs.
            self.clear_selection_range();
            self.clear_outline_ranges();
            self.clear_reference_markers();
        } else {
            self.current_diff_idx = Some(0);
            // Request auto-scroll to top (0x0) on next show() call so binary view top is at 0.
            self.set_pending_scroll_to(0, 0);
            // Also set the selection/outline/marker for the initial diff so it's visible immediately.
            let (s, e) = self.diff_ranges[0];
            self.set_selection_range(s, e);
            self.set_reference_markers(vec![s]);
            self.set_outline_ranges(vec![(s, e)]);
            self.set_selection_outline_enabled(true);
        }
    }

    /// Clear diff ranges.
    #[allow(dead_code)]
    pub fn clear_diff_ranges(&mut self) {
        self.diff_ranges.clear();
        self.current_diff_idx = None;
    }

    /// Return a slice of the current diff ranges (inclusive start, inclusive end).
    pub fn diff_ranges(&self) -> &[(usize, usize)] {
        &self.diff_ranges
    }

    /// Convenience: return true if any diff ranges are present.
    pub fn has_diffs(&self) -> bool {
        !self.diff_ranges.is_empty()
    }

    /// Return the currently selected diff index (if any).
    pub fn current_diff_index(&self) -> Option<usize> {
        self.current_diff_idx
    }

    /// Return the currently selected diff range (if any).
    #[allow(dead_code)]
    pub fn current_diff_range(&self) -> Option<(usize, usize)> {
        if let Some(idx) = self.current_diff_idx {
            self.diff_ranges.get(idx).copied()
        } else {
            None
        }
    }

    /// Advance to the next diff (wraps). Updates `current_diff_idx` and requests
    /// scrolling to the selected diff range.
    pub fn next_diff(&mut self) {
        if self.diff_ranges.is_empty() {
            self.current_diff_idx = None;
            // Clear any selection/outline/marker when no diffs.
            self.clear_selection_range();
            self.clear_outline_ranges();
            self.clear_reference_markers();
            return;
        }
        let len = self.diff_ranges.len();
        let next = match self.current_diff_idx {
            Some(i) => (i + 1) % len,
            None => 0,
        };
        self.current_diff_idx = Some(next);
        let (s, e) = self.diff_ranges[next];
        // Ensure the viewer scrolls to this diff and highlight it (selection + outline).
        self.set_pending_scroll_to(s, e);
        self.set_selection_range(s, e);
        self.set_reference_markers(vec![s]);
        self.set_outline_ranges(vec![(s, e)]);
        self.set_selection_outline_enabled(true);
    }

    /// Move to the previous diff (wraps). Updates `current_diff_idx` and requests
    /// scrolling to the selected diff range.
    pub fn prev_diff(&mut self) {
        if self.diff_ranges.is_empty() {
            self.current_diff_idx = None;
            // Clear any selection/outline/marker when no diffs.
            self.clear_selection_range();
            self.clear_outline_ranges();
            self.clear_reference_markers();
            return;
        }
        let len = self.diff_ranges.len();
        let prev = match self.current_diff_idx {
            Some(i) => (i + len - 1) % len,
            None => 0,
        };
        self.current_diff_idx = Some(prev);
        let (s, e) = self.diff_ranges[prev];
        // Ensure the viewer scrolls to this diff and highlight it (selection + outline).
        self.set_pending_scroll_to(s, e);
        self.set_selection_range(s, e);
        self.set_reference_markers(vec![s]);
        self.set_outline_ranges(vec![(s, e)]);
        self.set_selection_outline_enabled(true);
    }

    /// Enable or disable drawing an outline stroke for selection ranges.
    /// When `false`, selection ranges will be drawn as fill-only (no stroke).
    pub fn set_selection_outline_enabled(&mut self, enabled: bool) {
        self.selection_outline_enabled = enabled;
    }

    /// Request that the viewer auto-scroll to the inclusive byte range (start, end)
    /// on the next `show()` call. The call will be consumed after the scroll is
    /// attempted.
    pub fn set_pending_scroll_to(&mut self, start: usize, end: usize) {
        if end >= start {
            self.pending_scroll_to = Some((start, end));
        } else {
            self.pending_scroll_to = Some((end, start));
        }
    }

    /// Take the last computed selection rect (if any) produced during `show()` and clear it.
    /// This can be used by callers to perform custom scroll logic if desired.
    #[allow(dead_code)]
    pub fn take_last_selection_rect(&mut self) -> Option<egui::Rect> {
        self.last_selection_rect.take()
    }

    /// Show the viewer inside the provided `ui`, rendering `bytes`.
    ///
    /// This function allocates a rectangular area sized to the available width
    /// and the number of needed lines, draws the content with the painter, and
    /// updates the internal `selected` index when the user clicks a byte.
    ///
    /// If `set_pending_scroll_to` was called before this `show()` invocation,
    /// `show()` will attempt to auto-scroll the current UI scroll area so the
    /// requested byte range is visible.
    pub fn show(&mut self, ui: &mut egui::Ui, bytes: &[u8]) {
        // Clear last selection rect
        self.last_selection_rect = None;

        // Determine metrics from UI + configured font size.
        let font = egui::FontId::monospace(self.font_size);
        // Use monospace text height plus a small padding for row height.
        let mono_text_height = ui.text_style_height(&egui::TextStyle::Monospace);
        let row_height = (mono_text_height.max(self.font_size) + 6.0).max(18.0);

        let bpl = self.bytes_per_line;
        let lines = if bytes.is_empty() {
            0
        } else {
            (bytes.len() + bpl - 1) / bpl
        };
        let available_width = ui.available_width();

        // Heuristics for column widths (based on monospace approx).
        // Approximate character width from font size (monospace).
        let char_w = self.font_size * 0.6_f32;
        let offset_chars = 9.0; // "00000000:" -> 9 chars (8 hex + colon)
        let offset_width = offset_chars * char_w + 8.0;
        let hex_cell_w = char_w * 3.0; // "FF " (two hex + space)
        let _ascii_cell_w = char_w * 1.0;
        let sep_gap = 12.0_f32;

        // Compute required total height and allocate an area.
        let total_height = (lines as f32) * row_height;
        let total_size = egui::Vec2::new(available_width, total_height);

        // Allocate space in the UI for the whole viewer.
        let (rect, resp) = ui.allocate_exact_size(total_size, egui::Sense::click());

        // Painter at the allocated rectangle.
        let painter = ui.painter_at(rect);

        // Background for the whole widget (subtle)
        // Use a darker fill in dark mode to match dark themes while keeping the
        // light-mode behavior unchanged.
        let bg_color = if ui.visuals().dark_mode {
            // A subtle dark background suitable for hex viewing (adjust RGB as desired).
            egui::Color32::from_rgb(28, 28, 30)
        } else {
            ui.visuals().panel_fill
        };
        painter.rect_filled(rect, 0.0, bg_color);

        // Starting base point for text on each line.
        let base_x = rect.min.x + 6.0;

        // Precompute ascii column start X
        let ascii_base_x = base_x + offset_width + (bpl as f32) * hex_cell_w + sep_gap;

        // Draw reference markers in the margin (small circles) for marked offsets.
        for &r in &self.reference_markers {
            if r >= bytes.len() {
                continue;
            }
            let line_idx = r / bpl;
            let line_top = rect.min.y + (line_idx as f32) * row_height + 2.0;
            let center = egui::pos2(base_x - 8.0, line_top + (row_height - 4.0) * 0.5);
            // `extra_light_text_color` is not a method on `Visuals`. Use `text_color()` which
            // returns a `Color32` suitable for the small marker.
            let col = ui.visuals().text_color();
            painter.circle_filled(center, 3.0, col);
        }

        // iterate lines and draw only visible lines using painter to improve performance
        // Compute visible range using the current clip rect so we draw only what's visible.
        let clip_rect = ui.clip_rect();
        let visible_top = clip_rect.min.y.max(rect.min.y);
        let visible_bottom = clip_rect.max.y.min(rect.max.y);

        if visible_bottom > visible_top && lines > 0 {
            // Map visible Y range to line indices
            let first_line_f = ((visible_top - rect.min.y) / row_height).floor();
            let last_line_f = ((visible_bottom - rect.min.y) / row_height).ceil();

            let mut first_line = first_line_f.max(0.0) as usize;
            let mut last_line = last_line_f.max(0.0) as usize;

            let last_index = lines.saturating_sub(1);
            first_line = first_line.min(last_index);
            last_line = last_line.min(last_index);

            // Draw only the visible lines
            for line_idx in first_line..=last_line {
                let offset = line_idx * bpl;
                let end = ((line_idx + 1) * bpl).min(bytes.len());
                let chunk = &bytes[offset..end];

                // y coordinate for this line's top
                let line_top = rect.min.y + (line_idx as f32) * row_height + 2.0;

                // Draw offset
                let offset_text = format!("{:08X}:", offset);
                painter.text(
                    egui::pos2(base_x, line_top),
                    egui::Align2::LEFT_TOP,
                    offset_text.clone(),
                    font.clone(),
                    ui.visuals().text_color(),
                );

                // Draw hex cells
                for (i, b) in chunk.iter().enumerate() {
                    let global_idx = offset + i;
                    let x = base_x + offset_width + (i as f32) * hex_cell_w;

                    let cell_min = egui::pos2(x, line_top);
                    let cell_rect = egui::Rect::from_min_size(
                        cell_min,
                        egui::vec2(hex_cell_w, row_height - 4.0),
                    );

                    // If single byte selected, draw highlight rectangle.
                    // Range fills are drawn per-line (below) to avoid gaps between cells.
                    if self.selected == Some(global_idx) {
                        let radius = 2.0;
                        let highlight_color = ui.visuals().selection.bg_fill;
                        painter.rect_filled(cell_rect, radius, highlight_color);
                    }

                    // Draw hex text centered in cell
                    let hex_text = format!("{:02X}", b);
                    painter.text(
                        egui::pos2(
                            cell_rect.center().x,
                            cell_rect.center().y - (mono_text_height * 0.35),
                        ),
                        egui::Align2::CENTER_TOP,
                        hex_text,
                        font.clone(),
                        ui.visuals().text_color(),
                    );
                }

                // Draw ASCII column
                let mut ascii_text = String::with_capacity(chunk.len());
                for b in chunk.iter() {
                    let ch = if b.is_ascii_graphic() || *b == b' ' {
                        *b as char
                    } else {
                        '.'
                    };
                    ascii_text.push(ch);
                }
                painter.text(
                    egui::pos2(ascii_base_x, line_top),
                    egui::Align2::LEFT_TOP,
                    ascii_text,
                    font.clone(),
                    ui.visuals().text_color(),
                );
            }
        }

        // Draw selection_range as a continuous filled band (per-line segments) and stroke with a dark red outline.
        if let Some((mut s, mut e)) = self.selection_range {
            // Only draw if at least one endpoint is inside the file.
            let bytes_len = bytes.len();
            if s < bytes_len || e < bytes_len {
                if e < s {
                    std::mem::swap(&mut s, &mut e);
                }
                // Clamp to file bounds.
                let s_clamped = s.min(bytes_len.saturating_sub(1));
                let e_clamped = e.min(bytes_len.saturating_sub(1));

                let start_line = s_clamped / bpl;
                let end_line = e_clamped / bpl;

                // Use the UI selection colors for the range fill only.
                // Derive a semi-transparent fill from the selection background so the
                // fill remains readable across themes.
                let sel = ui.visuals().selection;
                let base_fill = sel.bg_fill;
                // Use the selection color but with controlled alpha for translucency.
                let fill_color = egui::Color32::from_rgba_unmultiplied(
                    base_fill.r(),
                    base_fill.g(),
                    base_fill.b(),
                    140,
                );

                // We'll compute a union rect across the per-line segments so we can
                // optionally auto-scroll to encompass the entire selection.
                // Also prepare a subtle outline stroke so selections remain visible
                // against similarly-colored AST items in the left pane.
                let sel = ui.visuals().selection;
                let stroke = {
                    let st = sel.stroke;
                    // Use a thin stroke with slightly reduced alpha for subtlety.
                    let c = egui::Color32::from_rgba_unmultiplied(
                        st.color.r(),
                        st.color.g(),
                        st.color.b(),
                        200,
                    );
                    egui::Stroke::new(1.0, c)
                };
                let mut union_rect: Option<egui::Rect> = None;

                // Draw per-line filled segments with slight inset so the stroke does not
                // overlap the hex text area and no gaps appear between adjacent cells.
                for line in start_line..=end_line {
                    let line_top = rect.min.y + (line as f32) * row_height + 2.0;
                    let line_start = if line == start_line {
                        (s_clamped % bpl) as f32
                    } else {
                        0.0
                    };
                    let line_end = if line == end_line {
                        (e_clamped % bpl) as f32
                    } else {
                        (bpl as f32) - 1.0
                    };

                    // Inset the filled rect slightly from cell bounds so that outline stroke
                    // is drawn inside the hex area and does not cover text.
                    let x0 = base_x + offset_width + line_start * hex_cell_w + 1.0;
                    let x1 = base_x + offset_width + (line_end + 1.0) * hex_cell_w - 1.0;
                    let y0 = line_top + 1.0;
                    let y1 = line_top + row_height - 4.0;

                    let seg_rect = egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1));
                    // Fill; draw stroke only if selection outlines are enabled and this selection
                    // is not covered by a fill-only range.
                    painter.rect_filled(seg_rect, 0.0, fill_color);
                    if self.selection_outline_enabled {
                        // If any configured fill-only range fully covers the selection, skip stroke.
                        let mut covered = false;
                        for &(fs, fe) in &self.fill_only_ranges {
                            if fs <= s_clamped && fe >= e_clamped {
                                covered = true;
                                break;
                            }
                        }
                        if !covered {
                            painter.rect_stroke(seg_rect, 1.0, stroke);
                        }
                    }

                    union_rect = Some(if let Some(u) = union_rect {
                        u.union(seg_rect)
                    } else {
                        seg_rect
                    });
                }

                // Save last computed selection rect for external use.
                if let Some(r) = union_rect {
                    // Keep the last computed rect for callers; do not show any tooltip for the selection band.
                    self.last_selection_rect = Some(r);
                } else {
                    self.last_selection_rect = None;
                }

                // Draw overlay outlines for explicit `outline_ranges` (e.g., Header field overlays).
                // These are stroke-only overlays drawn on top of any filled selection so
                // they appear as an outline/overlay rather than another filled band.
                for &(o_s, o_e) in &self.outline_ranges {
                    // Only draw if at least one endpoint is inside the file.
                    if o_s < bytes_len || o_e < bytes_len {
                        let o_start = o_s.min(bytes_len.saturating_sub(1));
                        let o_end = o_e.min(bytes_len.saturating_sub(1));
                        if o_end >= o_start {
                            let o_start_line = o_start / bpl;
                            let o_end_line = o_end / bpl;
                            for line in o_start_line..=o_end_line {
                                let line_top = rect.min.y + (line as f32) * row_height + 2.0;
                                let line_start = if line == o_start_line {
                                    (o_start % bpl) as f32
                                } else {
                                    0.0
                                };
                                let line_end = if line == o_end_line {
                                    (o_end % bpl) as f32
                                } else {
                                    (bpl as f32) - 1.0
                                };

                                let x0 = base_x + offset_width + line_start * hex_cell_w + 1.0;
                                let x1 =
                                    base_x + offset_width + (line_end + 1.0) * hex_cell_w - 1.0;
                                let y0 = line_top + 1.0;
                                let y1 = line_top + row_height - 4.0;

                                let o_rect = egui::Rect::from_min_max(
                                    egui::pos2(x0, y0),
                                    egui::pos2(x1, y1),
                                );

                                // Draw stroke-only overlay: slightly stronger alpha so the outline
                                // is visible on top of fills but still looks like an overlay.
                                let overlay_stroke = egui::Stroke::new(
                                    1.0,
                                    egui::Color32::from_rgba_unmultiplied(
                                        stroke.color.r(),
                                        stroke.color.g(),
                                        stroke.color.b(),
                                        230,
                                    ),
                                );
                                painter.rect_stroke(o_rect, 0.0, overlay_stroke);
                            }
                        }
                    }
                }
            }
        }

        // Draw diff overlays for any configured `diff_ranges`.
        // Each diff will be drawn as a semi-transparent red fill plus a red outline.
        // The currently-selected diff (if any) is highlighted with a stronger fill and thicker stroke.
        for (idx, &(d_s, d_e)) in self.diff_ranges.iter().enumerate() {
            if d_s < bytes.len() || d_e < bytes.len() {
                let ds = d_s.min(bytes.len().saturating_sub(1));
                let de = d_e.min(bytes.len().saturating_sub(1));
                if de >= ds {
                    let d_start_line = ds / bpl;
                    let d_end_line = de / bpl;
                    for line in d_start_line..=d_end_line {
                        let line_top = rect.min.y + (line as f32) * row_height + 2.0;
                        let line_start = if line == d_start_line {
                            (ds % bpl) as f32
                        } else {
                            0.0
                        };
                        let line_end = if line == d_end_line {
                            (de % bpl) as f32
                        } else {
                            (bpl as f32) - 1.0
                        };

                        let x0 = base_x + offset_width + line_start * hex_cell_w + 1.0;
                        let x1 = base_x + offset_width + (line_end + 1.0) * hex_cell_w - 1.0;
                        let y0 = line_top + 1.0;
                        let y1 = line_top + row_height - 4.0;

                        let d_rect =
                            egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1));

                        // Active diff visual style is stronger to stand out.
                        let is_active = self.current_diff_idx.map(|i| i == idx).unwrap_or(false);

                        // Fill: semi-transparent red (stronger for active).
                        let fill_color = if is_active {
                            egui::Color32::from_rgba_unmultiplied(220, 60, 60, 120)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(200, 60, 60, 90)
                        };
                        painter.rect_filled(d_rect, 0.0, fill_color);

                        // Stroke: red outline (thicker for active).
                        let color = if is_active {
                            egui::Color32::from_rgba_unmultiplied(220, 24, 24, 255)
                        } else {
                            egui::Color32::from_rgba_unmultiplied(200, 36, 36, 220)
                        };
                        let width = if is_active { 2.0 } else { 1.0 };
                        let diff_stroke = egui::Stroke::new(width, color);
                        painter.rect_stroke(d_rect, 0.0, diff_stroke);

                        // If the pointer is hovering over this diff overlay segment, show a tooltip
                        // that explicitly displays the ORIGINAL bytes for the full diff range so the user
                        // can see "Original:" content. We build a limited-length hex dump and an
                        // ASCII representation for readability.
                        if let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                            && d_rect.contains(pos)
                        {
                            // Determine a source buffer for the Original bytes:
                            // Prefer `self.original_bytes` if it was set by the UI layer;
                            // otherwise fall back to the `bytes` slice passed to `show()`.
                            let orig_buf: &[u8] = self.original_bytes.as_deref().unwrap_or(bytes);

                            // Guard: ensure ds..=de is within bounds of the original buffer.
                            if ds <= de && de < orig_buf.len() {
                                let orig_slice = &orig_buf[ds..=de];
                                let len_bytes = orig_slice.len();

                                // Build a hex representation (truncate if very long).
                                let hex_str = if len_bytes <= 64 {
                                    orig_slice
                                        .iter()
                                        .map(|b| format!("{:02X}", b))
                                        .collect::<Vec<_>>()
                                        .join(" ")
                                } else {
                                    // show head ... tail to give context without overwhelming the tooltip
                                    let head = orig_slice
                                        .iter()
                                        .take(32)
                                        .map(|b| format!("{:02X}", b))
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    let tail = orig_slice
                                        .iter()
                                        .rev()
                                        .take(32)
                                        .cloned()
                                        .collect::<Vec<_>>();
                                    let tail = tail
                                        .into_iter()
                                        .rev()
                                        .map(|b| format!("{:02X}", b))
                                        .collect::<Vec<_>>()
                                        .join(" ");
                                    format!("{} ... {}", head, tail)
                                };

                                // ASCII-friendly representation (non-printable -> '.'). Truncate similar to hex.
                                let ascii_str = if len_bytes <= 64 {
                                    orig_slice
                                        .iter()
                                        .map(|&b| {
                                            if b.is_ascii_graphic() || b == b' ' {
                                                b as char
                                            } else {
                                                '.'
                                            }
                                        })
                                        .collect::<String>()
                                } else {
                                    let head = orig_slice
                                        .iter()
                                        .take(32)
                                        .map(|&b| {
                                            if b.is_ascii_graphic() || b == b' ' {
                                                b as char
                                            } else {
                                                '.'
                                            }
                                        })
                                        .collect::<String>();
                                    let tail = orig_slice
                                        .iter()
                                        .rev()
                                        .take(32)
                                        .cloned()
                                        .collect::<Vec<u8>>();
                                    let tail = tail
                                        .into_iter()
                                        .rev()
                                        .map(|b| {
                                            if b.is_ascii_graphic() || b == b' ' {
                                                b as char
                                            } else {
                                                '.'
                                            }
                                        })
                                        .collect::<String>();
                                    format!("{} ... {}", head, tail)
                                };

                                let label = String::from("Original:");

                                // Also prepare rebuilt bytes for the same range (if available in the viewer).
                                // Build per-byte hex and ASCII lines for both Original and Rebuilt so values
                                // appear aligned and it's obvious which bytes differ.
                                let mut rebuilt_present = false;
                                let mut rebuilt_hex = String::new();
                                let mut rebuilt_ascii = String::new();

                                if let Some(rb) = &self.rebuilt_bytes {
                                    // If rebuilt bytes are present, attempt to build a same-length slice
                                    // aligned to ds..=de. If rebuilt is shorter, show available bytes and
                                    // use '--' for missing bytes so it's clear it's absent.
                                    if ds <= de {
                                        // Determine how many bytes originally are considered
                                        let orig_len = (de.saturating_sub(ds)).saturating_add(1);
                                        // Build per-byte hex parts for rebuilt (or placeholder)
                                        let mut parts_hex: Vec<String> =
                                            Vec::with_capacity(orig_len);
                                        let mut parts_ascii: Vec<char> =
                                            Vec::with_capacity(orig_len);
                                        for i in 0..orig_len {
                                            let idx = ds.saturating_add(i);
                                            if idx < rb.len() {
                                                let b = rb[idx];
                                                parts_hex.push(format!("{:02X}", b));
                                                parts_ascii.push(
                                                    if b.is_ascii_graphic() || b == b' ' {
                                                        b as char
                                                    } else {
                                                        '.'
                                                    },
                                                );
                                            } else {
                                                // Indicate missing rebuilt byte clearly
                                                parts_hex.push(String::from("--"));
                                                parts_ascii.push('.');
                                            }
                                        }
                                        // Join into display strings (truncate with head...tail if too long)
                                        let reb_full = parts_hex.join(" ");
                                        if parts_hex.len() <= 64 {
                                            rebuilt_hex = reb_full;
                                        } else {
                                            let head = parts_hex
                                                .iter()
                                                .take(32)
                                                .cloned()
                                                .collect::<Vec<_>>()
                                                .join(" ");
                                            // Collect last up-to-32 hex parts and reverse back to original order.
                                            let mut tail_vec = parts_hex
                                                .iter()
                                                .rev()
                                                .take(32)
                                                .cloned()
                                                .collect::<Vec<_>>();
                                            tail_vec.reverse();
                                            let tail = tail_vec.join(" ");
                                            rebuilt_hex = format!("{} ... {}", head, tail);
                                        }
                                        let reb_ascii_full = parts_ascii.iter().collect::<String>();
                                        if reb_ascii_full.len() <= 64 {
                                            rebuilt_ascii = reb_ascii_full;
                                        } else {
                                            let head =
                                                reb_ascii_full.chars().take(32).collect::<String>();
                                            // Take last up-to-32 characters and restore original order.
                                            let mut tail_chars: Vec<char> =
                                                reb_ascii_full.chars().rev().take(32).collect();
                                            tail_chars.reverse();
                                            let tail = tail_chars.into_iter().collect::<String>();
                                            rebuilt_ascii = format!("{} ... {}", head, tail);
                                        }
                                        rebuilt_present = true;
                                    }
                                }

                                // Estimate tooltip size from text length and font metrics.
                                let char_w = self.font_size * 0.6_f32;
                                let padding_x = 6.0_f32;
                                let padding_y = 4.0_f32;
                                // Number of text lines to display: original label/hex/ascii and optional rebuilt label/hex/ascii
                                let lines_count = if rebuilt_present { 6u32 } else { 3u32 };
                                let max_line_len =
                                    hex_str.len().max(ascii_str.len()).max(label.len());
                                // If rebuilt present, consider rebuilt hex length as well for width
                                let max_line_len = if rebuilt_present {
                                    max_line_len.max(rebuilt_hex.len()).max(rebuilt_ascii.len())
                                } else {
                                    max_line_len
                                };
                                let desired_w = max_line_len as f32 * char_w + padding_x * 2.0;

                                let right_space = rect.max.x - (pos.x + 12.0) - 8.0;
                                let left_space = (pos.x - 12.0) - rect.min.x - 8.0;
                                let max_allowed = right_space.max(left_space).max(64.0);

                                let tip_w = desired_w.min(max_allowed).max(64.0);
                                // Compute tooltip height based on lines_count
                                let tip_h = (mono_text_height.max(self.font_size)
                                    * lines_count as f32)
                                    + padding_y * 2.0;

                                let mut tip_x = pos.x + 12.0;
                                let mut tip_y = pos.y + 12.0;
                                if tip_x + tip_w > rect.max.x {
                                    tip_x = (pos.x - 12.0 - tip_w).max(rect.min.x + 4.0);
                                }
                                if tip_y + tip_h > rect.max.y {
                                    tip_y = rect.max.y - tip_h - 4.0;
                                }

                                let tip_pos = egui::pos2(tip_x, tip_y);
                                let tip_rect =
                                    egui::Rect::from_min_size(tip_pos, egui::vec2(tip_w, tip_h));

                                // Draw tooltip background.
                                let bg = if ui.visuals().dark_mode {
                                    egui::Color32::from_rgb(50, 50, 52)
                                } else {
                                    ui.visuals().panel_fill
                                };
                                painter.rect_filled(tip_rect.expand(2.0), 4.0, bg);

                                // Vertical baseline for lines
                                let base_y = tip_rect.min.y + padding_y * 0.5;
                                // Draw Original label + hex + ascii
                                painter.text(
                                    egui::pos2(tip_rect.min.x + padding_x, base_y),
                                    egui::Align2::LEFT_TOP,
                                    label,
                                    egui::FontId::monospace(self.font_size),
                                    ui.visuals().text_color(),
                                );
                                painter.text(
                                    egui::pos2(
                                        tip_rect.min.x + padding_x,
                                        base_y + mono_text_height * 1.0,
                                    ),
                                    egui::Align2::LEFT_TOP,
                                    hex_str,
                                    egui::FontId::monospace(self.font_size),
                                    ui.visuals().text_color(),
                                );
                                painter.text(
                                    egui::pos2(
                                        tip_rect.min.x + padding_x,
                                        base_y + mono_text_height * 2.0,
                                    ),
                                    egui::Align2::LEFT_TOP,
                                    ascii_str,
                                    egui::FontId::monospace(self.font_size),
                                    ui.visuals().text_color(),
                                );

                                // If rebuilt bytes are available, draw them below the original block as paired lines.
                                if rebuilt_present {
                                    painter.text(
                                        egui::pos2(
                                            tip_rect.min.x + padding_x,
                                            base_y + mono_text_height * 3.0,
                                        ),
                                        egui::Align2::LEFT_TOP,
                                        String::from("Rebuilt:"),
                                        egui::FontId::monospace(self.font_size),
                                        ui.visuals().text_color(),
                                    );
                                    painter.text(
                                        egui::pos2(
                                            tip_rect.min.x + padding_x,
                                            base_y + mono_text_height * 4.0,
                                        ),
                                        egui::Align2::LEFT_TOP,
                                        rebuilt_hex,
                                        egui::FontId::monospace(self.font_size),
                                        ui.visuals().text_color(),
                                    );
                                    painter.text(
                                        egui::pos2(
                                            tip_rect.min.x + padding_x,
                                            base_y + mono_text_height * 5.0,
                                        ),
                                        egui::Align2::LEFT_TOP,
                                        rebuilt_ascii,
                                        egui::FontId::monospace(self.font_size),
                                        ui.visuals().text_color(),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        // If there was a pending scroll request (from next/prev or initial diff set),
        // compute the target rect and ask the UI to scroll so the diff/selection is visible.
        if let Some((scroll_s, scroll_e)) = self.pending_scroll_to.take() {
            // Capture and clear the align-top flag immediately so it does not
            // persist if scrolling fails or the target rect is empty.
            let align_top = self.pending_scroll_align_top;
            self.pending_scroll_align_top = false;

            let bytes_len = bytes.len();
            if bytes_len > 0 {
                let ss = scroll_s.min(bytes_len.saturating_sub(1));
                let ee = scroll_e.min(bytes_len.saturating_sub(1));
                let s_line = ss / bpl;
                let e_line = ee / bpl;
                let mut scroll_union: Option<egui::Rect> = None;
                for line in s_line..=e_line {
                    let line_top = rect.min.y + (line as f32) * row_height + 2.0;
                    let line_start = if line == s_line {
                        (ss % bpl) as f32
                    } else {
                        0.0
                    };
                    let line_end = if line == e_line {
                        (ee % bpl) as f32
                    } else {
                        (bpl as f32) - 1.0
                    };
                    let x0 = base_x + offset_width + line_start * hex_cell_w + 1.0;
                    let x1 = base_x + offset_width + (line_end + 1.0) * hex_cell_w - 1.0;
                    let y0 = line_top + 1.0;
                    let y1 = line_top + row_height - 4.0;
                    let seg = egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1));
                    scroll_union = Some(if let Some(u) = scroll_union {
                        u.union(seg)
                    } else {
                        seg
                    });
                }
                if let Some(target_rect) = scroll_union {
                    // Default: center the target rect.
                    ui.scroll_to_rect(target_rect, Some(egui::Align::Center));
                    // If requested, align the target rect to the top (Min).
                    if align_top {
                        ui.scroll_to_rect(target_rect, Some(egui::Align::Min));
                    }
                }
            }
        }

        // Right-pane click handling:
        // - Map a pointer click to the global byte index (if any)
        // - Update the viewer's single-byte selection so the UI reflects the click
        // - Publish the clicked byte index into egui memory (temp storage) under the Id
        //   \"hex_clicked_byte\" so the left-pane code (outside this module) can read it
        //   and focus the corresponding AST command if a mapping exists.
        //
        // This keeps the HexViewer self-contained while allowing the outer UI to react.
        if resp.clicked()
            && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
        {
            // Compute coordinates relative to the hex grid start.
            let rel_x = pos.x - (base_x + offset_width);
            let rel_y = pos.y - rect.min.y;
            if rel_x >= 0.0 && rel_y >= 0.0 {
                let line_idx = (rel_y / row_height).floor() as usize;
                let col_f = (rel_x / hex_cell_w).floor();
                if col_f >= 0.0 {
                    let col = col_f as usize;
                    let global_idx = line_idx.saturating_mul(bpl).saturating_add(col);
                    if global_idx < bytes.len() {
                        // Update selection state so the viewer highlights the clicked byte.
                        self.selected = Some(global_idx);
                        self.selection_range = Some((global_idx, global_idx));
                        self.reference_markers = vec![global_idx];
                        // Publish clicked byte into egui temporary memory so other UI
                        // code (left pane) can detect the click and focus the AST node.
                        // Record the clicked byte index locally; the outer UI can
                        // consume it via `HexViewer::take_last_clicked_byte()` to
                        // focus the corresponding AST node without using egui temp storage.
                        self.last_clicked_byte = Some(global_idx);
                        // Request repaint so the consumer of the memory key can observe
                        // and react in the same frame if desired.
                        ui.ctx().request_repaint();
                    }
                }
            }
        }
    }
}

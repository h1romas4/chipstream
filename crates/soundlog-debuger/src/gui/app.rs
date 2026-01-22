/*! Application wrapper moved out of `main.rs`.

This module provides the `Debuger` type which implements `eframe::App`.
It is intended to be used as `ui::Debuger` (see `src/ui.rs`).
*/

use std::cell::RefCell;

use eframe::egui;
use eframe::{CreationContext, Frame, NativeOptions};

use super::UiState;
use soundlog::VgmBuilder;
use soundlog::meta::Gd3;
use soundlog::vgm::command::WaitSamples;

/// Launch the GUI with the provided initial bytes.
///
/// This used to live in `main.rs`. It configures the native window options and
/// starts the `eframe` event loop with `ui::Debuger` as the application.
pub fn run_gui(initial_bytes: Vec<u8>) {
    // Configure native options: fix horizontal width to 1024 and allow vertical resizing.
    let native_options = NativeOptions {
        initial_window_size: Some(egui::vec2(1024.0, 800.0)),
        min_window_size: Some(egui::vec2(1024.0, 200.0)),
        max_window_size: Some(egui::vec2(1024.0, 5000.0)),
        ..NativeOptions::default()
    };

    // Launch native window, moving initial bytes into the closure.
    if let Err(err) = eframe::run_native(
        "soundlog debuger",
        native_options,
        Box::new(move |cc: &CreationContext| {
            Box::new(Debuger::new_with_bytes(cc, initial_bytes.clone()))
        }),
    ) {
        eprintln!("failed to launch native window: {:?}", err);
    }
}

/// Embedded application type for the native window.
///
/// The struct holds the UI state and implements `eframe::App`.
pub struct Debuger {
    pub state: RefCell<UiState>,
}

impl Debuger {
    /// Create the application and set initial bytes into the UI state.
    pub fn new_with_bytes(cc: &CreationContext, initial_bytes: Vec<u8>) -> Self {
        // Increase UI scaling by 1.2x for better readability.
        let ctx = &cc.egui_ctx;
        let current = ctx.pixels_per_point();
        ctx.set_pixels_per_point(current * 1.2);

        // Initialize UI state: if we have initial bytes, populate AST from them;
        // otherwise construct an empty VGM using `VgmBuilder` and parse that so
        // the UI displays a real (empty) VGM document instead of purely
        // synthetic placeholders.
        let state = if initial_bytes.is_empty() {
            // Build an empty VGM document and serialize to bytes.
            let mut builder = VgmBuilder::new();
            // Add a single small wait command so a command bucket appears in the AST.
            builder.add_vgm_command(WaitSamples(1));

            // Include minimal GD3 metadata so the GUI shows metadata fields.
            builder.set_gd3(Gd3 {
                track_name_en: Some("Untitled".to_string()),
                game_name_en: Some("Empty VGM".to_string()),
                author_name_en: Some("soundlog-gui".to_string()),
                notes: Some("Automatically generated empty VGM".to_string()),
                ..Default::default()
            });

            let doc = builder.finalize();
            let bytes: Vec<u8> = (&doc).into();

            let mut s = UiState::new_empty();
            s.populate_from_bytes(&bytes);
            s
        } else {
            let mut s = UiState::new_empty();
            s.populate_from_bytes(&initial_bytes);
            s
        };

        Self {
            state: RefCell::new(state),
        }
    }
}

impl eframe::App for Debuger {
    // Called each frame to update the UI.
    fn update(&mut self, ctx: &egui::Context, frame: &mut Frame) {
        // Defer to the UI module's `show_ui` function to render everything.
        // `show_ui` is exported from the parent (`ui`) module, so refer to it
        // via `super::show_ui`.
        super::show_ui(&mut self.state.borrow_mut(), ctx, frame);
    }
}

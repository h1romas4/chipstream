//! Minimal `eframe`/`egui` application that composes the UI module.
//!
//! This file wires the UI components into the app. The UI module provides a
//! left AST pane and a right hex viewer; here we initialize the placeholder
//! state and call into the module each frame.

use eframe::egui;
use eframe::{CreationContext, Frame, NativeOptions};
use std::cell::RefCell;
mod ui;
mod vgm;
use soundlog::VgmBuilder;
use soundlog::meta::Gd3;
use soundlog::vgm::command::WaitSamples;
use ui::UiState;

// CLI parsing and file handling
use clap::Parser;
use flate2::read::GzDecoder;
use std::fs;
use std::io::Cursor;
use std::io::Read;
use std::path::PathBuf;

/// Simple CLI: optional file path to display
#[derive(Parser, Debug)]
#[command(about = "soundlog tools")]
struct Args {
    /// Path to binary file to display (supports .vgz (gzipped) and raw files)
    file: Option<PathBuf>,
}

fn main() {
    // Parse CLI args early so we can load the initial bytes before creating the UI.
    let args = Args::parse();

    // Try to load bytes from the provided file, otherwise keep empty vector.
    let mut initial_bytes: Vec<u8> = Vec::new();
    if let Some(path) = args.file {
        match fs::read(&path) {
            Ok(data) => {
                // Detect gzip by extension or by header (0x1f 0x8b)
                let is_gzip = path
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.eq_ignore_ascii_case("vgz") || s.eq_ignore_ascii_case("gz"))
                    .unwrap_or(false)
                    || (data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b);

                if is_gzip {
                    // Decompress in memory
                    let mut decoder = GzDecoder::new(Cursor::new(data));
                    let mut out = Vec::new();
                    match decoder.read_to_end(&mut out) {
                        Ok(_) => {
                            initial_bytes = out;
                        }
                        Err(e) => {
                            eprintln!("gzip decompression failed: {:?}", e);
                        }
                    }
                } else {
                    initial_bytes = data;
                }
            }
            Err(e) => {
                eprintln!("failed to read file: {:?}", e);
            }
        }
    }

    // Configure native options: fix horizontal width to 1024 and allow vertical resizing.
    let native_options = NativeOptions {
        initial_window_size: Some(egui::vec2(1024.0, 800.0)),
        min_window_size: Some(egui::vec2(1024.0, 200.0)),
        max_window_size: Some(egui::vec2(1024.0, 5000.0)),
        ..NativeOptions::default()
    };

    // Launch the native window. Move initial_bytes into the closure so MyApp can consume it.
    if let Err(err) = eframe::run_native(
        "soundlog debuger",
        native_options,
        Box::new(move |cc: &CreationContext| {
            Box::new(MyApp::new_with_bytes(cc, initial_bytes.clone()))
        }),
    ) {
        eprintln!("failed to launch native window: {:?}", err);
    }
}

struct MyApp {
    state: RefCell<UiState>,
}

impl MyApp {
    /// Create the application and set initial bytes into the UI state.
    fn new_with_bytes(cc: &CreationContext, initial_bytes: Vec<u8>) -> Self {
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

impl eframe::App for MyApp {
    // Called each frame to update the UI.
    fn update(&mut self, ctx: &egui::Context, frame: &mut Frame) {
        // Borrow the UI state mutably for the duration of the call to avoid
        // simultaneous immutable/mutable borrows inside the ui module.
        ui::show_ui(&mut self.state.borrow_mut(), ctx, frame);
    }
}

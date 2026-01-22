//! Minimal `eframe`/`egui` application that composes the UI module.
//!
//! This file wires the UI components into the app. The UI module provides a
//! left AST pane and a right hex viewer; here we initialize the placeholder
//! state and call into the module each frame.

mod cui;
mod gui;

// CLI parsing and file handling
use anyhow::Context;
use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;

/// Simple CLI: optional subcommand `test`, otherwise optional file path to display
#[derive(Subcommand, Debug)]
enum Commands {
    /// Run in test / headless mode
    Test {
        /// Path to binary file to test (use '-' for stdin)
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Print detailed diagnostics on mismatch
        #[arg(long)]
        diag: bool,
    },
}

#[derive(Parser, Debug)]
#[command(about = "soundlog tools")]
struct Args {
    /// Subcommand to run (e.g., `test`)
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to binary file to display (supports .vgz (gzipped) and raw files)
    file: Option<PathBuf>,
}

/// Helper: read bytes from a path, automatically handling `.vgz`/`.gz` or gzip header.
///
/// This centralizes the logic used both by the `test` subcommand and by the GUI
/// loader so the detection/decompression implementation isn't duplicated.
fn load_bytes_from_path(path: &PathBuf) -> anyhow::Result<Vec<u8>> {
    // Read file contents
    let data =
        fs::read(path).with_context(|| format!("failed to read file: {}", path.display()))?;

    // Detect gzip by extension or by header (0x1f 0x8b)
    let is_gzip = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.eq_ignore_ascii_case("vgz") || s.eq_ignore_ascii_case("gz"))
        .unwrap_or(false)
        || (data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b);

    if is_gzip {
        let mut decoder = GzDecoder::new(Cursor::new(data));
        let mut out = Vec::new();
        decoder
            .read_to_end(&mut out)
            .context("gzip decompression failed")?;
        Ok(out)
    } else {
        Ok(data)
    }
}

/// GUI runner moved to `ui::run_gui`.
///
/// The function used to live in this file but has been moved into the UI
fn main() {
    // Parse CLI args early so we can load the initial bytes before creating the UI.
    let args = Args::parse();

    // If the user requested the `test` subcommand, run the roundtrip test and exit.
    if let Some(Commands::Test { file, diag }) = args.command {
        // Read the input using the same logic as the GUI code path:
        // read raw bytes, detect gz/vgz by extension or header and decompress.
        match load_bytes_from_path(&file) {
            Ok(bytes) => match crate::cui::vgm::test_roundtrip(&file, bytes, diag) {
                Ok(_) => std::process::exit(0),
                Err(e) => {
                    eprintln!("test_roundtrip failed: {}", e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("failed to read input for test: {}", e);
                std::process::exit(1);
            }
        }
    }

    // Try to load bytes from the provided file, otherwise keep empty vector.
    let mut initial_bytes: Vec<u8> = Vec::new();
    if let Some(path) = args.file {
        match load_bytes_from_path(&path) {
            Ok(data) => initial_bytes = data,
            Err(e) => eprintln!("failed to read file: {}", e),
        }
    }

    // Launch GUI in a separate function (implementation moved to `ui::run_gui`).
    gui::run_gui(initial_bytes);
}

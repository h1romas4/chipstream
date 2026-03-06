//! Minimal `eframe`/`egui` application that composes the UI module.
//!
//! This file wires the UI components into the app. The UI module provides a
//! left AST pane and a right hex viewer; here we initialize the placeholder
//! state and call into the module each frame.

mod cui;
mod gui;
mod logger;

// CLI parsing and file handling
use anyhow::Context;
use clap::{Parser, Subcommand};
use flate2::read::GzDecoder;
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use crate::logger::Logger;
use std::sync::Arc;

/// Simple CLI: optional subcommand `test`, otherwise optional file path to display
#[derive(Subcommand, Debug)]
enum Commands {
    /// Run in test / headless mode
    Test {
        /// Path to binary file to test (use '-' for stdin)
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Dry-run: do not print standard one-line outputs; only emit errors/panics
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-dump VGM file with DAC streams expanded to chip writes
    Redump {
        /// Input VGM file path
        #[arg(value_name = "INPUT")]
        input: PathBuf,

        /// Output VGM file path (use '-' for stdout)
        #[arg(value_name = "OUTPUT")]
        output: PathBuf,

        /// Print diagnostic output after redump (re-parse output and show diagnostics)
        #[arg(long)]
        diag: bool,
    },
    /// Parse and display VGM file commands with offsets and lengths
    Parse {
        /// VGM file path to parse
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Play VGM file and display register writes with events
    Play {
        /// VGM file path to play
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Dry-run mode: process the file without printing output (only errors/panics)
        #[arg(long)]
        dry_run: bool,

        /// Loop count limit (default: 1 when unspecified — play once).
        /// Pass an explicit value to override (e.g. `--loop-count 2` to play twice).
        #[arg(long)]
        loop_count: Option<u32>,

        /// VGM loop_modifier override (0 = use file default; see VGM spec §loop_modifier)
        #[arg(long)]
        loop_modifier: Option<u8>,

        /// VGM loop_base override (see VGM spec §loop_base)
        #[arg(long)]
        loop_base: Option<i8>,
    },
}

#[derive(Parser, Debug)]
#[command(
    name = "soundlog",
    author = env!("CARGO_PKG_AUTHORS"),
    version = env!("CARGO_PKG_VERSION"),
    about = env!("CARGO_PKG_DESCRIPTION"),
)]
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
    // Create a default logger; some subcommands will override this based on their dry_run flags.
    let mut logger = Arc::new(Logger::new_stdout(false));

    // Handle subcommands
    match args.command {
        Some(Commands::Test { file, dry_run }) => {
            // Read the input using the same logic as the GUI code path:
            // read raw bytes, detect gz/vgz by extension or header and decompress.
            // The underlying `test_roundtrip` accepts a `dry_run: bool` which
            // suppresses standard one-line and diagnostic output when true.
            // Configure logger according to dry_run so main's messages respect it.
            logger = Arc::new(Logger::new_stdout(dry_run));
            // Pass `dry_run` through directly so that `--dry-run` results in no
            // normal/stdout output from the test helper.
            match load_bytes_from_path(&file) {
                Ok(bytes) => match crate::cui::vgm::test_roundtrip(&file, bytes, dry_run) {
                    Ok(_) => std::process::exit(0),
                    Err(e) => {
                        log_error!(&*logger, "test_roundtrip failed: {}", e);
                        std::process::exit(1);
                    }
                },
                Err(e) => {
                    log_error!(&*logger, "failed to read input for test: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Redump {
            input,
            output,
            diag,
        }) => {
            // Load input bytes
            match load_bytes_from_path(&input) {
                Ok(bytes) => {
                    // Call redump_vgm (preserves original loop and fadeout information from the file)
                    match crate::cui::vgm::redump_vgm(&input, &output, bytes, diag) {
                        Ok(_) => {
                            // redump succeeded; diagnostics (if diag) are produced inside `redump_vgm`.
                            // Exit success here.
                            std::process::exit(0);
                        }
                        Err(e) => {
                            log_error!(&*logger, "redump failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    log_error!(&*logger, "failed to read input for redump: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Parse { file }) => {
            // Load file
            match load_bytes_from_path(&file) {
                Ok(bytes) => {
                    // Call parse_vgm (pass logger Arc so the parse path can use centralized logging)
                    match crate::cui::vgm::parse_vgm(&file, bytes, logger.clone()) {
                        Ok(_) => {
                            std::process::exit(0);
                        }
                        Err(e) => {
                            log_error!(&*logger, "parse failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    log_error!(&*logger, "failed to read file: {}", e);
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Play {
            file,
            dry_run,
            loop_count,
            loop_modifier,
            loop_base,
        }) => {
            // Load file
            // Configure logger according to dry_run so main-level messages respect it.
            logger = Arc::new(Logger::new_stdout(dry_run));
            match load_bytes_from_path(&file) {
                Ok(bytes) => {
                    // Default loop_count to Some(1) when unspecified
                    let loop_count = loop_count.or(Some(1));
                    // Call play_vgm
                    match crate::cui::play::play_vgm(
                        &file,
                        bytes,
                        logger.clone(),
                        loop_count,
                        loop_modifier,
                        loop_base,
                    ) {
                        Ok(_) => {
                            std::process::exit(0);
                        }
                        Err(e) => {
                            log_error!(&*logger, "play failed: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    log_error!(&*logger, "failed to read file: {}", e);
                    std::process::exit(1);
                }
            }
        }
        None => {}
    }

    // Try to load bytes from the provided file, otherwise keep empty vector.
    let mut initial_bytes: Vec<u8> = Vec::new();
    if let Some(path) = args.file {
        match load_bytes_from_path(&path) {
            Ok(data) => initial_bytes = data,
            Err(e) => log_error!(&logger, "failed to read file: {}", e),
        }
    }

    // Launch GUI in a separate function (implementation moved to `ui::run_gui`).
    gui::run_gui(initial_bytes);
}

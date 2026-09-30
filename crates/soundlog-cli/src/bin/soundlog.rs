//! Command-line debugger for VGM and MDX files.

use anyhow::Context;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};
use flate2::read::GzDecoder;
use std::fs;
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::process;
use std::sync::Arc;

// Use the library crate's CUI and logger.
use soundlog::mdx::convert::{AdpcmMode, MdxToVgmOptions};
use soundlog_cli::cui;
use soundlog_cli::logger::Logger;

#[derive(Clone, Copy, Debug, ValueEnum)]
enum AdpcmModeArg {
    Through,
    Resample,
    Lpf,
}

impl From<AdpcmModeArg> for AdpcmMode {
    fn from(mode: AdpcmModeArg) -> Self {
        match mode {
            AdpcmModeArg::Through => Self::Through,
            AdpcmModeArg::Resample => Self::Resample,
            AdpcmModeArg::Lpf => Self::Lpf,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum MmlOutputFormat {
    Mdx,
    Vgm,
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum PdxExportFormatArg {
    #[default]
    Raw,
    Wav,
}

impl From<MmlOutputFormat> for soundlog_cli::cui::mdx::OutputFormat {
    fn from(format: MmlOutputFormat) -> Self {
        match format {
            MmlOutputFormat::Mdx => Self::Mdx,
            MmlOutputFormat::Vgm => Self::Vgm,
        }
    }
}

/// Command-line operations for inspecting and converting sound files.
#[derive(Subcommand, Debug)]
enum Commands {
    /// Test a VGM or VGZ file with a parse/build round trip and display its header
    Test {
        /// VGM or VGZ input file path (use '-' for stdin)
        #[arg(value_name = "VGM_FILE")]
        file: PathBuf,

        /// Dry-run: do not print standard one-line outputs; only emit errors/panics
        #[arg(long)]
        dry_run: bool,
    },
    /// Re-dump a VGM or VGZ file, expanding DAC streams to chip writes
    Redump {
        /// Input VGM or VGZ file path
        #[arg(value_name = "INPUT_VGM")]
        input: PathBuf,

        /// Output VGM file path (use '-' for stdout)
        #[arg(value_name = "OUTPUT_VGM")]
        output: PathBuf,

        /// Print diagnostic output after redump (re-parse output and show diagnostics)
        #[arg(long)]
        diag: bool,
    },
    /// Parse a VGM or VGZ file and display its commands, offsets, and lengths
    Parse {
        /// VGM or VGZ file path to parse
        #[arg(value_name = "VGM_FILE")]
        file: PathBuf,
    },
    /// Generate shell completion scripts
    Completions {
        #[arg(value_enum)]
        shell: Shell,
    },
    /// Stream a VGM or VGZ file and display its register writes and detected events
    Stream {
        /// VGM or VGZ file path to stream
        #[arg(value_name = "VGM_FILE")]
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
    /// MDX file operations
    Mdx {
        #[command(subcommand)]
        command: MdxCommands,
    },
    /// PDX file operations
    Pdx {
        #[command(subcommand)]
        command: PdxCommands,
    },
}

#[derive(Subcommand, Debug)]
enum MdxCommands {
    /// Parse, compile, and check finite lazy playback of MML without loading PDX
    ///
    /// Uses synthetic PCM samples, not actual PDX data. Runtime diagnostics
    /// include MML source locations when available and zero-based MDX indices.
    Check {
        /// MML source file to parse, or '-' to read from stdin
        #[arg(value_name = "MML_FILE")]
        input: PathBuf,

        /// Read source from stdin while using MML_FILE for diagnostic locations
        #[arg(long)]
        stdin: bool,

        /// Print the parsed MML syntax tree
        #[arg(short, long)]
        verbose: bool,

        /// Only parse MML, skipping compilation and playback
        #[arg(long)]
        parse_only: bool,

        /// Number of whole-song playthroughs to check
        #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..), conflicts_with = "parse_only")]
        loop_count: u32,

        /// Maximum playback ticks; reaching this limit reports an incomplete check
        #[arg(long, default_value_t = 100_000, value_parser = clap::value_parser!(u64).range(1..), conflicts_with = "parse_only")]
        max_ticks: u64,

        /// Maximum executed MDX commands, including commands within a single tick
        #[arg(long, default_value_t = 1_000_000, value_parser = clap::value_parser!(u64).range(1..), conflicts_with = "parse_only")]
        max_commands: u64,
    },
    /// Compile an MML source file into an MDX or VGM binary file
    Compile {
        /// MML source file to parse
        #[arg(value_name = "MML_FILE")]
        input: PathBuf,

        /// Output MDX or VGM file path
        #[arg(value_name = "OUTPUT_FILE")]
        output: PathBuf,

        /// Output format. VGM output uses default settings with native looping (same as `mdx convert --native-loop`).
        #[arg(long, value_enum, default_value_t = MmlOutputFormat::Mdx)]
        output_format: MmlOutputFormat,

        /// ADPCM processing mode for VGM output
        #[arg(long, value_enum, default_value_t = AdpcmModeArg::Through)]
        adpcm_mode: AdpcmModeArg,

        /// Skip bounded playback validation for MDX output (VGM conversion still reports playback errors)
        #[arg(long)]
        no_playback_check: bool,
    },
    /// Parse an MDX or MML file and display its track commands
    Parse {
        /// MDX or MML input file path
        #[arg(value_name = "MDX_OR_MML_FILE")]
        input: PathBuf,

        /// Optional PDX file to parse alongside an MDX input
        #[arg(long, value_name = "PDX_FILE")]
        pdx: Option<PathBuf>,
    },
    /// Convert an MDX file and verify that the generated VGM parses
    Test {
        /// MDX input file path
        #[arg(value_name = "MDX_FILE")]
        input: PathBuf,

        /// Optional PDX file used for PCM references
        #[arg(long, value_name = "PDX_FILE")]
        pdx: Option<PathBuf>,

        /// Dry-run mode: process the file without printing diagnostics
        #[arg(long)]
        dry_run: bool,

        /// YM2151 clock in Hz
        #[arg(long, default_value_t = 4_000_000)]
        ym2151_clock: u32,

        /// OKIM6258 clock in Hz (only used for files with PCM8/PCM8A tracks)
        #[arg(long, default_value_t = soundlog::mdx::pcm_mixer::PCM8_RECOMMENDED_OKIM6258_CLOCK_HZ)]
        okim6258_clock: u32,

        /// Number of whole-song playthroughs; COUNT follows VGM semantics (0 means 1)
        #[arg(long, value_name = "COUNT")]
        loop_count: Option<u32>,

        /// ADPCM mode: through, resample, or lpf (default: through)
        #[arg(long, value_enum, default_value_t = AdpcmModeArg::Through)]
        adpcm_mode: AdpcmModeArg,
    },
    /// Convert an MDX file to a VGM file
    Convert {
        /// MDX input file path
        #[arg(value_name = "MDX_FILE")]
        input: PathBuf,

        /// VGM output file path
        #[arg(value_name = "VGM_FILE")]
        output: PathBuf,

        /// Optional PDX file used for PCM references
        #[arg(long, value_name = "PDX_FILE")]
        pdx: Option<PathBuf>,

        /// YM2151 clock in Hz
        #[arg(long, default_value_t = 4_000_000)]
        ym2151_clock: u32,

        /// OKIM6258 clock in Hz
        #[arg(long, default_value_t = soundlog::mdx::pcm_mixer::PCM8_RECOMMENDED_OKIM6258_CLOCK_HZ)]
        okim6258_clock: u32,

        /// Number of whole-song playthroughs to write (default: 1; 0 means 1; no VGM loop point)
        #[arg(long, value_name = "COUNT", conflicts_with = "native_loop")]
        loop_count: Option<u32>,

        /// Use a native VGM loop point instead of a finite loop count.
        /// Sets VGM header loop_offset and loop_samples when a loop is detected.
        /// The estimated loop point may be inaccurate for per-track MDX F1 loops.
        #[arg(long, conflicts_with = "loop_count")]
        native_loop: bool,

        /// ADPCM mode: through, resample, or lpf
        #[arg(long, value_enum, default_value_t = AdpcmModeArg::Through)]
        adpcm_mode: AdpcmModeArg,
    },
    /// Convert an MDX or MML file lazily to a command stream and print register writes
    /// and events in the same format as `soundlog stream`
    Stream {
        /// MDX or MML input file path
        #[arg(value_name = "MDX_OR_MML_FILE")]
        input: PathBuf,

        /// Optional PDX file used for PCM references
        #[arg(long, value_name = "PDX_FILE")]
        pdx: Option<PathBuf>,

        /// Dry-run mode: process the file without printing output (only errors/panics)
        #[arg(long)]
        dry_run: bool,

        /// YM2151 clock in Hz
        #[arg(long, default_value_t = 4_000_000)]
        ym2151_clock: u32,

        /// OKIM6258 clock in Hz (only used for files with PCM8/PCM8A tracks;
        /// see `MdxCommands::Test`'s `okim6258_clock`)
        #[arg(long, default_value_t = soundlog::mdx::pcm_mixer::PCM8_RECOMMENDED_OKIM6258_CLOCK_HZ)]
        okim6258_clock: u32,

        /// Number of whole-song playthroughs (default: 1; COUNT follows VGM semantics, 0 means 1)
        #[arg(long, value_name = "COUNT")]
        loop_count: Option<u32>,

        /// ADPCM mode: through, resample, or lpf (default: through)
        #[arg(long, value_enum, default_value_t = AdpcmModeArg::Through)]
        adpcm_mode: AdpcmModeArg,
    },
}

#[derive(Subcommand, Debug)]
enum PdxCommands {
    /// Validate a PDX file and display its banks and sample allocation
    Test {
        /// PDX input file
        #[arg(value_name = "PDX_FILE")]
        input: PathBuf,

        /// Dry-run: validate the file without printing its summary
        #[arg(long)]
        dry_run: bool,
    },
    /// Convert WAV/raw samples to ADPCM as needed and write a PDX file
    #[command(override_usage = "soundlog pdx build [OPTIONS] <INPUT_WAV_OR_RAW>... <OUTPUT_PDX>")]
    Build {
        /// Input WAV/raw samples followed by the output PDX path
        #[arg(value_name = "INPUT_WAV_OR_RAW_OR_OUTPUT_PDX", num_args = 2..)]
        files: Vec<PathBuf>,

        /// Print detailed input and output information
        #[arg(short = 'v', long)]
        verbose: bool,

        /// Skip 16-bit WAV PCM scaling; the ADPCM encoder still clamps to signed 12-bit
        #[arg(long)]
        disable_12bit_conversion: bool,

        /// Store the PDX payload using LZ compression
        #[arg(long)]
        enable_lz: bool,
    },
    /// Export PDX samples as raw ADPCM bytes or mono WAV files
    ///
    /// LZ-compressed PDX input is decompressed first. Raw output contains the
    /// original ADPCM sample bytes, not the PDX-level LZ stream.
    Export {
        /// Input PDX file
        #[arg(value_name = "INPUT_PDX")]
        input: PathBuf,

        /// Directory for exported sample files
        #[arg(value_name = "OUTPUT_DIR")]
        output_dir: PathBuf,

        /// Sample rate for WAV output (ignored for raw output)
        #[arg(long, default_value_t = 15_625, value_name = "HZ")]
        sample_rate: u32,

        /// Export samples as raw ADPCM bytes or decoded WAV audio
        #[arg(long, value_enum, default_value_t = PdxExportFormatArg::Raw)]
        output_format: PdxExportFormatArg,
        /// Print detailed information about exported samples
        #[arg(short = 'v', long)]
        verbose: bool,
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
    /// Command to run.
    #[command(subcommand)]
    command: Commands,
}

/// Read bytes from a path, automatically handling `.vgz`/`.gz` or gzip headers.
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

/// Entry point.
///
/// This binary uses the library crate's CUI modules and logging macros.
fn main() {
    let args = Args::parse();
    // Create a default logger; some subcommands will override this based on their dry_run flags.
    let mut logger = Arc::new(Logger::new_stdout(false));

    // Handle subcommands
    match args.command {
        Commands::Mdx { command } => match command {
            MdxCommands::Check {
                input,
                stdin,
                verbose,
                parse_only,
                loop_count,
                max_ticks,
                max_commands,
            } => match cui::mdx::check_with_options(
                &input,
                verbose,
                stdin,
                cui::mdx::CheckOptions {
                    parse_only,
                    loop_count,
                    limits: soundlog::mdx::convert::MdxPlaybackCheckLimits {
                        max_ticks,
                        max_commands,
                    },
                },
            ) {
                Ok(()) => process::exit(0),
                Err(error) => {
                    soundlog_cli::log_error!(&*logger, "{error:#}");
                    process::exit(1);
                }
            },
            MdxCommands::Compile {
                input,
                output,
                output_format,
                adpcm_mode,
                no_playback_check,
            } => {
                match cui::mdx::compile_with_playback_check(
                    &input,
                    &output,
                    output_format.into(),
                    adpcm_mode.into(),
                    !no_playback_check,
                ) {
                    Ok(()) => process::exit(0),
                    Err(error) => {
                        soundlog_cli::log_error!(&*logger, "{error:#}");
                        process::exit(1);
                    }
                }
            }
            MdxCommands::Parse { input, pdx } => {
                match cui::mdx::parse_mdx(&input, pdx.as_deref(), logger.clone()) {
                    Ok(()) => process::exit(0),
                    Err(error) => {
                        soundlog_cli::log_error!(&*logger, "{error:#}");
                        process::exit(1);
                    }
                }
            }
            MdxCommands::Test {
                input,
                pdx,
                dry_run,
                ym2151_clock,
                okim6258_clock,
                loop_count,
                adpcm_mode,
            } => {
                logger = Arc::new(Logger::new_stdout(dry_run));
                let options = MdxToVgmOptions {
                    ym2151_clock,
                    okim6258_clock,
                    loop_count,
                    adpcm_mode: adpcm_mode.into(),
                };
                match cui::mdx::test_mdx(&input, pdx.as_deref(), logger.clone(), &options) {
                    Ok(()) => process::exit(0),
                    Err(error) => {
                        soundlog_cli::log_error!(&*logger, "mdx test failed: {}", error);
                        process::exit(1);
                    }
                }
            }
            MdxCommands::Convert {
                input,
                output,
                pdx,
                ym2151_clock,
                okim6258_clock,
                loop_count,
                native_loop,
                adpcm_mode,
            } => {
                let options = MdxToVgmOptions {
                    ym2151_clock,
                    okim6258_clock,
                    loop_count: if native_loop {
                        None
                    } else {
                        Some(loop_count.unwrap_or(1))
                    },
                    adpcm_mode: adpcm_mode.into(),
                };
                match cui::mdx::convert_mdx(&input, &output, pdx.as_deref(), &options) {
                    Ok(()) => process::exit(0),
                    Err(error) => {
                        soundlog_cli::log_error!(&*logger, "mdx convert failed: {}", error);
                        process::exit(1);
                    }
                }
            }
            MdxCommands::Stream {
                input,
                pdx,
                dry_run,
                ym2151_clock,
                okim6258_clock,
                loop_count,
                adpcm_mode,
            } => {
                // Configure logger according to dry_run so main's messages respect it.
                logger = Arc::new(Logger::new_stdout(dry_run));
                let loop_count = loop_count.or(Some(1));
                let options = MdxToVgmOptions {
                    ym2151_clock,
                    okim6258_clock,
                    loop_count,
                    adpcm_mode: adpcm_mode.into(),
                };
                match cui::mdx::stream_mdx(&input, pdx.as_deref(), logger.clone(), &options) {
                    Ok(()) => process::exit(0),
                    Err(error) => {
                        soundlog_cli::log_error!(&*logger, "{error:#}");
                        process::exit(1);
                    }
                }
            }
        },
        Commands::Pdx {
            command: PdxCommands::Test { input, dry_run },
        } => {
            logger = Arc::new(Logger::new_stdout(dry_run));
            match cui::pdx::test_pdx(&input, logger.clone()) {
                Ok(()) => process::exit(0),
                Err(error) => {
                    soundlog_cli::log_error!(&*logger, "PDX test failed: {error:#}");
                    process::exit(1);
                }
            }
        }
        Commands::Pdx {
            command:
                PdxCommands::Build {
                    files,
                    verbose,
                    disable_12bit_conversion,
                    enable_lz,
                },
        } => match cui::pdx::build_pdx_with_report(&files, enable_lz, disable_12bit_conversion) {
            Ok(report) => {
                if verbose {
                    let stdout_logger = Logger::new_stdout(false);
                    if let Err(error) = cui::pdx::log_build_report(&report, &stdout_logger) {
                        let stderr_logger = Logger::new_stderr(false);
                        soundlog_cli::log_error!(
                            &stderr_logger,
                            "failed to write PDX build report: {error:#}"
                        );
                        process::exit(1);
                    }
                }
                process::exit(0);
            }
            Err(error) => {
                let stderr_logger = Logger::new_stderr(false);
                soundlog_cli::log_error!(&stderr_logger, "PDX build failed: {error:#}");
                process::exit(1);
            }
        },
        Commands::Pdx {
            command:
                PdxCommands::Export {
                    input,
                    output_dir,
                    sample_rate,
                    output_format,
                    verbose,
                },
        } => match cui::pdx::export_pdx_with_report(
            &input,
            &output_dir,
            sample_rate,
            match output_format {
                PdxExportFormatArg::Raw => cui::pdx::PdxExportFormat::Raw,
                PdxExportFormatArg::Wav => cui::pdx::PdxExportFormat::Wav,
            },
        ) {
            Ok(report) => {
                if verbose {
                    let stdout_logger = Logger::new_stdout(false);
                    if let Err(error) = cui::pdx::log_export_report(&report, &stdout_logger) {
                        let stderr_logger = Logger::new_stderr(false);
                        soundlog_cli::log_error!(
                            &stderr_logger,
                            "failed to write PDX export report: {error:#}"
                        );
                        process::exit(1);
                    }
                }
                process::exit(0);
            }
            Err(error) => {
                let stderr_logger = Logger::new_stderr(false);
                soundlog_cli::log_error!(&stderr_logger, "PDX export failed: {error:#}");
                process::exit(1);
            }
        },
        Commands::Test { file, dry_run } => {
            // Configure logger according to dry_run so main's messages respect it.
            logger = Arc::new(Logger::new_stdout(dry_run));
            // Pass `dry_run` through directly so that `--dry-run` results in no normal/stdout output
            match load_bytes_from_path(&file) {
                Ok(bytes) => {
                    match cui::vgm::test_roundtrip(&file, bytes, dry_run) {
                        Ok(_) => process::exit(0),
                        Err(e) => {
                            // Qualify macro with crate name so the exported macro is resolved.
                            soundlog_cli::log_error!(&*logger, "test_roundtrip failed: {}", e);
                            process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    soundlog_cli::log_error!(&*logger, "failed to read input for test: {}", e);
                    process::exit(1);
                }
            }
        }
        Commands::Redump {
            input,
            output,
            diag,
        } => {
            // Load input bytes
            match load_bytes_from_path(&input) {
                Ok(bytes) => {
                    // Call redump_vgm (preserves original loop and fadeout information from the file)
                    match cui::vgm::redump_vgm(&input, &output, bytes, diag) {
                        Ok(_) => {
                            // redump succeeded; diagnostics (if diag) are produced inside `redump_vgm`.
                            process::exit(0);
                        }
                        Err(e) => {
                            soundlog_cli::log_error!(&*logger, "redump failed: {}", e);
                            process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    soundlog_cli::log_error!(&*logger, "failed to read input for redump: {}", e);
                    process::exit(1);
                }
            }
        }
        Commands::Parse { file } => {
            // Load file
            match load_bytes_from_path(&file) {
                Ok(bytes) => {
                    // Call parse_vgm (pass logger Arc so the parse path can use centralized logging)
                    match cui::vgm::parse_vgm(&file, bytes, logger.clone()) {
                        Ok(_) => {
                            process::exit(0);
                        }
                        Err(e) => {
                            soundlog_cli::log_error!(&*logger, "parse failed: {}", e);
                            process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    soundlog_cli::log_error!(&*logger, "failed to read file: {}", e);
                    process::exit(1);
                }
            }
        }
        Commands::Completions { shell } => {
            let mut command = Args::command();
            let mut stdout = std::io::stdout();
            generate(shell, &mut command, "soundlog", &mut stdout);
            process::exit(0);
        }
        Commands::Stream {
            file,
            dry_run,
            loop_count,
            loop_modifier,
            loop_base,
        } => {
            // Configure logger according to dry_run so main-level messages respect it.
            logger = Arc::new(Logger::new_stdout(dry_run));
            match load_bytes_from_path(&file) {
                Ok(bytes) => {
                    // Default loop_count to Some(1) when unspecified
                    let loop_count = loop_count.or(Some(1));
                    // Process the VGM stream.
                    match cui::stream::stream_vgm(
                        &file,
                        bytes,
                        logger.clone(),
                        loop_count,
                        loop_modifier,
                        loop_base,
                    ) {
                        Ok(_) => {
                            process::exit(0);
                        }
                        Err(e) => {
                            soundlog_cli::log_error!(&*logger, "stream failed: {}", e);
                            process::exit(1);
                        }
                    }
                }
                Err(e) => {
                    soundlog_cli::log_error!(&*logger, "failed to read file: {}", e);
                    process::exit(1);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mdx_compile_playback_check_is_enabled_by_default_and_can_be_skipped() {
        let args =
            Args::try_parse_from(["soundlog", "mdx", "compile", "song.mml", "song.mdx"]).unwrap();
        assert!(matches!(
            args.command,
            Commands::Mdx {
                command: MdxCommands::Compile {
                    no_playback_check: false,
                    ..
                }
            }
        ));
        let args = Args::try_parse_from([
            "soundlog",
            "mdx",
            "compile",
            "song.mml",
            "song.mdx",
            "--no-playback-check",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Commands::Mdx {
                command: MdxCommands::Compile {
                    no_playback_check: true,
                    ..
                }
            }
        ));
    }

    /// Checks default playback settings and parse-only compatibility.
    #[test]
    fn mdx_check_arguments_accept_defaults_and_parse_only() {
        let args = Args::try_parse_from(["soundlog", "mdx", "check", "song.mml"]).unwrap();
        assert!(matches!(
            args.command,
            Commands::Mdx {
                command: MdxCommands::Check {
                    parse_only: false,
                    loop_count: 1,
                    max_ticks: 100_000,
                    max_commands: 1_000_000,
                    ..
                }
            }
        ));
        let args = Args::try_parse_from([
            "soundlog",
            "mdx",
            "check",
            "song.mml",
            "--stdin",
            "--parse-only",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Commands::Mdx {
                command: MdxCommands::Check {
                    stdin: true,
                    parse_only: true,
                    ..
                }
            }
        ));
    }

    /// Checks positive execution limits and incompatible parse-only options.
    #[test]
    fn mdx_check_arguments_validate_playback_options() {
        for option in ["--loop-count", "--max-ticks", "--max-commands"] {
            assert!(Args::try_parse_from(["soundlog", "mdx", "check", "-", option, "0"]).is_err());
            assert!(
                Args::try_parse_from([
                    "soundlog",
                    "mdx",
                    "check",
                    "-",
                    "--parse-only",
                    option,
                    "2"
                ])
                .is_err()
            );
        }
        let args = Args::try_parse_from([
            "soundlog",
            "mdx",
            "check",
            "-",
            "--loop-count",
            "3",
            "--max-ticks",
            "64",
            "--max-commands",
            "1000",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Commands::Mdx {
                command: MdxCommands::Check {
                    loop_count: 3,
                    max_ticks: 64,
                    max_commands: 1000,
                    ..
                }
            }
        ));
    }
}

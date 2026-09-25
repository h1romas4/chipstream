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

impl From<MmlOutputFormat> for soundlog_cli::cui::mml::OutputFormat {
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
    /// Parse and validate an MML source file
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
    },
    /// Compile an MML source file into an MDX or VGM binary file
    Compile {
        /// MML source file to parse
        #[arg(value_name = "MML_FILE")]
        input: PathBuf,

        /// Output MDX or VGM file path
        #[arg(value_name = "OUTPUT_FILE")]
        output: PathBuf,

        /// Output format
        #[arg(long, value_enum, default_value_t = MmlOutputFormat::Mdx)]
        output_format: MmlOutputFormat,
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

        /// Total number of iterations for MDX repeat blocks
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

        /// Total number of whole-song playthroughs
        #[arg(long, value_name = "COUNT")]
        loop_count: Option<u32>,

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

        /// Total number of iterations for MDX repeat blocks (default: 1)
        #[arg(long, value_name = "COUNT")]
        loop_count: Option<u32>,

        /// ADPCM mode: through, resample, or lpf (default: through)
        #[arg(long, value_enum, default_value_t = AdpcmModeArg::Through)]
        adpcm_mode: AdpcmModeArg,
    },
}

#[derive(Subcommand, Debug)]
enum PdxCommands {
    /// Convert mono WAV files to ADPCM samples and write a PDX file
    Build {
        /// Input WAV files followed by the output PDX path
        #[arg(value_name = "INPUT_WAV_OR_OUTPUT_PDX", num_args = 2..)]
        files: Vec<PathBuf>,
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
            } => match cui::mml::check_with_stdin(&input, verbose, stdin) {
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
            } => match cui::mml::compile(&input, &output, output_format.into()) {
                Ok(()) => process::exit(0),
                Err(error) => {
                    soundlog_cli::log_error!(&*logger, "{error:#}");
                    process::exit(1);
                }
            },
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
                adpcm_mode,
            } => {
                let options = MdxToVgmOptions {
                    ym2151_clock,
                    okim6258_clock,
                    loop_count,
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
            command: PdxCommands::Build { files },
        } => match cui::mml::build_pdx(&files) {
            Ok(()) => process::exit(0),
            Err(error) => {
                soundlog_cli::log_error!(&*logger, "PDX build failed: {error:#}");
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

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

mod vgm;
use vgm::{info as vgm_info, read_vgm_as_vec, test_roundtrip as vgm_test_roundtrip};

/// soundlog command line tools
#[derive(Parser)]
#[command(
    name = env!("CARGO_PKG_NAME"),
    version = env!("CARGO_PKG_VERSION"),
    about = env!("CARGO_PKG_DESCRIPTION"),
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Show summary info for a VGM file (accepts .vgm or .vgz; use '-' for stdin)
    Info {
        /// Input file to read (use '-' for stdin)
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Run parse -> serialize -> re-parse roundtrip test and compare binaries
    Test {
        /// Input file to read (use '-' for stdin)
        #[arg(value_name = "FILE")]
        file: PathBuf,
        /// Print detailed diagnostics on mismatch
        #[arg(long = "diag")]
        diag: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Info { file } => {
            let bytes = read_vgm_as_vec(&file)?;
            vgm_info(&file, bytes)?;
        }
        Commands::Test { file, diag } => {
            let bytes = read_vgm_as_vec(&file)?;
            vgm_test_roundtrip(&file, bytes, diag)?;
        }
    }

    Ok(())
}

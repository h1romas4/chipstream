use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "mmlx", version, about = "Parse MML source files")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse or compile MML/MDX data.
    Mdx {
        #[command(subcommand)]
        command: MdxCommand,
    },
    /// Build PDX sample data.
    Pdx {
        #[command(subcommand)]
        command: PdxCommand,
    },
}

#[derive(Debug, Subcommand)]
enum MdxCommand {
    /// Parse and validate an MML source file.
    Check {
        /// MML source file to parse.
        input: PathBuf,

        /// Print the parsed MML syntax tree.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Compile an MML source file into an MDX binary file.
    Compile {
        /// MML source file to parse.
        input: PathBuf,

        /// Output MDX binary file.
        output: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum PdxCommand {
    /// Convert WAV files to ADPCM samples and write a PDX file.
    Build {
        /// Input WAV files followed by the output PDX path.
        #[arg(value_name = "INPUT_WAV_OR_OUTPUT_PDX", num_args = 2..)]
        files: Vec<PathBuf>,
    },
}

fn main() {
    let args = Args::parse();
    let result = match args.command {
        Command::Mdx { command } => match command {
            MdxCommand::Check { input, verbose } => mmlx_cli::check(&input, verbose),
            MdxCommand::Compile { input, output } => mmlx_cli::compile(&input, &output),
        },
        Command::Pdx {
            command: PdxCommand::Build { files },
        } => mmlx_cli::build_pdx(&files),
    };

    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

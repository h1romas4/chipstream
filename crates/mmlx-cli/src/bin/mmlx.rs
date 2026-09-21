use std::fs;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "mmlx", version, about = "Parse MML source files")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse and validate an MML source file.
    Check {
        /// MML source file to parse.
        input: PathBuf,

        /// Print the parsed MML syntax tree.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Compile an MML source file into an MDX binary file.
    Build {
        /// MML source file to parse.
        input: PathBuf,

        /// Output MDX binary file.
        output: PathBuf,
    },
}

fn main() {
    let args = Args::parse();
    match args.command {
        Command::Check { input, verbose } => {
            let document = parse_input(&input);
            if verbose {
                println!("{}", mmlx::mdx::format_tree(&document));
            }
        }
        Command::Build { input, output } => {
            let document = parse_input(&input);
            let document = match mmlx::mdx::compile(&document) {
                Ok(document) => document,
                Err(error) => {
                    eprintln!("compile error: {error}");
                    std::process::exit(1);
                }
            };
            if let Err(error) = fs::write(&output, document.to_bytes()) {
                eprintln!("{}: {error}", output.display());
                std::process::exit(1);
            }
        }
    }
}

fn parse_input(input: &Path) -> mmlx::mdx::MmlDocument {
    let source = match fs::read_to_string(input) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("{}: {error}", input.display());
            std::process::exit(1);
        }
    };
    match mmlx::mdx::parse(&source) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("parse error: {error}");
            std::process::exit(1);
        }
    }
}

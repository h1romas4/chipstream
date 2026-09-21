use std::fs;
use std::path::PathBuf;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(name = "mmlx", version, about = "Parse MML source files")]
struct Args {
    /// MML source file to parse.
    input: PathBuf,

    /// Output MDX binary file.
    output: PathBuf,
}

fn main() {
    let args = Args::parse();
    let source = match fs::read_to_string(&args.input) {
        Ok(source) => source,
        Err(error) => {
            eprintln!("{}: {error}", args.input.display());
            std::process::exit(1);
        }
    };
    let document = match mmlx::mdx::parse(&source) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("parse error: {error}");
            std::process::exit(1);
        }
    };

    let document = match mmlx::mdx::compile(&document) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("compile error: {error}");
            std::process::exit(1);
        }
    };
    if let Err(error) = fs::write(&args.output, document.to_bytes()) {
        eprintln!("{}: {error}", args.output.display());
        std::process::exit(1);
    }
}

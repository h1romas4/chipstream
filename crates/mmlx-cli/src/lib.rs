use std::path::{Path, PathBuf};

pub mod mdx;

pub fn check(input: &Path, verbose: bool) -> Result<(), String> {
    let document = mdx::parse_input(input)?;
    if verbose {
        println!("{}", mmlx::mdx::format_tree(&document));
    }
    Ok(())
}

pub fn compile(input: &Path, output: &Path) -> Result<(), String> {
    let document = mdx::parse_input(input)?;
    let document =
        mmlx::mdx::compile(&document).map_err(|error| format!("compile error: {error}"))?;
    std::fs::write(output, document.to_bytes())
        .map_err(|error| format!("{}: {error}", output.display()))
}

pub fn build_pdx(files: &[PathBuf]) -> Result<(), String> {
    mdx::build_pdx(files)
}

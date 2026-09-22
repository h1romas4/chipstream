use std::path::{Path, PathBuf};

pub mod mdx;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Mdx,
    Vgm,
}

pub fn check(input: &Path, verbose: bool) -> Result<(), String> {
    let document = mdx::parse_input(input)?;
    if verbose {
        println!("{}", mmlx::mdx::format_tree(&document));
    }
    Ok(())
}

pub fn compile(input: &Path, output: &Path) -> Result<(), String> {
    compile_with_format(input, output, OutputFormat::Mdx)
}

pub fn compile_with_format(
    input: &Path,
    output: &Path,
    output_format: OutputFormat,
) -> Result<(), String> {
    let document = mdx::parse_input(input)?;
    let mdx_document =
        mmlx::mdx::compile(&document).map_err(|error| format!("compile error: {error}"))?;
    match output_format {
        OutputFormat::Mdx => std::fs::write(output, mdx_document.to_bytes())
            .map_err(|error| format!("{}: {error}", output.display())),
        OutputFormat::Vgm => mdx::write_vgm(&document, &mdx_document, input, output),
    }
}

pub fn build_pdx(files: &[PathBuf]) -> Result<(), String> {
    mdx::build_pdx(files)
}

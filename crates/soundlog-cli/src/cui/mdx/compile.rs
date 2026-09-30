use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use soundlog::mdx::convert::{AdpcmMode, MdxToVgmOptions, to_vgm_document};
use soundlog::mdx::package::MdxPackage;
use soundlog::meta::Gd3;

use super::find_pdx_path;
use super::mml::parse_input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Mdx,
    Vgm,
}

/// Compile an MML file to MDX or VGM.
pub fn compile(
    input: &Path,
    output: &Path,
    output_format: OutputFormat,
    adpcm_mode: AdpcmMode,
) -> Result<()> {
    let source = parse_input(input)?;
    let mdx_document = mmlx::mdx::compile(&source)
        .map_err(|error| anyhow!("{}: error: compile error: {error}", input.display()))?;
    match output_format {
        OutputFormat::Mdx => fs::write(output, mdx_document.to_bytes())
            .with_context(|| format!("failed to write MDX output: {}", output.display())),
        OutputFormat::Vgm => write_vgm(&source, &mdx_document, input, output, adpcm_mode),
    }
}

fn write_vgm(
    source: &mmlx::mdx::MmlDocument,
    mdx: &soundlog::mdx::document::MdxDocument,
    input: &Path,
    output: &Path,
    adpcm_mode: AdpcmMode,
) -> Result<()> {
    let pdx_bytes = source
        .pcm_file
        .as_deref()
        .map(|name| {
            let path = find_pdx_path(input, name).ok_or_else(|| {
                anyhow!(
                    "PDX file not found: {name:?} (referenced by {})",
                    input.display()
                )
            })?;
            fs::read(&path).with_context(|| format!("failed to read PDX input: {}", path.display()))
        })
        .transpose()?;
    let package = MdxPackage::parse_owned(mdx.to_bytes(), pdx_bytes)
        .context("failed to prepare VGM conversion")?;
    let options = MdxToVgmOptions {
        adpcm_mode,
        ..MdxToVgmOptions::default()
    };
    let mut document = to_vgm_document(&package, &options)
        .map_err(|error| anyhow!("MDX to VGM conversion failed: {error}"))?;
    if let Some(title) = source.title.as_deref() {
        document.gd3 = Some(Gd3 {
            track_name_origin: Some(title.to_owned()),
            ..Gd3::default()
        });
    }
    let bytes: Vec<u8> = (&document).into();
    fs::write(output, bytes)
        .with_context(|| format!("failed to write VGM output: {}", output.display()))
}

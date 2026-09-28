use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document};
use soundlog::meta::Gd3;

use super::read_mdx_package;

/// Convert an MDX package to a VGM file.
pub fn convert_mdx(
    input: &Path,
    output: &Path,
    pdx: Option<&Path>,
    options: &MdxToVgmOptions,
) -> Result<()> {
    let package = read_mdx_package(input, pdx)?;
    let mut document = to_vgm_document(&package, options)
        .map_err(|error| anyhow!("MDX to VGM conversion failed: {error:?}"))?;
    if !package.mdx.header.title.is_empty() {
        document.gd3 = Some(Gd3 {
            track_name_origin: Some(package.mdx.header.title.clone()),
            ..Gd3::default()
        });
    }
    let bytes: Vec<u8> = (&document).into();
    fs::write(output, bytes)
        .with_context(|| format!("failed to write VGM output: {}", output.display()))?;
    Ok(())
}

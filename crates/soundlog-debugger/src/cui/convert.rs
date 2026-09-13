use std::fs;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document};
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::pcm_mixer::PCM8_OKIM6258_CLOCK_DIVIDER;
use soundlog::vgm::header::Okim6258Flags;

/// Convert an MDX file, optionally paired with its PDX file, into VGM.
pub fn mdx2vgm(
    input: &Path,
    output: &Path,
    pdx: Option<&Path>,
    options: &MdxToVgmOptions,
) -> Result<()> {
    let mdx_bytes = fs::read(input)
        .with_context(|| format!("failed to read MDX input: {}", input.display()))?;
    let pdx_bytes = pdx
        .map(|path| {
            fs::read(path).with_context(|| format!("failed to read PDX input: {}", path.display()))
        })
        .transpose()?;
    let package = MdxPackage::parse_owned(mdx_bytes, pdx_bytes)
        .map_err(|error| anyhow!("failed to parse MDX package: {error}"))?;
    let mut document = to_vgm_document(&package, options)
        .map_err(|error| anyhow!("MDX to VGM conversion failed: {error:?}"))?;
    if package.drives_okim6258() {
        document.header.okim6258_flags = Okim6258Flags {
            clock_divider: PCM8_OKIM6258_CLOCK_DIVIDER,
            adpcm_3bit_select: false,
            output_12bit: false,
            reserved: 0,
        };
    }
    let bytes: Vec<u8> = (&document).into();

    if output.as_os_str() == "-" {
        io::stdout()
            .write_all(&bytes)
            .context("failed to write VGM to stdout")?;
    } else {
        fs::write(output, bytes)
            .with_context(|| format!("failed to write VGM output: {}", output.display()))?;
    }
    Ok(())
}

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use soundlog::mdx::convert::{AdpcmMode, MdxToVgmOptions, to_vgm_document_with_diagnostics};
use soundlog::mdx::package::MdxPackage;
use soundlog::meta::Gd3;

use super::check::{CheckOptions, check_compiled_document};
use super::find_pdx_path;
use super::mml::parse_input;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Mdx,
    Vgm,
}

/// Compile MML to MDX with default playback validation, or directly to VGM.
pub fn compile(
    input: &Path,
    output: &Path,
    output_format: OutputFormat,
    adpcm_mode: AdpcmMode,
) -> Result<()> {
    compile_with_playback_check(input, output, output_format, adpcm_mode, true)
}

/// Compile MML, optionally skipping bounded playback validation for MDX output.
///
/// VGM output always reports errors encountered during the actual conversion.
pub fn compile_with_playback_check(
    input: &Path,
    output: &Path,
    output_format: OutputFormat,
    adpcm_mode: AdpcmMode,
    playback_check: bool,
) -> Result<()> {
    let source = parse_input(input)?;
    let mdx_document = mmlx::mdx::compile(&source)
        .map_err(|error| anyhow!("{}: error: compile error: {error}", input.display()))?;
    match output_format {
        OutputFormat::Mdx => {
            if playback_check {
                check_compiled_document(input, mdx_document.clone(), CheckOptions::default())?;
            }
            fs::write(output, mdx_document.to_bytes())
                .with_context(|| format!("failed to write MDX output: {}", output.display()))
        }
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
    let mut document = to_vgm_document_with_diagnostics(&package, &options)
        .map_err(|error| anyhow!("{}: error: {error}", input.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn mdx_compile_checks_playback_before_writing_without_loading_pdx() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("soundlog-compile-{}-{unique}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let input = directory.join("song.mml");
        let output = directory.join("song.mdx");
        fs::write(&output, b"existing output").unwrap();
        fs::write(&input, "A @42 c4").unwrap();
        for format in [OutputFormat::Mdx, OutputFormat::Vgm] {
            let error = compile(&input, &output, format, AdpcmMode::Through)
                .unwrap_err()
                .to_string();
            assert_eq!(
                error,
                format!(
                    "{}: error: missing tone for voice 42 (track 0, MDX command 1)",
                    input.display()
                )
            );
            assert_eq!(fs::read(&output).unwrap(), b"existing output");
        }
        compile_with_playback_check(
            &input,
            &output,
            OutputFormat::Mdx,
            AdpcmMode::Through,
            false,
        )
        .unwrap();
        assert_ne!(fs::read(&output).unwrap(), b"existing output");
        assert!(
            compile_with_playback_check(
                &input,
                &output,
                OutputFormat::Vgm,
                AdpcmMode::Through,
                false
            )
            .is_err()
        );
        fs::write(&input, "A [r1]255[r1]255[r1]255").unwrap();
        let previous = fs::read(&output).unwrap();
        let error = compile(&input, &output, OutputFormat::Mdx, AdpcmMode::Through)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("playback check incomplete: tick limit exceeded"),
            "{error}"
        );
        assert_eq!(fs::read(&output).unwrap(), previous);
        compile_with_playback_check(
            &input,
            &output,
            OutputFormat::Mdx,
            AdpcmMode::Through,
            false,
        )
        .unwrap();
        for source in ["A r4", "#pcmfile \"missing.pdx\"\nP L c4"] {
            fs::write(&input, source).unwrap();
            compile(&input, &output, OutputFormat::Mdx, AdpcmMode::Through).unwrap();
            assert_ne!(fs::read(&output).unwrap(), b"existing output");
        }
        fs::remove_dir_all(directory).unwrap();
    }
}

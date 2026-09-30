use std::fs;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document_with_diagnostics};
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
    let mut document = to_vgm_document_with_diagnostics(&package, options)
        .map_err(|error| anyhow!("{}: error: {error}", input.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::mdx::command::{MdxNote, MdxVoiceOrPcmBank};
    use soundlog::mdx::document::MdxBuilder;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn conversion_reports_playback_coordinates_without_overwriting_output() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("soundlog-convert-{}-{unique}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let input = directory.join("song.mdx");
        let output = directory.join("song.vgm");
        let mut builder = MdxBuilder::new();
        builder
            .add_mdx_command(0, MdxVoiceOrPcmBank { value: 42 })
            .add_mdx_command(
                0,
                MdxNote {
                    note: 0x80,
                    length: 8,
                },
            );
        fs::write(&input, builder.finalize().unwrap().try_to_bytes().unwrap()).unwrap();
        fs::write(&output, b"existing output").unwrap();
        for loop_count in [None, Some(1), Some(3)] {
            let options = MdxToVgmOptions {
                loop_count,
                ..Default::default()
            };
            let error = convert_mdx(&input, &output, None, &options)
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
        fs::remove_dir_all(directory).unwrap();
    }
}

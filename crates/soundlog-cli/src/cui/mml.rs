use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document};
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::pcm::encode_adpcm;
use soundlog::mdx::pdx::PdxBuilder;
use soundlog::meta::Gd3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Mdx,
    Vgm,
}

/// Parse an MML file and optionally print its typed syntax tree.
pub fn check(input: &Path, verbose: bool) -> anyhow::Result<()> {
    let document = parse_input(input)?;
    if verbose {
        let tree = mmlx::mdx::format_tree(&document);
        let mut stdout = io::BufWriter::new(io::stdout().lock());
        match stdout
            .write_all(tree.as_bytes())
            .and_then(|()| stdout.write_all(b"\n"))
            .and_then(|()| stdout.flush())
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error).context("failed to write verbose output"),
        }
    }
    Ok(())
}

/// Compile an MML file to MDX or VGM.
pub fn compile(input: &Path, output: &Path, output_format: OutputFormat) -> anyhow::Result<()> {
    let source = parse_input(input)?;
    let mdx_document = mmlx::mdx::compile(&source)
        .map_err(|error| anyhow!("{}: error: compile error: {error}", input.display()))?;
    match output_format {
        OutputFormat::Mdx => fs::write(output, mdx_document.to_bytes())
            .with_context(|| format!("failed to write MDX output: {}", output.display())),
        OutputFormat::Vgm => write_vgm(&source, &mdx_document, input, output),
    }
}

fn parse_input(input: &Path) -> anyhow::Result<mmlx::mdx::MmlDocument> {
    let source = fs::read_to_string(input)
        .with_context(|| format!("failed to read MML input: {}", input.display()))?;
    parse_source(input, &source)
}

pub(crate) fn parse_source(input: &Path, source: &str) -> anyhow::Result<mmlx::mdx::MmlDocument> {
    mmlx::mdx::parse(source).map_err(|error| {
        let location = match &error {
            mmlx::mdx::ParseError::Syntax(message) => syntax_error_location(message),
            mmlx::mdx::ParseError::InvalidValue {
                line_number,
                column,
                ..
            } => Some((*line_number, *column)),
        };
        let message = error.to_string();
        match location {
            Some((line, column)) => {
                anyhow!("{}:{line}:{column}: error: {message}", input.display())
            }
            None => anyhow!("{}: error: {message}", input.display()),
        }
    })
}

fn syntax_error_location(message: &str) -> Option<(usize, usize)> {
    let coordinates = message.lines().next()?.split_once(" at line ")?.1;
    let (line, columns) = coordinates.split_once(", columns ")?;
    let start = columns.split_once('-')?.0;
    Some((line.parse().ok()?, start.parse().ok()?))
}

fn write_vgm(
    source: &mmlx::mdx::MmlDocument,
    mdx: &soundlog::mdx::document::MdxDocument,
    input: &Path,
    output: &Path,
) -> anyhow::Result<()> {
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
    let mut document = to_vgm_document(&package, &MdxToVgmOptions::default())
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

fn find_pdx_path(input: &Path, name: &str) -> Option<PathBuf> {
    let parent = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let path = parent.join(name);
    if path.is_file() {
        return Some(path);
    }
    if Path::new(name).extension().is_none() {
        for extension in ["pdx", "PDX"] {
            let candidate = parent.join(format!("{name}.{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let requested_name = Path::new(name).file_name()?.to_str()?;
    fs::read_dir(parent)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|candidate| {
            candidate.is_file()
                && candidate
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .is_some_and(|file_name| file_name.eq_ignore_ascii_case(requested_name))
        })
}

/// Build a PDX file from mono WAV samples, using the final path as output.
pub fn build_pdx(files: &[PathBuf]) -> anyhow::Result<()> {
    let (output, inputs) = files
        .split_last()
        .ok_or_else(|| anyhow!("expected at least one input WAV and one output PDX"))?;
    let mut builder = PdxBuilder::new();

    for (sample_index, input) in inputs.iter().enumerate() {
        let samples = read_wav_samples(input)?;
        let encoded = encode_adpcm(&samples);
        builder
            .set_sample(sample_index / 96, sample_index % 96, encoded)
            .with_context(|| format!("failed to encode sample from {}", input.display()))?;
    }

    fs::write(output, builder.finalize().to_bytes())
        .with_context(|| format!("failed to write PDX output: {}", output.display()))
}

fn read_wav_samples(path: &Path) -> anyhow::Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to read WAV input: {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(anyhow!(
            "only mono WAV files are supported (found {} channels)",
            spec.channels
        ))
        .with_context(|| path.display().to_string());
    }

    match spec.sample_format {
        hound::SampleFormat::Int => match spec.bits_per_sample {
            8 => reader
                .samples::<i8>()
                .map(|sample| sample.map(|value| i16::from(value) << 4))
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("failed to read WAV samples: {}", path.display())),
            16 => reader
                .samples::<i16>()
                .map(|sample| sample.map(|value| value >> 4))
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("failed to read WAV samples: {}", path.display())),
            24 | 32 => reader
                .samples::<i32>()
                .map(|sample| {
                    sample.map(|value| {
                        let shift = u32::from(spec.bits_per_sample - 12);
                        (value >> shift).clamp(-2048, 2047) as i16
                    })
                })
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("failed to read WAV samples: {}", path.display())),
            bits => Err(anyhow!("unsupported integer WAV bit depth {bits}"))
                .with_context(|| path.display().to_string()),
        },
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| {
                sample.map(|value| (value * 2047.0).round().clamp(-2048.0, 2047.0) as i16)
            })
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("failed to read WAV samples: {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::mdx::pdx::PdxDocument;

    #[test]
    fn builds_pdx_from_wav_input() {
        let stem = format!("soundlog-cli-pdx-test-{}", std::process::id());
        let wav_path = std::env::temp_dir().join(format!("{stem}.wav"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.pdx"));
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 8_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&wav_path, spec).unwrap();
        for sample in [0_i16, 1024, -1024] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();

        build_pdx(&[wav_path.clone(), pdx_path.clone()]).unwrap();
        let document = PdxDocument::parse(&fs::read(&pdx_path).unwrap()).unwrap();
        assert_eq!(document.sample_bytes(0, 0).unwrap().len(), 2);

        fs::remove_file(wav_path).unwrap();
        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn resolves_pdx_stem_with_uppercase_extension() {
        let stem = format!("soundlog-cli-pdx-path-{}", std::process::id());
        let input_path = std::env::temp_dir().join(format!("{stem}.mml"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(find_pdx_path(&input_path, &stem), Some(pdx_path.clone()));

        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn resolves_pdx_name_case_insensitively() {
        let stem = format!("soundlog-cli-pdx-case-{}", std::process::id());
        let input_path = std::env::temp_dir().join(format!("{stem}.mml"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(
            find_pdx_path(&input_path, &format!("{stem}.pdx")),
            Some(pdx_path.clone())
        );

        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn resolves_pdx_for_input_without_parent_directory() {
        let stem = format!("soundlog-cli-pdx-relative-{}", std::process::id());
        let input_path = PathBuf::from(format!("{stem}.mml"));
        let pdx_path = PathBuf::from(format!("{stem}.PDX"));
        fs::write(&pdx_path, [0_u8]).unwrap();

        assert_eq!(
            find_pdx_path(&input_path, &format!("{stem}.pdx")),
            Some(PathBuf::from(format!("./{stem}.PDX")))
        );

        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn formats_parse_errors_as_editor_diagnostics() {
        let input = Path::new("songs/broken.mml");

        let syntax_error = parse_source(input, "A c4\nB ]").unwrap_err();
        assert!(
            syntax_error.to_string().starts_with(
                "songs/broken.mml:2:3: error: MML syntax error at line 2, columns 3-4"
            )
        );
        assert!(syntax_error.to_string().contains("  B ]\n    ^"));

        let grammar_error = parse_source(input, "A z").unwrap_err();
        assert!(
            grammar_error.to_string().starts_with(
                "songs/broken.mml:1:3: error: MML syntax error at line 1, columns 3-4"
            )
        );

        let value_error = parse_source(input, "A c4\nB t5000").unwrap_err();
        assert!(
            value_error
                .to_string()
                .starts_with("songs/broken.mml:2:4: error: MML value error at line 2, columns 4-8")
        );
    }
}

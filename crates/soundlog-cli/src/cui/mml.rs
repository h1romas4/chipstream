use std::collections::HashSet;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document};
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::pcm::{decode_adpcm, encode_adpcm};
use soundlog::mdx::pdx::{PdxBuilder, PdxDocument};
use soundlog::meta::Gd3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Mdx,
    Vgm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdxExportFormat {
    Raw,
    Wav,
}

/// Parse an MML file and optionally print its typed syntax tree.
pub fn check(input: &Path, verbose: bool) -> anyhow::Result<()> {
    check_with_stdin(input, verbose, false)
}

/// Parse an MML file or stdin and optionally print its typed syntax tree.
pub fn check_with_stdin(input: &Path, verbose: bool, stdin: bool) -> anyhow::Result<()> {
    let document = if stdin || input == Path::new("-") {
        parse_reader(input, io::stdin().lock())?
    } else {
        parse_input(input)?
    };
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

fn parse_reader<R: Read>(input: &Path, mut reader: R) -> anyhow::Result<mmlx::mdx::MmlDocument> {
    let mut source = String::new();
    reader.read_to_string(&mut source).with_context(|| {
        format!(
            "failed to read MML input from stdin for {}",
            input.display()
        )
    })?;
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

/// Build a PDX file from WAV or raw ADPCM samples, using the final path as output.
pub fn build_pdx(files: &[PathBuf], enable_lz: bool) -> anyhow::Result<()> {
    let (output, inputs) = files
        .split_last()
        .ok_or_else(|| anyhow!("expected at least one input WAV and one output PDX"))?;
    let mut builder = PdxBuilder::new();
    builder.set_lz_compressed(enable_lz);
    let exported_slots = inputs
        .iter()
        .map(|input| parse_exported_sample_slot(input))
        .collect::<Vec<_>>();
    let exported_count = exported_slots.iter().filter(|slot| slot.is_some()).count();
    if exported_count != 0 && exported_count != inputs.len() {
        return Err(anyhow!(
            "cannot mix exported bank-note WAV names with ordinary WAV input names"
        ));
    }
    let mut used_slots = HashSet::new();

    for (sample_index, input) in inputs.iter().enumerate() {
        let encoded = match input.extension().and_then(|extension| extension.to_str()) {
            Some(extension) if extension.eq_ignore_ascii_case("wav") => {
                encode_adpcm(&read_wav_samples(input)?)
            }
            Some(extension) if extension.eq_ignore_ascii_case("raw") => fs::read(input)
                .with_context(|| format!("failed to read raw ADPCM input: {}", input.display()))?,
            _ => {
                return Err(anyhow!(
                    "unsupported PDX sample input {}; expected WAV or raw ADPCM",
                    input.display()
                ));
            }
        };
        let (bank, note) =
            exported_slots[sample_index].unwrap_or((sample_index / 96, sample_index % 96));
        if !used_slots.insert((bank, note)) {
            return Err(anyhow!(
                "duplicate PDX sample slot: bank {bank}, note {note}"
            ));
        }
        builder
            .set_sample(bank, note, encoded)
            .with_context(|| format!("failed to encode sample from {}", input.display()))?;
    }

    fs::write(output, builder.finalize().to_bytes())
        .with_context(|| format!("failed to write PDX output: {}", output.display()))
}

fn parse_exported_sample_slot(path: &Path) -> Option<(usize, usize)> {
    let stem = path.file_stem()?.to_str()?;
    let coordinates = stem.strip_prefix("bank-")?;
    let (bank, note) = coordinates.split_once("-note-")?;
    Some((bank.parse().ok()?, note.parse().ok()?))
}

/// Export populated PDX ADPCM samples as raw bytes or decoded mono 16-bit WAV files.
pub fn export_pdx(
    input: &Path,
    output_dir: &Path,
    sample_rate: u32,
    output_format: PdxExportFormat,
) -> anyhow::Result<usize> {
    if output_format == PdxExportFormat::Wav && sample_rate == 0 {
        return Err(anyhow!("WAV sample rate must be greater than zero"));
    }
    let bytes = fs::read(input)
        .with_context(|| format!("failed to read PDX input: {}", input.display()))?;
    let document = PdxDocument::parse(&bytes)
        .with_context(|| format!("failed to parse PDX input: {}", input.display()))?;
    fs::create_dir_all(output_dir).with_context(|| {
        format!(
            "failed to create output directory: {}",
            output_dir.display()
        )
    })?;

    let wav_spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut exported = 0;
    for (bank, bank_data) in document.banks.iter().enumerate() {
        for (note, entry) in bank_data.entries.iter().enumerate() {
            if entry.is_none() {
                continue;
            }
            let adpcm = document
                .sample_bytes(bank, note)
                .ok_or_else(|| anyhow!("invalid PDX sample range: bank {bank}, note {note}"))?;
            let extension = match output_format {
                PdxExportFormat::Raw => "raw",
                PdxExportFormat::Wav => "wav",
            };
            let output = output_dir.join(format!("bank-{bank:02}-note-{note:02}.{extension}"));
            match output_format {
                PdxExportFormat::Raw => {
                    fs::write(&output, adpcm).with_context(|| {
                        format!("failed to write raw ADPCM output: {}", output.display())
                    })?;
                }
                PdxExportFormat::Wav => {
                    let mut writer =
                        hound::WavWriter::create(&output, wav_spec).with_context(|| {
                            format!("failed to create WAV output: {}", output.display())
                        })?;
                    for sample in decode_adpcm(adpcm) {
                        writer.write_sample(sample << 4).with_context(|| {
                            format!("failed to write WAV samples: {}", output.display())
                        })?;
                    }
                    writer.finalize().with_context(|| {
                        format!("failed to finalize WAV output: {}", output.display())
                    })?;
                }
            }
            exported += 1;
        }
    }
    Ok(exported)
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
    use soundlog::mdx::pdx::{PdxBuilder, PdxDocument};

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

        build_pdx(&[wav_path.clone(), pdx_path.clone()], false).unwrap();
        let document = PdxDocument::parse(&fs::read(&pdx_path).unwrap()).unwrap();
        assert_eq!(document.sample_bytes(0, 0).unwrap().len(), 2);

        fs::remove_file(wav_path).unwrap();
        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn builds_lz_compressed_pdx_when_enabled() {
        let stem = format!("soundlog-cli-pdx-lz-{}", std::process::id());
        let raw_path = std::env::temp_dir().join(format!("{stem}.raw"));
        let pdx_path = std::env::temp_dir().join(format!("{stem}.pdx"));
        let expected_adpcm = vec![0x00, 0x17, 0x8f, 0x42];
        fs::write(&raw_path, &expected_adpcm).unwrap();

        build_pdx(&[raw_path.clone(), pdx_path.clone()], true).unwrap();
        let document = PdxDocument::parse(&fs::read(&pdx_path).unwrap()).unwrap();
        assert!(document.is_compressed());
        assert_eq!(document.sample_bytes(0, 0).unwrap(), expected_adpcm);

        fs::remove_file(raw_path).unwrap();
        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn export_build_round_trip_preserves_sparse_sample_slots() {
        let stem = format!("soundlog-cli-pdx-roundtrip-{}", std::process::id());
        let directory = std::env::temp_dir().join(stem);
        let input_pdx = directory.join("original.pdx");
        let export_dir = directory.join("wav");
        let rebuilt_pdx = directory.join("rebuilt.pdx");
        fs::create_dir_all(&directory).unwrap();

        let mut builder = PdxBuilder::new();
        builder.set_sample(0, 5, vec![0x70, 0x9c, 0x47]).unwrap();
        builder
            .set_sample(1, 2, vec![0x01, 0x23, 0x45, 0x67])
            .unwrap();
        let original_bytes = builder.finalize().to_bytes();
        fs::write(&input_pdx, &original_bytes).unwrap();

        assert_eq!(
            export_pdx(&input_pdx, &export_dir, 15_625, PdxExportFormat::Wav).unwrap(),
            2
        );
        let mut wav_files = fs::read_dir(&export_dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        wav_files.sort();
        assert_eq!(
            wav_files
                .iter()
                .filter_map(|path| path.file_name().and_then(|name| name.to_str()))
                .collect::<Vec<_>>(),
            ["bank-00-note-05.wav", "bank-01-note-02.wav"]
        );
        for wav_path in &wav_files {
            assert_eq!(
                hound::WavReader::open(wav_path).unwrap().spec().sample_rate,
                15_625
            );
        }

        let mut build_files = wav_files;
        build_files.push(rebuilt_pdx.clone());
        build_pdx(&build_files, false).unwrap();
        assert_eq!(fs::read(&rebuilt_pdx).unwrap(), original_bytes);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn raw_export_build_round_trip_preserves_adpcm_bytes() {
        let stem = format!("soundlog-cli-pdx-raw-{}", std::process::id());
        let directory = std::env::temp_dir().join(stem);
        let input_pdx = directory.join("original.pdx");
        let export_dir = directory.join("adpcm");
        let rebuilt_pdx = directory.join("rebuilt.pdx");
        fs::create_dir_all(&directory).unwrap();

        let mut builder = PdxBuilder::new();
        let expected_adpcm = vec![0x00, 0x17, 0x8f, 0x42];
        builder.set_sample(1, 3, expected_adpcm.clone()).unwrap();
        let original_bytes = builder.finalize().to_bytes();
        fs::write(&input_pdx, &original_bytes).unwrap();

        assert_eq!(
            export_pdx(&input_pdx, &export_dir, 0, PdxExportFormat::Raw).unwrap(),
            1
        );
        let raw_path = export_dir.join("bank-01-note-03.raw");
        assert_eq!(fs::read(&raw_path).unwrap(), expected_adpcm);
        build_pdx(&[raw_path, rebuilt_pdx.clone()], false).unwrap();
        assert_eq!(fs::read(&rebuilt_pdx).unwrap(), original_bytes);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn build_rejects_mixed_exported_and_ordinary_wav_names() {
        let stem = format!("soundlog-cli-pdx-mixed-{}", std::process::id());
        let directory = std::env::temp_dir().join(stem);
        fs::create_dir_all(&directory).unwrap();
        let exported = directory.join("bank-00-note-00.wav");
        let ordinary = directory.join("sample.wav");
        let output = directory.join("output.pdx");

        for path in [&exported, &ordinary] {
            let writer = hound::WavWriter::create(
                path,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: 15_625,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            writer.finalize().unwrap();
        }

        let error = build_pdx(&[exported, ordinary, output], false).unwrap_err();
        assert!(error.to_string().contains("cannot mix"));
        fs::remove_dir_all(directory).unwrap();
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

    #[test]
    fn parses_stdin_and_keeps_the_input_path_in_diagnostics() {
        let input = Path::new("songs/unsaved.mml");

        assert!(
            parse_reader(input, io::Cursor::new("A c4\nB t5000")).is_err_and(|error| {
                error
                    .to_string()
                    .starts_with("songs/unsaved.mml:2:4: error: MML value error")
            })
        );
        assert!(parse_reader(input, io::Cursor::new("A c4")).is_ok());
    }
}

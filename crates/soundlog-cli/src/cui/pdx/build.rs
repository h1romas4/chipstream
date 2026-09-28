use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use comfy_table::{Cell, ContentArrangement, Table, presets::NOTHING};
use soundlog::mdx::pcm::encode_adpcm;
use soundlog::mdx::pdx::{PdxBuilder, PdxDocument};

use crate::logger::Logger;

#[derive(Debug, Clone)]
pub struct PdxBuildReport {
    pub output_path: PathBuf,
    pub file_bytes: usize,
    pub decoded_bytes: usize,
    pub lz_compressed: bool,
    pub banks: usize,
    pub samples: Vec<PdxBuildSampleReport>,
}

#[derive(Debug, Clone)]
pub struct PdxBuildSampleReport {
    pub input_path: PathBuf,
    pub input_format: &'static str,
    pub input_bit_depth: Option<u16>,
    pub conversion: &'static str,
    pub input_bytes: u64,
    pub bank: usize,
    pub note: usize,
    pub adpcm_bytes: usize,
}

struct WavSampleData {
    samples: Vec<i16>,
    format: &'static str,
    bits_per_sample: u16,
    conversion: &'static str,
}

/// Build a PDX file from WAV or raw ADPCM samples, using the final path as output.
pub fn build_pdx(files: &[PathBuf], enable_lz: bool) -> Result<()> {
    build_pdx_with_report(files, enable_lz, false).map(|_| ())
}

/// Build a PDX file and return details for optional verbose reporting.
pub fn build_pdx_with_report(
    files: &[PathBuf],
    enable_lz: bool,
    disable_12bit_conversion: bool,
) -> Result<PdxBuildReport> {
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
    let mut sample_reports = Vec::with_capacity(inputs.len());

    for (sample_index, input) in inputs.iter().enumerate() {
        let (encoded, input_format, input_bit_depth, conversion, input_bytes) = match input
            .extension()
            .and_then(|extension| extension.to_str())
        {
            Some(extension) if extension.eq_ignore_ascii_case("wav") => {
                let wav = read_wav_samples(input, disable_12bit_conversion)?;
                let input_bytes = fs::metadata(input)
                    .with_context(|| format!("failed to read WAV metadata: {}", input.display()))?
                    .len();
                (
                    encode_adpcm(&wav.samples),
                    wav.format,
                    Some(wav.bits_per_sample),
                    wav.conversion,
                    input_bytes,
                )
            }
            Some(extension) if extension.eq_ignore_ascii_case("raw") => {
                let encoded = fs::read(input).with_context(|| {
                    format!("failed to read raw ADPCM input: {}", input.display())
                })?;
                let input_bytes = encoded.len() as u64;
                (
                    encoded,
                    "raw ADPCM",
                    None,
                    "no (raw bytes copied)",
                    input_bytes,
                )
            }
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
        sample_reports.push(PdxBuildSampleReport {
            input_path: input.clone(),
            input_format,
            input_bit_depth,
            conversion,
            input_bytes,
            bank,
            note,
            adpcm_bytes: encoded.len(),
        });
        builder
            .set_sample(bank, note, encoded)
            .with_context(|| format!("failed to encode sample from {}", input.display()))?;
    }

    let output_bytes = builder.finalize().to_bytes();
    let document = PdxDocument::parse(&output_bytes).context("failed to validate built PDX")?;
    fs::write(output, &output_bytes)
        .with_context(|| format!("failed to write PDX output: {}", output.display()))?;

    Ok(PdxBuildReport {
        output_path: output.clone(),
        file_bytes: output_bytes.len(),
        decoded_bytes: document.decoded_bytes().len(),
        lz_compressed: document.is_compressed(),
        banks: document.banks.len(),
        samples: sample_reports,
    })
}

fn parse_exported_sample_slot(path: &Path) -> Option<(usize, usize)> {
    let stem = path.file_stem()?.to_str()?;
    let coordinates = stem.strip_prefix("bank-")?;
    let (bank, note) = coordinates.split_once("-note-")?;
    Some((bank.parse().ok()?, note.parse().ok()?))
}

fn read_wav_samples(path: &Path, disable_12bit_conversion: bool) -> Result<WavSampleData> {
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

    let (samples, format, conversion) = match spec.sample_format {
        hound::SampleFormat::Int => match spec.bits_per_sample {
            8 => (
                reader
                    .samples::<i8>()
                    .map(|sample| sample.map(|value| i16::from(value) << 4))
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| format!("failed to read WAV samples: {}", path.display()))?,
                "integer PCM",
                "yes (8 -> 12 bit, << 4)",
            ),
            16 => {
                let samples = reader
                    .samples::<i16>()
                    .map(|sample| {
                        sample.map(|value| {
                            if disable_12bit_conversion {
                                value
                            } else {
                                value >> 4
                            }
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| format!("failed to read WAV samples: {}", path.display()))?;
                (
                    samples,
                    "integer PCM",
                    if disable_12bit_conversion {
                        "no (16-bit passed through; encoder clamps to 12-bit)"
                    } else {
                        "yes (16 -> 12 bit, >> 4)"
                    },
                )
            }
            24 | 32 => (
                reader
                    .samples::<i32>()
                    .map(|sample| {
                        sample.map(|value| {
                            let shift = u32::from(spec.bits_per_sample - 12);
                            (value >> shift).clamp(-2048, 2047) as i16
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .with_context(|| format!("failed to read WAV samples: {}", path.display()))?,
                "integer PCM",
                if spec.bits_per_sample == 24 {
                    "yes (24 -> 12 bit, >> 12)"
                } else {
                    "yes (32 -> 12 bit, >> 20)"
                },
            ),
            bits => Err(anyhow!("unsupported integer WAV bit depth {bits}"))
                .with_context(|| path.display().to_string())?,
        },
        hound::SampleFormat::Float => (
            reader
                .samples::<f32>()
                .map(|sample| {
                    sample.map(|value| (value * 2047.0).round().clamp(-2048.0, 2047.0) as i16)
                })
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("failed to read WAV samples: {}", path.display()))?,
            "float PCM",
            "yes (float -> 12 bit, scale/clamp)",
        ),
    };

    Ok(WavSampleData {
        samples,
        format,
        bits_per_sample: spec.bits_per_sample,
        conversion,
    })
}

/// Print detailed information about a completed PDX build.
pub fn log_build_report(report: &PdxBuildReport, logger: &Logger) -> Result<()> {
    logger.info(format_args!("PDX build complete:"))?;

    let mut summary = Table::new();
    summary.load_style(NOTHING);
    summary.set_content_arrangement(ContentArrangement::Dynamic);
    summary.set_header(vec![Cell::new("Field"), Cell::new("Value")]);
    summary.add_row(vec![
        Cell::new("output"),
        Cell::new(report.output_path.display()),
    ]);
    summary.add_row(vec![Cell::new("file_bytes"), Cell::new(report.file_bytes)]);
    summary.add_row(vec![
        Cell::new("decoded_bytes"),
        Cell::new(report.decoded_bytes),
    ]);
    summary.add_row(vec![
        Cell::new("lz_compressed"),
        Cell::new(report.lz_compressed),
    ]);
    summary.add_row(vec![Cell::new("banks"), Cell::new(report.banks)]);
    summary.add_row(vec![Cell::new("samples"), Cell::new(report.samples.len())]);
    logger.info(format_args!("{summary}"))?;

    let mut sample_table = Table::new();
    sample_table.load_style(NOTHING);
    sample_table.set_content_arrangement(ContentArrangement::Dynamic);
    sample_table.set_header(vec![
        Cell::new("Input"),
        Cell::new("Format"),
        Cell::new("Bit depth"),
        Cell::new("12-bit conversion"),
        Cell::new("Input bytes"),
        Cell::new("Bank"),
        Cell::new("Note"),
        Cell::new("ADPCM bytes"),
    ]);
    for sample in &report.samples {
        sample_table.add_row(vec![
            Cell::new(sample.input_path.display()),
            Cell::new(sample.input_format),
            Cell::new(
                sample
                    .input_bit_depth
                    .map(|bits| format!("{bits}-bit"))
                    .unwrap_or_else(|| "n/a".to_string()),
            ),
            Cell::new(sample.conversion),
            Cell::new(sample.input_bytes),
            Cell::new(sample.bank),
            Cell::new(sample.note),
            Cell::new(sample.adpcm_bytes),
        ]);
    }
    logger.info(format_args!("Samples (bank/note indices are zero-based):"))?;
    logger.info(format_args!("{sample_table}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let report =
            build_pdx_with_report(&[wav_path.clone(), pdx_path.clone()], false, false).unwrap();
        assert_eq!(report.samples[0].input_bit_depth, Some(16));
        assert_eq!(report.samples[0].conversion, "yes (16 -> 12 bit, >> 4)");
        let document = PdxDocument::parse(&fs::read(&pdx_path).unwrap()).unwrap();
        assert_eq!(document.sample_bytes(0, 0).unwrap().len(), 2);

        fs::remove_file(wav_path).unwrap();
        fs::remove_file(pdx_path).unwrap();
    }

    #[test]
    fn can_disable_16bit_pcm_scaling_before_adpcm_encoding() {
        let stem = format!("soundlog-cli-pdx-no-12bit-{}", std::process::id());
        let wav_path = std::env::temp_dir().join(format!("{stem}.wav"));
        let default_pdx = std::env::temp_dir().join(format!("{stem}-default.pdx"));
        let unscaled_pdx = std::env::temp_dir().join(format!("{stem}-unscaled.pdx"));
        let mut writer = hound::WavWriter::create(
            &wav_path,
            hound::WavSpec {
                channels: 1,
                sample_rate: 8_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for sample in [0_i16, 8_192, -8_192, 1_024] {
            writer.write_sample(sample).unwrap();
        }
        writer.finalize().unwrap();

        let default_report =
            build_pdx_with_report(&[wav_path.clone(), default_pdx.clone()], false, false).unwrap();
        let unscaled_report =
            build_pdx_with_report(&[wav_path.clone(), unscaled_pdx.clone()], false, true).unwrap();

        assert_eq!(
            default_report.samples[0].conversion,
            "yes (16 -> 12 bit, >> 4)"
        );
        assert_eq!(
            unscaled_report.samples[0].conversion,
            "no (16-bit passed through; encoder clamps to 12-bit)"
        );
        let default_document = PdxDocument::parse(&fs::read(&default_pdx).unwrap()).unwrap();
        let unscaled_document = PdxDocument::parse(&fs::read(&unscaled_pdx).unwrap()).unwrap();
        assert_ne!(
            default_document.sample_bytes(0, 0),
            unscaled_document.sample_bytes(0, 0)
        );

        fs::remove_file(wav_path).unwrap();
        fs::remove_file(default_pdx).unwrap();
        fs::remove_file(unscaled_pdx).unwrap();
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
}

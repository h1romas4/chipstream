use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use comfy_table::{Cell, ContentArrangement, Table, presets::NOTHING};
use soundlog::mdx::pcm::decode_adpcm;
use soundlog::mdx::pdx::PdxDocument;

use crate::logger::Logger;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdxExportFormat {
    Raw,
    Wav,
}

#[derive(Debug, Clone)]
pub struct PdxExportReport {
    pub input_path: PathBuf,
    pub input_bytes: usize,
    pub decoded_bytes: usize,
    pub lz_compressed: bool,
    pub output_dir: PathBuf,
    pub output_format: PdxExportFormat,
    pub sample_rate: Option<u32>,
    pub samples: Vec<PdxExportSampleReport>,
}

#[derive(Debug, Clone)]
pub struct PdxExportSampleReport {
    pub bank: usize,
    pub note: usize,
    pub output_path: PathBuf,
    pub adpcm_bytes: usize,
    pub output_bytes: u64,
    pub frames: Option<usize>,
}

/// Export populated PDX ADPCM samples as raw bytes or decoded mono 16-bit WAV files.
pub fn export_pdx(
    input: &Path,
    output_dir: &Path,
    sample_rate: u32,
    output_format: PdxExportFormat,
) -> Result<usize> {
    export_pdx_with_report(input, output_dir, sample_rate, output_format)
        .map(|report| report.samples.len())
}

/// Export PDX samples and return details for optional verbose reporting.
pub fn export_pdx_with_report(
    input: &Path,
    output_dir: &Path,
    sample_rate: u32,
    output_format: PdxExportFormat,
) -> Result<PdxExportReport> {
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
    let mut sample_reports = Vec::new();
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
            let output_bytes = fs::metadata(&output)
                .with_context(|| format!("failed to read output metadata: {}", output.display()))?
                .len();
            sample_reports.push(PdxExportSampleReport {
                bank,
                note,
                output_path: output,
                adpcm_bytes: adpcm.len(),
                output_bytes,
                frames: match output_format {
                    PdxExportFormat::Raw => None,
                    PdxExportFormat::Wav => Some(adpcm.len() * 2),
                },
            });
        }
    }

    Ok(PdxExportReport {
        input_path: input.to_path_buf(),
        input_bytes: bytes.len(),
        decoded_bytes: document.decoded_bytes().len(),
        lz_compressed: document.is_compressed(),
        output_dir: output_dir.to_path_buf(),
        output_format,
        sample_rate: match output_format {
            PdxExportFormat::Raw => None,
            PdxExportFormat::Wav => Some(sample_rate),
        },
        samples: sample_reports,
    })
}

/// Print detailed information about exported PDX samples.
pub fn log_export_report(report: &PdxExportReport, logger: &Logger) -> Result<()> {
    logger.info(format_args!("PDX export complete:"))?;

    let mut summary = Table::new();
    summary.load_style(NOTHING);
    summary.set_content_arrangement(ContentArrangement::Dynamic);
    summary.set_header(vec![Cell::new("Field"), Cell::new("Value")]);
    summary.add_row(vec![
        Cell::new("input"),
        Cell::new(report.input_path.display()),
    ]);
    summary.add_row(vec![
        Cell::new("input_bytes"),
        Cell::new(report.input_bytes),
    ]);
    summary.add_row(vec![
        Cell::new("decoded_bytes"),
        Cell::new(report.decoded_bytes),
    ]);
    summary.add_row(vec![
        Cell::new("lz_compressed"),
        Cell::new(report.lz_compressed),
    ]);
    summary.add_row(vec![
        Cell::new("output_dir"),
        Cell::new(report.output_dir.display()),
    ]);
    summary.add_row(vec![
        Cell::new("output_format"),
        Cell::new(match report.output_format {
            PdxExportFormat::Raw => "raw ADPCM",
            PdxExportFormat::Wav => "mono 16-bit PCM WAV",
        }),
    ]);
    summary.add_row(vec![
        Cell::new("sample_rate_hz"),
        Cell::new(
            report
                .sample_rate
                .map(|rate| rate.to_string())
                .unwrap_or_else(|| "n/a".to_string()),
        ),
    ]);
    summary.add_row(vec![Cell::new("samples"), Cell::new(report.samples.len())]);
    logger.info(format_args!("{summary}"))?;

    let mut sample_table = Table::new();
    sample_table.load_style(NOTHING);
    sample_table.set_content_arrangement(ContentArrangement::Dynamic);
    sample_table.set_header(vec![
        Cell::new("Bank"),
        Cell::new("Note"),
        Cell::new("Output"),
        Cell::new("ADPCM bytes"),
        Cell::new("Output bytes"),
        Cell::new("Frames"),
    ]);
    for sample in &report.samples {
        sample_table.add_row(vec![
            Cell::new(sample.bank),
            Cell::new(sample.note),
            Cell::new(sample.output_path.display()),
            Cell::new(sample.adpcm_bytes),
            Cell::new(sample.output_bytes),
            Cell::new(
                sample
                    .frames
                    .map(|frames| frames.to_string())
                    .unwrap_or_else(|| "n/a".to_string()),
            ),
        ]);
    }
    logger.info(format_args!("Samples (bank/note indices are zero-based):"))?;
    logger.info(format_args!("{sample_table}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::mdx::pdx::PdxBuilder;

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
        super::super::build::build_pdx(&build_files, false).unwrap();
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
        super::super::build::build_pdx(&[raw_path, rebuilt_pdx.clone()], false).unwrap();
        assert_eq!(fs::read(&rebuilt_pdx).unwrap(), original_bytes);

        fs::remove_dir_all(directory).unwrap();
    }
}

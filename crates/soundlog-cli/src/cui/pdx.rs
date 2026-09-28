use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use comfy_table::{Cell, ContentArrangement, Table, presets::NOTHING};
use soundlog::mdx::pdx::{PdxDocument, PdxSample};

use crate::cui::mml::{PdxBuildReport, PdxExportFormat, PdxExportReport};
use crate::logger::Logger;

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

/// Parse and validate a PDX file, then print its bank and sample allocation.
pub fn test_pdx(input: &Path, logger: Arc<Logger>) -> Result<()> {
    let bytes = fs::read(input)
        .with_context(|| format!("failed to read PDX input: {}", input.display()))?;
    let document = PdxDocument::parse(&bytes)
        .with_context(|| format!("failed to parse PDX input: {}", input.display()))?;
    if logger.is_noop() {
        return Ok(());
    }

    let samples = populated_samples(&document);
    let sample_bytes = samples
        .iter()
        .map(|(_, _, sample)| u64::from(sample.size))
        .sum::<u64>();
    let table_bytes = document
        .banks
        .iter()
        .map(|bank| bank.entries.len() * 8)
        .sum::<usize>();

    let _ = logger.info(format_args!("PDX:"));
    let mut summary = Table::new();
    summary.load_style(NOTHING);
    summary.set_content_arrangement(ContentArrangement::Dynamic);
    summary.set_header(vec![Cell::new("Field"), Cell::new("Value")]);
    summary.add_row(vec![Cell::new("file_bytes"), Cell::new(bytes.len())]);
    summary.add_row(vec![
        Cell::new("decoded_bytes"),
        Cell::new(document.decoded_bytes().len()),
    ]);
    summary.add_row(vec![
        Cell::new("lz_compressed"),
        Cell::new(if document.is_compressed() {
            "yes"
        } else {
            "no"
        }),
    ]);
    summary.add_row(vec![Cell::new("banks"), Cell::new(document.banks.len())]);
    summary.add_row(vec![Cell::new("samples"), Cell::new(samples.len())]);
    summary.add_row(vec![
        Cell::new("sample_data_bytes"),
        Cell::new(sample_bytes),
    ]);
    summary.add_row(vec![Cell::new("bank_table_bytes"), Cell::new(table_bytes)]);
    let _ = logger.info(format_args!("{summary}"));

    let mut bank_table = Table::new();
    bank_table.load_style(NOTHING);
    bank_table.set_content_arrangement(ContentArrangement::Dynamic);
    bank_table.set_header(vec![
        Cell::new("Bank"),
        Cell::new("Samples"),
        Cell::new("Bytes"),
    ]);
    for (bank_index, bank) in document.banks.iter().enumerate() {
        let entries = bank.entries.iter().flatten().collect::<Vec<_>>();
        let allocated_bytes = entries
            .iter()
            .map(|sample| u64::from(sample.size))
            .sum::<u64>();
        bank_table.add_row(vec![
            Cell::new(bank_index),
            Cell::new(entries.len()),
            Cell::new(allocated_bytes),
        ]);
    }
    let _ = logger.info(format_args!("Banks:"));
    let _ = logger.info(format_args!("{bank_table}"));

    let mut sample_table = Table::new();
    sample_table.load_style(NOTHING);
    sample_table.set_content_arrangement(ContentArrangement::Dynamic);
    sample_table.set_header(vec![
        Cell::new("Bank"),
        Cell::new("Note"),
        Cell::new("Offset"),
        Cell::new("Bytes"),
    ]);
    if samples.is_empty() {
        sample_table.add_row(vec![
            Cell::new("(none)"),
            Cell::new(""),
            Cell::new(""),
            Cell::new(""),
        ]);
    } else {
        for (bank, note, sample) in samples {
            sample_table.add_row(vec![
                Cell::new(bank),
                Cell::new(note),
                Cell::new(sample.start),
                Cell::new(sample.size),
            ]);
        }
    }
    let _ = logger.info(format_args!("Samples (bank/note indices are zero-based):"));
    let _ = logger.info(format_args!("{sample_table}"));
    Ok(())
}

fn populated_samples(document: &PdxDocument) -> Vec<(usize, usize, PdxSample)> {
    document
        .banks
        .iter()
        .enumerate()
        .flat_map(|(bank_index, bank)| {
            bank.entries
                .iter()
                .enumerate()
                .filter_map(move |(note_index, sample)| {
                    sample.map(|sample| (bank_index, note_index, sample))
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::mdx::pdx::PdxBuilder;

    #[test]
    fn lists_populated_samples_with_their_bank_and_note_indices() {
        let mut builder = PdxBuilder::new();
        builder.set_sample(0, 5, vec![0x01, 0x02]).unwrap();
        builder.set_sample(2, 7, vec![0x03, 0x04, 0x05]).unwrap();
        let document = builder.finalize();

        let samples = populated_samples(&document);

        assert_eq!(
            samples,
            vec![
                (
                    0,
                    5,
                    PdxSample {
                        start: 2304,
                        size: 2
                    }
                ),
                (
                    2,
                    7,
                    PdxSample {
                        start: 2306,
                        size: 3
                    }
                ),
            ]
        );
    }

    #[test]
    fn reports_invalid_pdx_input_with_path_context() {
        let input =
            std::env::temp_dir().join(format!("soundlog-invalid-pdx-{}", std::process::id()));
        fs::write(&input, b"invalid").unwrap();

        let error = test_pdx(&input, Arc::new(Logger::new_noop())).unwrap_err();

        assert!(error.to_string().contains("failed to parse PDX input"));
        assert!(error.to_string().contains(input.to_string_lossy().as_ref()));
        fs::remove_file(input).unwrap();
    }
}

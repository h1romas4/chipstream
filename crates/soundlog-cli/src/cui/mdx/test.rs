use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use comfy_table::{Cell, ContentArrangement, Table, presets::NOTHING};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document};
use soundlog::meta::Gd3;

use crate::logger::Logger;

use super::{read_mdx_package, resolve_pdx_path};

/// Convert an MDX package to VGM and verify that the generated VGM parses.
pub fn test_mdx(
    input: &Path,
    pdx: Option<&Path>,
    logger: Arc<Logger>,
    options: &MdxToVgmOptions,
) -> Result<()> {
    let package = read_mdx_package(input, pdx)?;
    let pdx_path = resolve_pdx_path(input, pdx, package.mdx.header.pdx_name.as_deref());
    if !logger.is_noop() {
        let _ = logger.info(format_args!("MDX:"));
        let mut table = Table::new();
        table.load_style(NOTHING);
        table.set_content_arrangement(ContentArrangement::Dynamic);
        table.set_header(vec![Cell::new("Field"), Cell::new("Value")]);
        table.add_row(vec![
            Cell::new("title"),
            Cell::new(package.mdx.header.title.clone()),
        ]);
        table.add_row(vec![
            Cell::new("pdx_name"),
            Cell::new(package.mdx.header.pdx_name.as_deref().unwrap_or("(none)")),
        ]);
        table.add_row(vec![
            Cell::new("pdx_file"),
            Cell::new(match pdx_path {
                Some(path) => format!(
                    "{} ({})",
                    path.display(),
                    if path.is_file() {
                        "exists"
                    } else {
                        "not found"
                    }
                ),
                None => "(none)".to_string(),
            }),
        ]);
        if let Some(pdx) = package.pdx.as_ref() {
            table.add_row(vec![
                Cell::new("pdx_banks"),
                Cell::new(pdx.banks.len().to_string()),
            ]);
        }
        table.add_row(vec![
            Cell::new("tracks"),
            Cell::new(package.mdx.tracks.len().to_string()),
        ]);
        let _ = logger.info(format_args!("{table}"));
    }
    let document = to_vgm_document(&package, options)
        .map_err(|error| anyhow::anyhow!("MDX to VGM conversion failed: {error:?}"))?;
    let document = if package.mdx.header.title.is_empty() {
        document
    } else {
        let mut document = document;
        document.gd3 = Some(Gd3 {
            track_name_origin: Some(package.mdx.header.title.clone()),
            ..Gd3::default()
        });
        document
    };
    let bytes: Vec<u8> = (&document).into();
    let reparsed: soundlog::VgmDocument = (&bytes[..])
        .try_into()
        .with_context(|| format!("generated VGM failed to parse: {}", input.display()))?;
    if !logger.is_noop() {
        crate::cui::vgm::print_vgm_diag_table(&document, &reparsed, &logger);
    }
    Ok(())
}

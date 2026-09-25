use std::convert::TryInto;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use comfy_table::{Cell, ContentArrangement, Table, presets::NOTHING};
use mmlx::mdx::compat::normalize_mxdrv16y_tracks;
use soundlog::chip::state::{Okim6258State, Ym2151State};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document, to_vgm_stream_generator};
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::package::MdxPackage;
use soundlog::meta::Gd3;
use soundlog::vgm::VgmStream;
use soundlog::vgm::command::Instance;

use crate::logger::Logger;

/// Reads an MDX package, resolving its PDX sidecar when no explicit path was
/// supplied.
pub(crate) fn read_mdx_package(input: &Path, pdx: Option<&Path>) -> Result<MdxPackage> {
    let mdx_bytes = fs::read(input)
        .with_context(|| format!("failed to read MDX input: {}", input.display()))?;
    let mdx_bytes = normalize_mxdrv16y_tracks(&mdx_bytes)
        .map_err(|error| anyhow!("failed to normalize MDX input: {error}"))?;
    let mdx = MdxDocument::parse(mdx_bytes.as_ref())
        .map_err(|error| anyhow!("failed to parse MDX input: {error}"))?;

    let pdx_path = resolve_pdx_path(input, pdx, mdx.header.pdx_name.as_deref());
    let pdx_bytes = pdx_path
        .as_deref()
        .filter(|path| path.is_file())
        .map(|path| {
            fs::read(path).with_context(|| format!("failed to read PDX input: {}", path.display()))
        })
        .transpose()?;

    MdxPackage::parse(mdx_bytes.as_ref(), pdx_bytes.as_deref())
        .map_err(|error| anyhow!("failed to parse MDX package: {error}"))
}

fn resolve_pdx_path(input: &Path, pdx: Option<&Path>, pdx_name: Option<&str>) -> Option<PathBuf> {
    if let Some(path) = pdx {
        return Some(path.to_path_buf());
    }
    let name = pdx_name?;
    let directory = input
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let candidates = [
        directory.join(name),
        directory.join(format!("{name}.PDX")),
        directory.join(format!("{name}.pdx")),
    ];
    candidates
        .iter()
        .find(|candidate| candidate.is_file())
        .cloned()
        .or_else(|| find_case_insensitive_file(directory, &candidates))
        .or_else(|| candidates.into_iter().next())
}

fn find_case_insensitive_file(directory: &Path, candidates: &[PathBuf]) -> Option<PathBuf> {
    let entries = fs::read_dir(directory).ok()?;
    let candidate_names = candidates
        .iter()
        .filter_map(|candidate| candidate.file_name())
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .collect::<Vec<_>>();
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.is_file()
                && path
                    .file_name()
                    .map(|name| {
                        let name = name.to_string_lossy().to_ascii_lowercase();
                        candidate_names.iter().any(|candidate| candidate == &name)
                    })
                    .unwrap_or(false)
        })
}

/// Parse an MDX or MML file and print its track commands with source offsets.
pub fn parse_mdx(input: &Path, pdx: Option<&Path>, logger: Arc<Logger>) -> Result<()> {
    let mdx = if input
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mml"))
    {
        if pdx.is_some() {
            return Err(anyhow!("--pdx can only be used with MDX input"));
        }
        let source = fs::read_to_string(input)
            .with_context(|| format!("failed to read MML input: {}", input.display()))?;
        let parsed = mmlx::mdx::parse(&source)
            .map_err(|error| anyhow!("failed to parse MML input: {error}"))?;
        mmlx::mdx::compile(&parsed)
            .map_err(|error| anyhow!("failed to compile MML input: {error}"))?
    } else {
        read_mdx_package(input, pdx)?.mdx
    };
    let _ = logger.info(format_args!(
        "{:<8} {:<8} {:<8} {:<8} {}",
        "Track", "Index", "Offset", "Length", "Command"
    ));
    let source_map = mdx.sourcemap();
    for (track, commands) in mdx.tracks.iter().enumerate() {
        for (command_index, command) in commands.iter().enumerate() {
            let (offset, length) = source_map
                .get(track)
                .and_then(|track_map| track_map.get(command_index))
                .copied()
                .unwrap_or((0, 0));
            let _ = logger.info(format_args!(
                "{:<8} {:<8} 0x{:06x} {:<8} {:?}",
                track, command_index, offset, length, command
            ));
        }
    }
    Ok(())
}

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
        table.load_preset(NOTHING);
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
        .map_err(|error| anyhow!("MDX to VGM conversion failed: {error:?}"))?;
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

/// Convert an MDX file (lazily, via `VgmStream`/`VgmCallbackStream`) and
/// stream it with the same register-write/event log format as
/// `soundlog stream`. Exercises the lazy `VgmCommandGenerator` path end to end
/// without building a full `VgmDocument` up front.
pub fn stream_mdx(
    input: &Path,
    pdx: Option<&Path>,
    logger: Arc<Logger>,
    options: &MdxToVgmOptions,
) -> Result<()> {
    let package = read_mdx_package(input, pdx)?;
    let has_pcm = package.drives_okim6258();

    let generator = to_vgm_stream_generator(package, *options)
        .map_err(|error| anyhow!("MDX to VGM conversion failed: {error}"))?;
    let stream = VgmStream::from_generator(generator);

    crate::cui::stream::run_callback_stream(
        stream,
        logger,
        &input.display().to_string(),
        |callback_stream| {
            // No `VgmDocument`/header exists in the lazy path (see
            // `VgmStream::from_generator`), so track state manually using
            // the same clocks passed to the generator.
            callback_stream
                .track_state::<Ym2151State>(Instance::Primary, options.ym2151_clock as f32);
            if has_pcm {
                callback_stream
                    .track_state::<Okim6258State>(Instance::Primary, options.okim6258_clock as f32);
            }
        },
    )
}

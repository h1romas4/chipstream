use std::convert::TryInto;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use comfy_table::{Cell, ContentArrangement, Table, presets::NOTHING};
use soundlog::chip::state::{Okim6258State, Ym2151State};
use soundlog::mdx::command::MdxCommand;
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document, to_vgm_stream_generator};
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::parser::parse_mdx_command;
use soundlog::mdx::pcm_mixer::PCM8_OKIM6258_CLOCK_DIVIDER;
use soundlog::meta::Gd3;
use soundlog::vgm::VgmStream;
use soundlog::vgm::command::Instance;
use soundlog::vgm::header::Okim6258Flags;

use crate::logger::Logger;

/// Reads an MDX package, resolving its PDX sidecar when no explicit path was
/// supplied.
pub(crate) fn read_mdx_package(input: &Path, pdx: Option<&Path>) -> Result<MdxPackage> {
    let mdx_bytes = fs::read(input)
        .with_context(|| format!("failed to read MDX input: {}", input.display()))?;
    let mdx = MdxDocument::parse(&mdx_bytes)
        .map_err(|error| anyhow!("failed to parse MDX input: {error}"))?;

    let pdx_path = resolve_pdx_path(input, pdx, mdx.header.pdx_name.as_deref());
    let pdx_bytes = pdx_path
        .as_deref()
        .filter(|path| path.is_file())
        .map(|path| {
            fs::read(path).with_context(|| format!("failed to read PDX input: {}", path.display()))
        })
        .transpose()?;

    MdxPackage::parse_owned(mdx_bytes, pdx_bytes)
        .map_err(|error| anyhow!("failed to parse MDX package: {error}"))
}

/// Detects the MXDRV16y layout using the voice-area boundary heuristic from
/// NanoDriveX. Standard MDX keeps track entries before the voice data, while
/// MXDRV16y places a track entry after the inferred voice area.
#[allow(dead_code)]
fn detect_mxdrv16y(package: &MdxPackage) -> Result<bool> {
    let bytes = package.mdx.to_bytes();
    let voice_data_offset = package
        .mdx
        .header
        .tone_data_position()
        .ok_or_else(|| anyhow!("MDX voice data offset overflow"))?;
    let mut voice_data_end = bytes.len();
    let mut detected = false;

    for track in 0..package.mdx.header.track_count() {
        let Some(track_position) = package.mdx.header.track_position(track) else {
            continue;
        };
        if track_position >= voice_data_offset && track_position < voice_data_end {
            voice_data_end = track_position;
            detected = true;
        }
    }

    for track in 0..package.mdx.header.track_count() {
        let Some(mut position) = package.mdx.header.track_position(track) else {
            continue;
        };
        if position >= voice_data_offset {
            continue;
        }
        for _ in 0..64 {
            if position >= voice_data_offset {
                if position < voice_data_end {
                    voice_data_end = position;
                    detected = true;
                }
                break;
            }
            let Ok((command, length)) = parse_mdx_command(&bytes, position) else {
                break;
            };
            let next_position = position.saturating_add(length);
            if matches!(command, MdxCommand::Note(_) | MdxCommand::Rest(_)) {
                break;
            }
            if let MdxCommand::Jump(jump) = command {
                if jump.offset == 0 {
                    break;
                }
                let Some(jump_position) = next_position.checked_add_signed(jump.offset as isize)
                else {
                    break;
                };
                position = jump_position;
            } else {
                position = next_position;
            }
        }
    }

    Ok(detected && voice_data_end > voice_data_offset)
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

/// Converts an MDX file, optionally paired with its PDX file, into VGM.
pub fn mdx2vgm(
    input: &Path,
    output: &Path,
    pdx: Option<&Path>,
    options: &MdxToVgmOptions,
) -> Result<()> {
    let package = read_mdx_package(input, pdx)?;
    let mut options = *options;
    if !options.mxdrv16y && is_mxdrv16y_layout(&package)? {
        options.mxdrv16y = true;
    }
    let mut document = to_vgm_document(&package, &options)
        .map_err(|error| anyhow!("MDX to VGM conversion failed: {error:?}"))?;
    if !package.mdx.header.title.is_empty() {
        document.gd3 = Some(Gd3 {
            track_name_origin: Some(package.mdx.header.title.clone()),
            ..Gd3::default()
        });
    }
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

/// Detects MXDRV16y either through the voice-area heuristic or its characteristic
/// 16-track control track ending in an unconditional loop.
fn is_mxdrv16y_layout(package: &MdxPackage) -> Result<bool> {
    if detect_mxdrv16y(package)? {
        return Ok(true);
    }
    Ok(package.mdx.header.track_count() == 16
        && package
            .mdx
            .tracks
            .get(15)
            .and_then(|track| track.last())
            .is_some_and(|command| matches!(command, MdxCommand::EndOfTrackLoop(_))))
}

/// Parse an MDX file and print its track commands with source offsets.
pub fn parse_mdx(input: &Path, pdx: Option<&Path>, logger: Arc<Logger>) -> Result<()> {
    let package = read_mdx_package(input, pdx)?;
    let _ = logger.info(format_args!(
        "{:<8} {:<8} {:<8} {:<8} {}",
        "Track", "Index", "Offset", "Length", "Command"
    ));
    let source_map = package.mdx.sourcemap();
    for (track, commands) in package.mdx.tracks.iter().enumerate() {
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
    let mxdrv16y = options.mxdrv16y;
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
        table.add_row(vec![Cell::new("mxdrv16y"), Cell::new(mxdrv16y.to_string())]);
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
        crate::cui::vgm::print_vgm_diag_table(&document, &reparsed);
    }
    Ok(())
}

/// Convert an MDX file (lazily, via `VgmStream`/`VgmCallbackStream`) and
/// play it back with the same register-write/event log format as
/// `soundlog play`. Exercises the lazy `VgmCommandGenerator` path end to
/// end, unlike `mdx convert` which builds a full `VgmDocument` up front.
pub fn play_mdx(
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

    crate::cui::play::run_callback_stream(
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

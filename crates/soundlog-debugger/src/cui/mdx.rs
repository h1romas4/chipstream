use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
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
    options.mxdrv16y |= detect_mxdrv16y(&package)?;
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

/// Parse an MDX file and print its summary and every track command.
pub fn parse_mdx(input: &Path, pdx: Option<&Path>) -> Result<()> {
    let package = read_mdx_package(input, pdx)?;
    let pdx_path = resolve_pdx_path(input, pdx, package.mdx.header.pdx_name.as_deref());
    let mxdrv16y = detect_mxdrv16y(&package)?;

    println!("Title: {}", package.mdx.header.title);
    println!(
        "PDX: {}",
        package.mdx.header.pdx_name.as_deref().unwrap_or("(none)")
    );
    match pdx_path {
        Some(path) => println!(
            "PDX file: {} ({})",
            path.display(),
            if path.is_file() {
                "exists"
            } else {
                "not found"
            }
        ),
        None => println!("PDX file: (none)"),
    }
    println!("MXDRV16y: {mxdrv16y}");
    println!("Tracks: {}", package.mdx.tracks.len());
    println!("Tones: {}", package.mdx.tone_bank.tones.len());
    if let Some(pdx) = package.pdx.as_ref() {
        let (sample_count, sample_bytes) = pdx
            .banks
            .iter()
            .flat_map(|bank| bank.entries.iter().flatten())
            .fold((0usize, 0u64), |(count, bytes), sample| {
                (count + 1, bytes + u64::from(sample.size))
            });
        println!("PDX banks: {}", pdx.banks.len());
        println!("PDX samples: {sample_count} ({sample_bytes} bytes)");
    }
    let source_map = package.mdx.sourcemap();
    for (track, commands) in package.mdx.tracks.iter().enumerate() {
        println!("Track {track}: {} commands", commands.len());
        for (command_index, command) in commands.iter().enumerate() {
            let (offset, length) = source_map
                .get(track)
                .and_then(|track_map| track_map.get(command_index))
                .copied()
                .unwrap_or((0, 0));
            let bytes = command
                .to_mdx_bytes()
                .unwrap_or_default()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<Vec<_>>()
                .join(" ");
            println!("  [{command_index:04}] 0x{offset:06x} +{length:02}  {bytes:<17} {command:?}");
        }
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
    let mut options = *options;
    options.mxdrv16y |= detect_mxdrv16y(&package)?;

    let generator = to_vgm_stream_generator(package, options)
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

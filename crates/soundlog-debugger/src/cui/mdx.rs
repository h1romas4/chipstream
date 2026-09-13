use std::fs;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use soundlog::chip::state::{Okim6258State, Ym2151State};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_stream_generator};
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::VgmStream;
use soundlog::vgm::command::Instance;

use crate::logger::Logger;

/// Parse an MDX file and print its summary and every track command.
pub fn parse_mdx(input: &Path, pdx: Option<&Path>) -> Result<()> {
    let mdx_bytes = fs::read(input)
        .with_context(|| format!("failed to read MDX input: {}", input.display()))?;
    let pdx_bytes = pdx
        .map(|path| {
            fs::read(path).with_context(|| format!("failed to read PDX input: {}", path.display()))
        })
        .transpose()?;
    let package = MdxPackage::parse_owned(mdx_bytes, pdx_bytes)
        .map_err(|error| anyhow!("failed to parse MDX package: {error}"))?;

    println!("Title: {}", package.mdx.header.title);
    println!(
        "PDX: {}",
        package.mdx.header.pdx_name.as_deref().unwrap_or("(none)")
    );
    println!("Tracks: {}", package.mdx.tracks.len());
    println!("Tones: {}", package.mdx.tone_bank.tones.len());
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
    if let Some(pdx) = package.pdx.as_ref() {
        println!("PDX banks: {}", pdx.banks.len());
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
    let mdx_bytes = fs::read(input)
        .with_context(|| format!("failed to read MDX input: {}", input.display()))?;
    let pdx_bytes = pdx
        .map(|path| {
            fs::read(path).with_context(|| format!("failed to read PDX input: {}", path.display()))
        })
        .transpose()?;
    let package = MdxPackage::parse_owned(mdx_bytes, pdx_bytes)
        .map_err(|error| anyhow!("failed to parse MDX package: {error}"))?;
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

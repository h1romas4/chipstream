use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

use crate::logger::Logger;

use super::read_mdx_or_mml_package;

/// Parse an MDX or MML file and print its track commands with source offsets.
pub fn parse_mdx(input: &Path, pdx: Option<&Path>, logger: Arc<Logger>) -> Result<()> {
    let package = read_mdx_or_mml_package(input, pdx)?;
    let mdx = &package.mdx;
    let _ = logger.info(format_args!(
        "{:<8} {:<8} {:<8} {:<8} {}",
        "Track", "Index", "Offset", "Length", "Command"
    ));
    let source_map = mdx.sourcemap()?;
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

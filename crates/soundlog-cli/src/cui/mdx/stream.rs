use std::path::Path;
use std::sync::Arc;

use anyhow::{Result, anyhow};
use soundlog::chip::state::{Okim6258State, Ym2151State};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_stream_generator};
use soundlog::vgm::VgmStream;
use soundlog::vgm::command::Instance;

use crate::logger::Logger;

use super::read_mdx_or_mml_package;

/// Convert an MDX or MML file lazily and stream its events.
pub fn stream_mdx(
    input: &Path,
    pdx: Option<&Path>,
    logger: Arc<Logger>,
    options: &MdxToVgmOptions,
) -> Result<()> {
    let package = read_mdx_or_mml_package(input, pdx)?;
    let has_pcm = package.drives_okim6258();

    let generator = to_vgm_stream_generator(package, *options)
        .map_err(|error| anyhow!("MDX to VGM conversion failed: {error}"))?;
    let stream = VgmStream::from_generator(generator);

    crate::cui::stream::run_callback_stream(
        stream,
        logger,
        &input.display().to_string(),
        |callback_stream| {
            callback_stream
                .track_state::<Ym2151State>(Instance::Primary, options.ym2151_clock as f32);
            if has_pcm {
                callback_stream
                    .track_state::<Okim6258State>(Instance::Primary, options.okim6258_clock as f32);
            }
        },
    )
}

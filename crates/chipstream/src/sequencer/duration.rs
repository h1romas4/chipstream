use soundlog::mdx::convert::{
    MdxLoopMarker, MdxToVgmOptions, to_vgm_stream_generator,
    to_vgm_stream_generator_with_loop_marker,
};
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::VGM_SAMPLE_RATE;
use soundlog::vgm::command::VgmCommand;
use soundlog::vgm::header::VgmHeader;
use soundlog::vgm::stream::{StreamResult, VgmStream};

use crate::fixed::divide_ratio;

use super::{MdxSequencerError, VgmSequencerError};

const MAX_DURATION_COMMANDS: u64 = 10_000_000;

/// Measure finite VGM playback in output frames without rendering audio.
///
/// `data` must contain uncompressed VGM bytes. If `play_count` is `None`, a
/// VGM with a loop point is unbounded and returns `None`; one without a loop
/// point is measured to its natural end. Inputs requiring more than the
/// internal command-scan limit also return `None`.
pub fn measure_vgm_duration_frames(
    data: &[u8],
    output_rate: u32,
    play_count: Option<u32>,
    fadeout_samples: Option<u32>,
) -> Result<Option<u64>, VgmSequencerError> {
    let header = VgmHeader::from_bytes(data)?;
    let mut stream = VgmStream::from_vgm(data.to_vec())?;
    let command_start = VgmHeader::command_start(header.version, header.data_offset);
    let loop_position = usize::try_from(header.loop_offset)
        .ok()
        .and_then(|offset| 0x1c_usize.checked_add(offset));
    if play_count.is_none()
        && loop_position.is_some_and(|position| position >= command_start && position < data.len())
    {
        return Ok(None);
    }
    stream.set_loop_count(play_count);
    stream.set_fadeout_samples(
        fadeout_samples
            .filter(|samples| *samples > 0)
            .map(|n| n as usize),
    );
    measure_stream_frames(stream, output_rate, None, fadeout_samples)
}

/// Measure finite MDX playback in output frames without rendering audio.
///
/// The package is consumed to build a playback-equivalent command generator.
/// `options.loop_count: None` means unbounded playback and returns `None`.
/// Inputs requiring more than the internal command-scan limit also return
/// `None`.
pub fn measure_mdx_duration_frames(
    package: MdxPackage,
    output_rate: u32,
    options: MdxToVgmOptions,
    fadeout_samples: Option<u32>,
) -> Result<Option<u64>, MdxSequencerError> {
    if options.loop_count.is_none() {
        return Ok(None);
    }

    let fadeout_samples = fadeout_samples.filter(|samples| *samples > 0);
    let (generator, marker) = if fadeout_samples.is_some() && options.loop_count.is_some() {
        let (generator, marker) = to_vgm_stream_generator_with_loop_marker(package, options)
            .map_err(MdxSequencerError::Conversion)?;
        (generator, Some(marker))
    } else {
        (
            to_vgm_stream_generator(package, options).map_err(MdxSequencerError::Conversion)?,
            None,
        )
    };
    measure_stream_frames(
        VgmStream::from_generator(generator),
        output_rate,
        marker,
        fadeout_samples,
    )
    .map_err(MdxSequencerError::Playback)
}

fn measure_stream_frames(
    mut stream: VgmStream,
    output_rate: u32,
    marker: Option<MdxLoopMarker>,
    fadeout_samples: Option<u32>,
) -> Result<Option<u64>, VgmSequencerError> {
    if output_rate == 0 {
        return Err(VgmSequencerError::InvalidSampleRate);
    }
    let (whole, fraction) =
        divide_ratio(output_rate, VGM_SAMPLE_RATE, 1).ok_or(VgmSequencerError::DurationOverflow)?;
    let mut phase = 1 << 31;
    let mut total_frames = 0_u64;
    let mut fade_remaining = None;

    for _ in 0..MAX_DURATION_COMMANDS {
        let result = stream.next().ok_or(VgmSequencerError::UnexpectedEnd)??;
        if fade_remaining.is_none()
            && (stream.is_fading_out() || marker.as_ref().is_some_and(MdxLoopMarker::reached))
        {
            fade_remaining = fadeout_samples;
        }

        match result {
            StreamResult::EndOfStream => return Ok(Some(total_frames)),
            StreamResult::NeedsMoreData => return Err(VgmSequencerError::UnexpectedEnd),
            StreamResult::Command(VgmCommand::WaitSamples(wait)) => {
                let wait = u64::from(wait.0);
                let elapsed =
                    fade_remaining.map_or(wait, |remaining| wait.min(u64::from(remaining)));
                advance_frames(elapsed, whole, fraction, &mut phase, &mut total_frames)?;
                if let Some(remaining) = &mut fade_remaining {
                    *remaining -= elapsed as u32;
                    if *remaining == 0 {
                        return Ok(Some(total_frames));
                    }
                }
            }
            StreamResult::Command(_) => {}
        }
    }

    Ok(None)
}

fn advance_frames(
    samples: u64,
    whole: u32,
    fraction: u32,
    phase: &mut u32,
    total_frames: &mut u64,
) -> Result<(), VgmSequencerError> {
    let fractional_total = u128::from(*phase) + u128::from(samples) * u128::from(fraction);
    let frames = u128::from(samples) * u128::from(whole) + (fractional_total >> 32);
    let frames = u64::try_from(frames).map_err(|_| VgmSequencerError::DurationOverflow)?;
    *total_frames = total_frames
        .checked_add(frames)
        .ok_or(VgmSequencerError::DurationOverflow)?;
    *phase = fractional_total as u32;
    Ok(())
}

#[cfg(test)]
mod tests {
    use soundlog::mdx::package::MdxPackage;
    use soundlog::vgm::VgmBuilder;
    use soundlog::vgm::command::WaitSamples;

    use super::{measure_mdx_duration_frames, measure_vgm_duration_frames};
    use crate::audio::StereoBlock;
    use crate::sequencer::{MdxSequencer, MdxSequencerOptions, VgmSequencer};

    fn rendered_frames<E: std::fmt::Debug>(
        mut render: impl FnMut() -> Result<Option<StereoBlock>, E>,
    ) -> u64 {
        let mut frames = 0_u64;
        while let Some(block) = render().unwrap() {
            frames += block.frames().len() as u64;
        }
        frames
    }

    #[test]
    fn vgm_duration_matches_rendered_output_with_loop_and_fadeout() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        builder.add_vgm_command(WaitSamples(3_000));
        builder.set_loop_index(1);
        let bytes: Vec<u8> = builder.finalize().into();
        let mut player = VgmSequencer::from_vgm(bytes.clone(), 48_000, 512).unwrap();
        player.set_loop_count(Some(2));
        player.set_fadeout_samples(Some(2_205)).unwrap();
        let rendered = rendered_frames(|| player.render());

        assert_eq!(
            measure_vgm_duration_frames(&bytes, 48_000, Some(2), Some(2_205)).unwrap(),
            Some(rendered)
        );
    }

    #[test]
    fn mdx_duration_matches_rendered_output_with_loop_and_fadeout() {
        let bytes = include_bytes!("../../../soundlog/assets/mdx/mdx_fm_loop.mdx");
        let package = MdxPackage::parse(bytes, None).unwrap();
        let options = MdxSequencerOptions {
            loop_count: Some(2),
            ..MdxSequencerOptions::default()
        };
        let mut player = MdxSequencer::with_options_and_fadeout(
            package.clone(),
            48_000,
            512,
            options,
            Some(2_205),
        )
        .unwrap();
        let rendered = rendered_frames(|| player.render());

        assert_eq!(
            measure_mdx_duration_frames(package, 48_000, options, Some(2_205)).unwrap(),
            Some(rendered)
        );
    }

    #[test]
    fn unbounded_loop_duration_is_unknown() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        builder.set_loop_index(0);
        let bytes: Vec<u8> = builder.finalize().into();
        assert_eq!(
            measure_vgm_duration_frames(&bytes, 48_000, None, None).unwrap(),
            None
        );
    }

    #[test]
    fn non_looping_vgm_with_unlimited_count_ends_naturally() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        let bytes: Vec<u8> = builder.finalize().into();
        assert_eq!(
            measure_vgm_duration_frames(&bytes, 48_000, None, None).unwrap(),
            Some(1_088)
        );
    }
}

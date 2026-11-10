//! Bounded playback-duration measurement without generating PCM audio.

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

use super::{MdxSequencerError, MdxSequencerOptions, PlaybackOptions, VgmSequencerError};

const MAX_DURATION_COMMANDS: u32 = 10_000_000;
const MAX_UNLIMITED_MDX_DURATION_COMMANDS: u32 = 100_000;

/// Result of a bounded playback-duration scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaybackDuration {
    /// Playback ends after this many output frames.
    Finite(u32),
    /// Playback was observed repeating without a loop limit, barring errors.
    Unbounded,
    /// The scan limit was reached or the maximum frame count was exceeded.
    Unknown,
}

impl PlaybackDuration {
    /// Return the frame count only when a finite duration was measured.
    pub fn finite_frames(self) -> Option<u32> {
        match self {
            Self::Finite(frames) => Some(frames),
            Self::Unbounded | Self::Unknown => None,
        }
    }
}

/// Measure VGM playback in output frames without rendering audio.
///
/// `data` must contain uncompressed VGM bytes. If `options.loop_count` is `None`, a
/// VGM that reaches a loop point is unbounded; one without a loop point is
/// measured to its natural end. A scan that reaches its limit or exceeds
/// `u32::MAX` output frames is unknown.
pub fn measure_vgm_duration_frames(
    data: &[u8],
    output_rate: u32,
    options: PlaybackOptions,
) -> Result<PlaybackDuration, VgmSequencerError> {
    let fadeout_samples = super::options::fadeout_samples(options.fadeout)?;
    let header = VgmHeader::from_bytes(data)?;
    let mut stream = VgmStream::from_vgm(data.to_vec())?;
    let command_start = VgmHeader::command_start(header.version, header.data_offset);
    let loop_position = usize::try_from(header.loop_offset)
        .ok()
        .and_then(|offset| 0x1c_usize.checked_add(offset));
    let detect_loop = options.loop_count.is_none()
        && header.loop_offset != 0
        && loop_position.is_some_and(|position| position >= command_start && position < data.len());
    stream.set_loop_count(options.loop_count);
    stream.set_fadeout_samples(
        fadeout_samples
            .filter(|samples| *samples > 0)
            .map(|n| n as usize),
    );
    measure_stream_frames(
        stream,
        output_rate,
        None,
        fadeout_samples,
        detect_loop,
        MAX_DURATION_COMMANDS,
    )
}

/// Measure MDX playback in output frames without rendering audio.
///
/// The package is consumed to build a playback-equivalent command generator.
/// An unlimited loop count does not imply the track actually loops; if it
/// does not end before the scan limit, the result is unknown. The limit is
/// lower for unlimited looping to bound work. Driver errors, including resource
/// limits, are returned as errors rather than an unknown duration. Durations
/// exceeding `u32::MAX` output frames are unknown.
pub fn measure_mdx_duration_frames(
    package: MdxPackage,
    output_rate: u32,
    options: MdxSequencerOptions,
) -> Result<PlaybackDuration, MdxSequencerError> {
    let fadeout_samples = super::options::fadeout_samples(options.playback.fadeout)
        .map_err(MdxSequencerError::Playback)?;
    let options: MdxToVgmOptions = options.into();
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
        false,
        if options.loop_count.is_none() {
            MAX_UNLIMITED_MDX_DURATION_COMMANDS
        } else {
            MAX_DURATION_COMMANDS
        },
    )
    .map_err(MdxSequencerError::Playback)
}

/// Scan waits and loop markers up to the command limit without rendering audio.
fn measure_stream_frames(
    mut stream: VgmStream,
    output_rate: u32,
    marker: Option<MdxLoopMarker>,
    fadeout_samples: Option<u32>,
    detect_loop: bool,
    max_commands: u32,
) -> Result<PlaybackDuration, VgmSequencerError> {
    if output_rate == 0 {
        return Err(VgmSequencerError::InvalidSampleRate);
    }
    let (whole, fraction) =
        divide_ratio(output_rate, VGM_SAMPLE_RATE, 1).ok_or(VgmSequencerError::DurationOverflow)?;
    let mut phase = 1 << 31;
    let mut total_frames = 0_u32;
    let mut fade_remaining = None;

    for _ in 0..max_commands {
        let result = stream.next().ok_or(VgmSequencerError::UnexpectedEnd)??;
        if detect_loop && stream.current_loop_count() > 0 {
            return Ok(PlaybackDuration::Unbounded);
        }
        if fade_remaining.is_none()
            && (stream.is_fading_out() || marker.as_ref().is_some_and(MdxLoopMarker::reached))
        {
            fade_remaining = fadeout_samples;
        }

        match result {
            StreamResult::EndOfStream => return Ok(PlaybackDuration::Finite(total_frames)),
            StreamResult::NeedsMoreData => return Err(VgmSequencerError::UnexpectedEnd),
            StreamResult::Command(VgmCommand::WaitSamples(wait)) => {
                let wait = u32::from(wait.0);
                let elapsed = fade_remaining.map_or(wait, |remaining| wait.min(remaining));
                if advance_frames(elapsed, whole, fraction, &mut phase, &mut total_frames).is_none()
                {
                    return Ok(PlaybackDuration::Unknown);
                }
                if let Some(remaining) = &mut fade_remaining {
                    *remaining -= elapsed;
                    if *remaining == 0 {
                        return Ok(PlaybackDuration::Finite(total_frames));
                    }
                }
            }
            StreamResult::Command(_) => {}
        }
    }

    Ok(PlaybackDuration::Unknown)
}

/// Accumulate output frames for a VGM wait using fixed-point timing.
fn advance_frames(
    samples: u32,
    whole: u32,
    fraction: u32,
    phase: &mut u32,
    total_frames: &mut u32,
) -> Option<()> {
    let (fractional_frames, fractional_phase) = multiply_u32_wide(samples, fraction);
    let (fractional_phase, carry) = fractional_phase.overflowing_add(*phase);
    let fractional_frames = fractional_frames.checked_add(u32::from(carry))?;
    let frames = samples.checked_mul(whole)?.checked_add(fractional_frames)?;
    *total_frames = total_frames.checked_add(frames)?;
    *phase = fractional_phase;
    Some(())
}

/// Return the high and low words of a 32-bit product.
fn multiply_u32_wide(a: u32, b: u32) -> (u32, u32) {
    let a_low = a & 0xffff;
    let a_high = a >> 16;
    let b_low = b & 0xffff;
    let b_high = b >> 16;

    let product = a_low * b_low;
    let low_low = product & 0xffff;
    let product = a_high * b_low + (product >> 16);
    let middle_low = product & 0xffff;
    let high = product >> 16;
    let product = a_low * b_high + middle_low;
    let high = a_high * b_high + high + (product >> 16);
    let low = (product << 16) | low_low;
    (high, low)
}

#[cfg(test)]
mod tests {
    use soundlog::mdx::package::MdxPackage;
    use soundlog::vgm::VgmBuilder;
    use soundlog::vgm::command::WaitSamples;

    use super::{PlaybackDuration, measure_mdx_duration_frames, measure_vgm_duration_frames};
    use crate::audio::StereoBlock;
    use crate::sequencer::{MdxSequencer, MdxSequencerOptions, PlaybackOptions, VgmSequencer};

    #[test]
    fn u32_multiplication_keeps_the_high_and_low_words() {
        assert_eq!(
            super::multiply_u32_wide(u32::MAX, u32::MAX),
            (u32::MAX - 1, 1)
        );
        assert_eq!(super::multiply_u32_wide(0x8000_0000, 2), (1, 0));
        assert_eq!(
            super::multiply_u32_wide(u32::MAX, 0x1_0000),
            (0xffff, 0xffff_0000)
        );
    }

    #[test]
    fn duration_exceeding_u32_output_frames_is_unknown() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(u16::MAX));
        let bytes: Vec<u8> = builder.finalize().into();

        assert_eq!(
            measure_vgm_duration_frames(&bytes, u32::MAX, PlaybackOptions::default()).unwrap(),
            PlaybackDuration::Unknown
        );
    }

    #[test]
    fn duration_accumulation_handles_u32_boundary() {
        let mut phase = 0;
        let mut total_frames = u32::MAX - 1;
        assert_eq!(
            super::advance_frames(1, 1, 0, &mut phase, &mut total_frames),
            Some(())
        );
        assert_eq!(total_frames, u32::MAX);
        assert_eq!(
            super::advance_frames(1, 1, 0, &mut phase, &mut total_frames),
            None
        );
    }

    fn rendered_frames<E: std::fmt::Debug>(
        mut render: impl FnMut() -> Result<Option<StereoBlock>, E>,
    ) -> u32 {
        let mut frames = 0_u32;
        while let Some(block) = render().unwrap() {
            frames = frames
                .checked_add(u32::try_from(block.frames().len()).unwrap())
                .unwrap();
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
        let options = PlaybackOptions {
            loop_count: Some(2),
            fadeout: Some(std::time::Duration::from_millis(50)),
        };
        player.set_playback_options(options).unwrap();
        let rendered = rendered_frames(|| player.render());

        assert_eq!(
            measure_vgm_duration_frames(&bytes, 48_000, options).unwrap(),
            PlaybackDuration::Finite(rendered)
        );
    }

    #[test]
    fn mdx_duration_matches_rendered_output_with_loop_and_fadeout() {
        let bytes = include_bytes!("../../../soundlog/assets/mdx/mdx_fm_loop.mdx");
        let package = MdxPackage::parse(bytes, None).unwrap();
        let options = MdxSequencerOptions {
            playback: crate::sequencer::PlaybackOptions {
                loop_count: Some(2),
                fadeout: Some(std::time::Duration::from_millis(50)),
            },
            ..MdxSequencerOptions::default()
        };
        let mut player = MdxSequencer::with_options(package.clone(), 48_000, 512, options).unwrap();
        let rendered = rendered_frames(|| player.render());

        assert_eq!(
            measure_mdx_duration_frames(package, 48_000, options).unwrap(),
            PlaybackDuration::Finite(rendered)
        );
    }

    #[test]
    fn unbounded_loop_duration_is_detected() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        builder.set_loop_index(0);
        let bytes: Vec<u8> = builder.finalize().into();
        assert_eq!(
            measure_vgm_duration_frames(
                &bytes,
                48_000,
                PlaybackOptions {
                    loop_count: None,
                    fadeout: None
                }
            )
            .unwrap(),
            PlaybackDuration::Unbounded
        );
        assert!(matches!(
            measure_vgm_duration_frames(
                &bytes,
                0,
                PlaybackOptions {
                    loop_count: None,
                    fadeout: None
                }
            ),
            Err(super::VgmSequencerError::InvalidSampleRate)
        ));
    }

    #[test]
    fn non_looping_vgm_with_unlimited_count_ends_naturally() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        let bytes: Vec<u8> = builder.finalize().into();
        assert_eq!(
            measure_vgm_duration_frames(
                &bytes,
                48_000,
                PlaybackOptions {
                    loop_count: None,
                    fadeout: None
                }
            )
            .unwrap(),
            PlaybackDuration::Finite(1_088)
        );
    }

    #[test]
    fn non_looping_mdx_with_unlimited_count_ends_naturally() {
        use soundlog::mdx::command::MdxRest;
        use soundlog::mdx::document::MdxBuilder;

        let mut builder = MdxBuilder::new();
        builder.add_mdx_command(0, MdxRest { ticks: 1 });
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        let options = MdxSequencerOptions {
            playback: crate::sequencer::PlaybackOptions {
                loop_count: None,
                fadeout: None,
            },
            ..Default::default()
        };
        let mut player = MdxSequencer::with_options(package.clone(), 48_000, 512, options).unwrap();
        let rendered = rendered_frames(|| player.render());
        assert_eq!(
            measure_mdx_duration_frames(package, 48_000, options).unwrap(),
            PlaybackDuration::Finite(rendered)
        );
    }

    #[test]
    fn playback_options_apply_loop_count_and_duration() {
        use crate::sequencer::PlaybackOptions;
        use std::time::Duration;

        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1_000));
        builder.set_loop_index(0);
        let bytes: Vec<u8> = builder.finalize().into();
        let mut player = VgmSequencer::from_vgm(bytes.clone(), 48_000, 512).unwrap();
        let options = PlaybackOptions {
            loop_count: Some(2),
            fadeout: Some(Duration::from_millis(50)),
        };
        player.set_playback_options(options).unwrap();
        let rendered = rendered_frames(|| player.render());
        assert_eq!(
            measure_vgm_duration_frames(&bytes, 48_000, options).unwrap(),
            PlaybackDuration::Finite(rendered)
        );
        assert!(
            player
                .set_playback_options(PlaybackOptions::default())
                .is_err()
        );
        assert!(super::super::options::fadeout_samples(Some(Duration::MAX)).is_err());
    }

    #[test]
    fn scan_limit_is_unknown_not_unbounded() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1));
        builder.add_vgm_command(WaitSamples(1));
        let stream = soundlog::vgm::VgmStream::from_document(builder.finalize());
        assert_eq!(
            super::measure_stream_frames(stream, 44_100, None, None, false, 1).unwrap(),
            PlaybackDuration::Unknown
        );
    }

    #[test]
    fn unlimited_mdx_loop_is_unknown_when_the_scan_limit_is_reached() {
        let bytes = include_bytes!("../../../soundlog/assets/mdx/mdx_fm_loop.mdx");
        let package = MdxPackage::parse(bytes, None).unwrap();
        let options = MdxSequencerOptions {
            playback: PlaybackOptions {
                loop_count: None,
                fadeout: None,
            },
            max_ticks: None,
            ..Default::default()
        };
        assert_eq!(
            measure_mdx_duration_frames(package, 44_100, options).unwrap(),
            PlaybackDuration::Unknown
        );
    }
}

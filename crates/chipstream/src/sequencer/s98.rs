//! Incremental S98 playback through the VGM rendering engine.

use std::ops::{Deref, DerefMut};

use soundlog::s98::{S98Document, S98PlaybackOptions, S98VgmGenerator};
use soundlog::vgm::{VgmCommandGenerator, VgmStream};

use super::factory::ChipFactoryRegistry;
use super::vgm::config::ChipConfiguration;
use super::vgm::{FadeMarker, StreamFactory};
use super::{PlaybackOptions, VgmSequencer, VgmSequencerError};

/// Loop, fade, and resource limits for S98 playback.
#[derive(Debug, Clone, Copy)]
pub struct S98SequencerOptions {
    /// Loop count and fade duration.
    pub playback: PlaybackOptions,
    /// Maximum total S98 ticks, including repeats and fade.
    pub max_ticks: Option<u64>,
    /// Maximum source steps without generating a sample wait.
    pub max_commands_without_wait: Option<u32>,
}

impl Default for S98SequencerOptions {
    fn default() -> Self {
        let defaults = S98PlaybackOptions::default();
        Self {
            playback: PlaybackOptions::default(),
            max_ticks: defaults.max_ticks,
            max_commands_without_wait: defaults.max_commands_without_wait,
        }
    }
}

impl From<S98SequencerOptions> for S98PlaybackOptions {
    fn from(options: S98SequencerOptions) -> Self {
        Self {
            loop_count: options.playback.loop_count,
            max_ticks: options.max_ticks,
            max_commands_without_wait: options.max_commands_without_wait,
        }
    }
}

pub(super) fn s98_generator(
    document: S98Document,
    options: S98PlaybackOptions,
    mark_loop: bool,
) -> Result<(Box<dyn VgmCommandGenerator>, Option<FadeMarker>), soundlog::ParseError> {
    if mark_loop {
        let (generator, marker) = S98VgmGenerator::new_with_loop_marker(document, options)?;
        Ok((Box::new(generator), Some(FadeMarker::S98(marker))))
    } else {
        Ok((Box::new(S98VgmGenerator::new(document, options)?), None))
    }
}

/// A device-independent S98 player producing stereo PCM blocks.
///
/// The underlying [`VgmSequencer`] methods are available through dereferencing.
/// Rendering and checkpoint restoration may allocate and are not real-time safe.
pub struct S98Sequencer<'a> {
    player: VgmSequencer<'a>,
}

impl<'a> S98Sequencer<'a> {
    /// Create a player from a parsed S98 document.
    pub fn new(
        document: S98Document,
        output_rate: u32,
        chunk_frames: usize,
        options: S98SequencerOptions,
    ) -> Result<Self, VgmSequencerError> {
        Self::new_with_factories(
            document,
            output_rate,
            chunk_frames,
            options,
            ChipFactoryRegistry::new(),
        )
    }

    /// Create a player with selected external chip factories.
    pub fn new_with_factories(
        document: S98Document,
        output_rate: u32,
        chunk_frames: usize,
        options: S98SequencerOptions,
        factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        let fade = super::options::fadeout_samples(options.playback.fadeout)?
            .filter(|samples| *samples != 0);
        let generator_options = options.into();
        let mark_loop = fade.is_some() && options.playback.loop_count.is_some();
        let replay_document = document.clone();
        let (generator, marker, header): (Box<dyn VgmCommandGenerator>, _, _) = if mark_loop {
            let (generator, marker) =
                S98VgmGenerator::new_with_loop_marker(document, generator_options)?;
            let header = generator.vgm_header().clone();
            (Box::new(generator), Some(FadeMarker::S98(marker)), header)
        } else {
            let generator = S98VgmGenerator::new(document, generator_options)?;
            let header = generator.vgm_header().clone();
            (Box::new(generator), None, header)
        };
        let stream_factory: StreamFactory = Box::new(move || {
            let (generator, marker) =
                s98_generator(replay_document.clone(), generator_options, mark_loop)?;
            Ok((VgmStream::from_generator(generator), marker))
        });
        let config = ChipConfiguration::from_header(&header, None, true);
        let mut player = VgmSequencer::from_generator_parts(
            generator,
            Some(stream_factory),
            config,
            output_rate,
            chunk_frames,
            factories,
        )?;
        player.configure_fadeout(fade, marker)?;
        Ok(Self { player })
    }
}

impl<'a> Deref for S98Sequencer<'a> {
    type Target = VgmSequencer<'a>;
    fn deref(&self) -> &Self::Target {
        &self.player
    }
}

impl DerefMut for S98Sequencer<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.player
    }
}

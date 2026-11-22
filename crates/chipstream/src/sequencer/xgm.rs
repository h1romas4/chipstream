//! Incremental XGM v1 playback through the VGM rendering engine.

use soundlog::vgm::{VgmCommandGenerator, VgmStream};
use soundlog::xgm::{XgmDocument, XgmPlaybackOptions, XgmVgmGenerator};

use crate::audio::StereoBlock;

use super::factory::ChipFactoryRegistry;
use super::vgm::config::ChipConfiguration;
use super::vgm::{FadeMarker, StreamFactory};
use super::{
    Chip, Instance, NativeOutput, PlaybackOptions, StateEvent, VgmSequencer,
    VgmSequencerCheckpoint, VgmSequencerError, VgmStateTracker, VgmWriteCallbackTarget,
};

/// Track, loop, fade, and resource limits for XGM playback.
#[derive(Debug, Clone, Copy)]
pub struct XgmSequencerOptions {
    /// Loop count and fade duration.
    pub playback: PlaybackOptions,
    /// Zero-based track index.
    pub track: usize,
    /// Maximum total XGM frames, including loops and fade; `None` disables the limit.
    pub max_frames: Option<u32>,
    /// Maximum XGM commands between frame waits; `None` disables the limit.
    pub max_commands_per_frame: Option<u32>,
}

impl Default for XgmSequencerOptions {
    fn default() -> Self {
        let defaults = XgmPlaybackOptions::default();
        Self {
            playback: PlaybackOptions::default(),
            track: defaults.track,
            max_frames: defaults.max_frames,
            max_commands_per_frame: defaults.max_commands_per_frame,
        }
    }
}

impl From<XgmSequencerOptions> for XgmPlaybackOptions {
    fn from(options: XgmSequencerOptions) -> Self {
        Self {
            track: options.track,
            loop_count: options.playback.loop_count,
            max_frames: options.max_frames,
            max_commands_per_frame: options.max_commands_per_frame,
        }
    }
}

pub(super) fn xgm_generator(
    document: XgmDocument,
    options: XgmPlaybackOptions,
    mark_loop: bool,
) -> Result<(Box<dyn VgmCommandGenerator>, Option<FadeMarker>), soundlog::ParseError> {
    if mark_loop {
        let (generator, marker) = XgmVgmGenerator::new_with_loop_marker(document, options)?;
        Ok((Box::new(generator), Some(FadeMarker::Xgm(marker))))
    } else {
        Ok((Box::new(XgmVgmGenerator::new(document, options)?), None))
    }
}

/// A device-independent XGM player producing stereo PCM blocks.
///
/// Playback and PCM mixing follow soundlog's XGM generator. A finite loop
/// count with a nonzero fade continues from the loop boundary while fading.
/// Set `max_frames` to `None` for playback without a total frame limit.
/// Rendering and checkpoint restoration may allocate and are not real-time safe.
pub struct XgmSequencer<'a> {
    player: VgmSequencer<'a>,
}

impl<'a> XgmSequencer<'a> {
    /// Create a player from a parsed document.
    pub fn new(
        document: XgmDocument,
        output_rate: u32,
        chunk_frames: usize,
        options: XgmSequencerOptions,
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
        document: XgmDocument,
        output_rate: u32,
        chunk_frames: usize,
        options: XgmSequencerOptions,
        factories: ChipFactoryRegistry,
    ) -> Result<Self, VgmSequencerError> {
        let fade = super::options::fadeout_samples(options.playback.fadeout)?
            .filter(|samples| *samples != 0);
        let generator_options = options.into();
        let mark_loop = fade.is_some() && options.playback.loop_count.is_some();
        let header = document.header.timing.vgm_header();
        let replay_document = document.clone();
        let (generator, marker) = xgm_generator(document, generator_options, mark_loop)?;
        let stream_factory: StreamFactory = Box::new(move || {
            let (generator, marker) =
                xgm_generator(replay_document.clone(), generator_options, mark_loop)
                    .map_err(|error| error.to_string())?;
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

    /// Return the number of output frames emitted so far.
    pub fn position_frames(&self) -> usize {
        self.player.position_frames()
    }

    /// Return the fade start position once its loop boundary has been reached.
    pub fn fade_start_frame(&self) -> Option<usize> {
        self.player.fade_start_frame()
    }

    /// Return playable chip instances.
    pub fn chip_instances(&self) -> impl Iterator<Item = (Chip, Instance)> + '_ {
        self.player.chip_instances()
    }

    /// Set a chip's post-resampling gain.
    pub fn set_chip_gain(
        &mut self,
        chip: Chip,
        instance: Instance,
        gain: f32,
    ) -> Result<(), VgmSequencerError> {
        self.player.set_chip_gain(chip, instance, gain)
    }

    /// Return the native output layout for a chip instance.
    pub fn output_layout(
        &self,
        chip: Chip,
        instance: Instance,
    ) -> Result<&'static [NativeOutput], VgmSequencerError> {
        self.player.output_layout(chip, instance)
    }

    /// Set the gains for a chip's native outputs.
    pub fn set_output_gains(
        &mut self,
        chip: Chip,
        instance: Instance,
        gains: &[f32],
    ) -> Result<(), VgmSequencerError> {
        self.player.set_output_gains(chip, instance, gains)
    }

    /// Transform typed chip writes before they reach the chip.
    pub fn filter_write<C, F>(&mut self, filter: F)
    where
        C: VgmWriteCallbackTarget,
        F: FnMut(Instance, C, usize) -> C + 'a,
    {
        self.player.filter_write(filter);
    }

    /// Enable tracking using each matching voice's effective clock.
    pub fn track_state_with_voice_clocks<S: VgmStateTracker>(&mut self) {
        self.player.track_state_with_voice_clocks::<S>();
    }

    /// Register a callback for writes producing tracked state changes.
    pub fn on_state_change<C, F>(&mut self, callback: F)
    where
        C: VgmWriteCallbackTarget,
        F: FnMut(Instance, usize, Vec<StateEvent>) + 'a,
    {
        self.player.on_state_change::<C, F>(callback);
    }

    /// Report and clear whether state events were dropped.
    pub fn take_state_event_overflow(&mut self) -> bool {
        self.player.take_state_event_overflow()
    }

    /// Capture the exact state at an output-block boundary.
    pub fn checkpoint(&mut self) -> Result<VgmSequencerCheckpoint, VgmSequencerError> {
        self.player.checkpoint()
    }

    /// Restore a checkpoint created by this player.
    ///
    /// After a restore error, create a new sequencer.
    pub fn restore_checkpoint(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
    ) -> Result<(), VgmSequencerError> {
        self.player.restore_checkpoint(checkpoint)
    }

    /// Restore a checkpoint and advance to an output-frame position.
    pub fn seek_to_frame(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
        target: usize,
    ) -> Result<(), VgmSequencerError> {
        self.player.seek_to_frame(checkpoint, target)
    }

    /// Restore a checkpoint and seek while allowing cancellation.
    pub fn seek_to_frame_with_cancel(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
        target: usize,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), VgmSequencerError> {
        self.player
            .seek_to_frame_with_cancel(checkpoint, target, is_cancelled)
    }

    /// Return the next block, or `None` after playback finishes.
    pub fn render(&mut self) -> Result<Option<StereoBlock>, VgmSequencerError> {
        self.player.render()
    }
}

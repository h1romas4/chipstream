//! Incremental MDX playback and stereo audio rendering.
//!
//! [`MdxSequencer`] drives a soundlog MDX command generator through the shared
//! VGM playback engine without retaining a converted VGM. The caller supplies
//! optional PDX data; playback with PDX requires the `okim6258` feature.

use std::borrow::Cow;
use std::error::Error;
use std::fmt;

use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::convert::{
    AdpcmMode, MdxConvertError, MdxToVgmOptions, to_vgm_stream_generator,
    to_vgm_stream_generator_with_loop_marker,
};
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::{VgmHeader, VgmStream};

use crate::audio::{NativeEffect, StereoBlock, StereoEffect};

use super::factory::ChipFactoryRegistry;
use super::vgm::config::ChipConfiguration;
use super::vgm::{FadeMarker, StreamFactory};
use super::{
    Chip, EffectId, Instance, NativeOutput, OutputGroupId, PlaybackOptions, StateEvent,
    VgmSequencer, VgmSequencerCheckpoint, VgmSequencerError, VgmStateTracker,
    VgmWriteCallbackTarget,
};

/// Playback and driver options for the incremental MDX sequencer.
///
/// Defaults play one traversal with no fade. Conversion to soundlog's
/// `MdxToVgmOptions` transfers driver settings and loop count, but not the
/// sequencer's audio fade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxSequencerOptions {
    /// Loop count and fade duration, shared with VGM playback.
    pub playback: PlaybackOptions,
    /// YM2151 master clock in Hz.
    pub ym2151_clock: u32,
    /// OKIM6258 master clock in Hz.
    pub okim6258_clock: u32,
    /// ADPCM processing mode.
    pub adpcm_mode: AdpcmMode,
    /// Maximum elapsed driver ticks; `None` disables the limit.
    pub max_ticks: Option<u32>,
    /// Maximum commands per tick; `None` disables the limit.
    pub max_commands_per_tick: Option<u32>,
    /// Maximum decoded PCM samples cached by the driver; `None` disables the limit.
    pub max_decoded_pcm_samples: Option<u32>,
}

impl Default for MdxSequencerOptions {
    /// Build options from the MDX driver's defaults.
    fn default() -> Self {
        let driver = MdxToVgmOptions::default();
        Self {
            playback: PlaybackOptions::default(),
            ym2151_clock: driver.ym2151_clock,
            okim6258_clock: driver.okim6258_clock,
            adpcm_mode: driver.adpcm_mode,
            max_ticks: driver.max_ticks,
            max_commands_per_tick: driver.max_commands_per_tick,
            max_decoded_pcm_samples: driver.max_decoded_pcm_samples,
        }
    }
}

impl From<MdxSequencerOptions> for MdxToVgmOptions {
    /// Transfer MDX driver settings and loop count to the converter.
    fn from(options: MdxSequencerOptions) -> Self {
        Self {
            ym2151_clock: options.ym2151_clock,
            okim6258_clock: options.okim6258_clock,
            adpcm_mode: options.adpcm_mode,
            loop_count: options.playback.loop_count,
            max_ticks: options.max_ticks,
            max_commands_per_tick: options.max_commands_per_tick,
            max_decoded_pcm_samples: options.max_decoded_pcm_samples,
            ..Self::default()
        }
    }
}

/// Failure to parse, initialize, or render an MDX package.
///
/// Matching this error outside chipstream requires a fallback arm:
///
/// ```compile_fail
/// use chipstream::sequencer::MdxSequencerError;
///
/// fn describe(error: MdxSequencerError) {
///     match error {
///         MdxSequencerError::Compatibility(_) => {}
///         MdxSequencerError::Parse(_) => {}
///         MdxSequencerError::Conversion(_) => {}
///         MdxSequencerError::Playback(_) => {}
///     }
/// }
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub enum MdxSequencerError {
    /// MDX compatibility normalization failed.
    Compatibility(String),
    /// MDX or PDX parsing failed.
    Parse(soundlog::ParseError),
    /// MDX driver initialization or options failed.
    Conversion(MdxConvertError),
    /// The underlying sequencer failed.
    Playback(VgmSequencerError),
}

impl fmt::Display for MdxSequencerError {
    /// Format the MDX playback failure.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Compatibility(error) => {
                write!(f, "MDX compatibility normalization failed: {error}")
            }
            Self::Parse(error) => write!(f, "MDX/PDX parsing failed: {error}"),
            Self::Conversion(error) => write!(f, "MDX playback initialization failed: {error}"),
            Self::Playback(error) => write!(f, "MDX playback failed: {error}"),
        }
    }
}

impl Error for MdxSequencerError {
    /// Return the underlying parse, conversion, or playback error.
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Compatibility(_) => None,
            Self::Parse(error) => Some(error),
            Self::Conversion(error) => Some(error),
            Self::Playback(error) => Some(error),
        }
    }
}

/// A device-independent MDX player producing stereo f32 PCM blocks.
///
/// The MDX driver generates commands incrementally, including song loops and
/// PCM8/PCM8A mixing. It does not retain a whole converted VGM. Rendering can
/// read ahead and allocate; it is not guaranteed real-time safe.
/// A supplied PDX requires the `okim6258` Cargo feature, even if the song
/// does not use PCM notes; without a PDX the PCM path is disabled.
///
/// Legacy ADPCM and PCM8/PCM8A use soundlog's OKIM6258 re-encoding path.
/// Embedded fadeout does not attenuate legacy ADPCM in the default `Through`
/// mode; `Resample` and `Lpf` apply it. See [`soundlog::mdx::convert`] for
/// driver behavior and limits. Output is not clipped.
///
/// [`Self::on_write`] reports typed chip writes while rendering, with optional
/// state-change data registered through [`Self::track_state`].
/// After a rendering or checkpoint restore error, create a new sequencer.
pub struct MdxSequencer<'a> {
    /// VGM-based playback engine driven by the MDX generator.
    player: VgmSequencer<'a>,
}

impl<'a> MdxSequencer<'a> {
    /// Parse owned MDX and optional PDX buffers with the specified options.
    ///
    /// PDX lookup is the caller's responsibility. A missing PDX is accepted,
    /// even if the MDX names one, and disables the PCM path.
    /// The PDX parser can reuse an uncompressed input buffer. MDX is parsed
    /// into commands rather than retained as its original buffer. Decompression
    /// or MXDRV16y track-stub normalization may allocate additional buffers.
    pub fn from_mdx(
        mdx: Vec<u8>,
        pdx: Option<Vec<u8>>,
        output_rate: u32,
        chunk_frames: usize,
        options: MdxSequencerOptions,
    ) -> Result<Self, MdxSequencerError> {
        Self::from_mdx_with_factories(
            mdx,
            pdx,
            output_rate,
            chunk_frames,
            options,
            ChipFactoryRegistry::new(),
        )
    }

    /// Parse owned MDX/PDX buffers and use selected external chip factories.
    pub fn from_mdx_with_factories(
        mdx: Vec<u8>,
        pdx: Option<Vec<u8>>,
        output_rate: u32,
        chunk_frames: usize,
        options: MdxSequencerOptions,
        factories: ChipFactoryRegistry,
    ) -> Result<Self, MdxSequencerError> {
        let normalized =
            match normalize_mdx_for_playback(&mdx).map_err(MdxSequencerError::Compatibility)? {
                Cow::Borrowed(_) => mdx,
                Cow::Owned(normalized) => normalized,
            };
        let package = MdxPackage::parse_owned(normalized, pdx).map_err(MdxSequencerError::Parse)?;
        Self::new_with_factories(package, output_rate, chunk_frames, options, factories)
    }

    /// Play a parsed package with explicit driver options.
    ///
    /// The MDX driver applies `playback.loop_count`: `Some(n)` selects `n`
    /// traversals, with zero treated as one. Finite nested repeats are
    /// unaffected. Set `max_ticks` to `None` for an indefinite loop.
    ///
    /// Fade starts at the selected loop boundary, shared by synchronized F1
    /// tracks, and playback continues during the fade. Embedded fades or
    /// natural endings can finish playback earlier.
    ///
    /// The default tick limit is 100,000 (about 17 minutes at tempo t120).
    /// Command-per-tick and decoded-PCM limits are independently configurable.
    pub fn new(
        package: MdxPackage,
        output_rate: u32,
        chunk_frames: usize,
        options: MdxSequencerOptions,
    ) -> Result<Self, MdxSequencerError> {
        Self::new_with_factories(
            package,
            output_rate,
            chunk_frames,
            options,
            ChipFactoryRegistry::new(),
        )
    }

    /// Play a parsed package with explicit options and external chip factories.
    pub fn new_with_factories(
        package: MdxPackage,
        output_rate: u32,
        chunk_frames: usize,
        options: MdxSequencerOptions,
        factories: ChipFactoryRegistry,
    ) -> Result<Self, MdxSequencerError> {
        let fadeout_samples = super::options::fadeout_samples(options.playback.fadeout)
            .map_err(MdxSequencerError::Playback)?;
        let options: MdxToVgmOptions = options.into();
        if options.ym2151_clock == 0 || options.ym2151_clock > 0x3fff_ffff {
            return Err(MdxSequencerError::Conversion(
                MdxConvertError::InvalidOptions("YM2151 clock must be in 1..=0x3fffffff Hz"),
            ));
        }
        let has_pcm = package.drives_okim6258();
        if has_pcm && !(1024..=0x3fff_ffff).contains(&options.okim6258_clock) {
            return Err(MdxSequencerError::Conversion(
                MdxConvertError::InvalidOptions("OKIM6258 clock must be in 1024..=0x3fffffff Hz"),
            ));
        }
        let fadeout_samples = fadeout_samples.filter(|samples| *samples != 0);
        let replay_package = package.clone();
        let (generator, marker) = if fadeout_samples.is_some() && options.loop_count.is_some() {
            let (generator, marker) = to_vgm_stream_generator_with_loop_marker(package, options)
                .map_err(MdxSequencerError::Conversion)?;
            (generator, Some(FadeMarker::Mdx(marker)))
        } else {
            (
                to_vgm_stream_generator(package, options).map_err(MdxSequencerError::Conversion)?,
                None,
            )
        };
        let stream_factory: StreamFactory = Box::new(move || {
            if fadeout_samples.is_some() && options.loop_count.is_some() {
                let (generator, marker) =
                    to_vgm_stream_generator_with_loop_marker(replay_package.clone(), options)
                        .map_err(soundlog::ParseError::from)?;
                Ok((
                    VgmStream::from_generator(generator),
                    Some(FadeMarker::Mdx(marker)),
                ))
            } else {
                let generator = to_vgm_stream_generator(replay_package.clone(), options)
                    .map_err(soundlog::ParseError::from)?;
                Ok((VgmStream::from_generator(generator), None))
            }
        });
        let mut header = VgmHeader {
            ym2151_clock: options.ym2151_clock,
            okim6258_clock: if has_pcm { options.okim6258_clock } else { 0 },
            ..VgmHeader::default()
        };
        header.okim6258_flags.clock_divider = soundlog::mdx::pcm_mixer::PCM8_OKIM6258_CLOCK_DIVIDER;
        let mut config = ChipConfiguration::from_header(&header, None, false);
        if has_pcm {
            // Legacy MDX PCM rate commands can switch between these clocks.
            for clock in [4_000_000_u32, 8_000_000_u32] {
                config.observe_clock(&Chip::Okim6258, Instance::Primary, clock);
            }
        }
        let mut player = VgmSequencer::from_generator_parts(
            generator,
            Some(stream_factory),
            config,
            output_rate,
            chunk_frames,
            factories,
        )
        .map_err(MdxSequencerError::Playback)?;
        player
            .configure_fadeout(fadeout_samples, marker)
            .map_err(MdxSequencerError::Playback)?;
        Ok(Self { player })
    }

    /// Change a chip's post-resampling mix gain. See [`VgmSequencer::set_chip_gain`].
    pub fn set_chip_gain(
        &mut self,
        chip: Chip,
        instance: Instance,
        gain: f32,
    ) -> Result<(), MdxSequencerError> {
        self.player
            .set_chip_gain(chip, instance, gain)
            .map_err(MdxSequencerError::Playback)
    }

    /// Return the native output layout for a chip instance.
    pub fn output_layout(
        &self,
        chip: Chip,
        instance: Instance,
    ) -> Result<&'static [NativeOutput], MdxSequencerError> {
        self.player
            .output_layout(chip, instance)
            .map_err(MdxSequencerError::Playback)
    }

    /// Set native output gains. See [`VgmSequencer::set_output_gains`].
    pub fn set_output_gains(
        &mut self,
        chip: Chip,
        instance: Instance,
        gains: &[f32],
    ) -> Result<(), MdxSequencerError> {
        self.player
            .set_output_gains(chip, instance, gains)
            .map_err(MdxSequencerError::Playback)
    }

    /// Set or clear native-output pan. See [`VgmSequencer::set_output_pan`].
    pub fn set_output_pan(
        &mut self,
        chip: Chip,
        instance: Instance,
        output: usize,
        pan: Option<f32>,
    ) -> Result<(), MdxSequencerError> {
        self.player
            .set_output_pan(chip, instance, output, pan)
            .map_err(MdxSequencerError::Playback)
    }

    /// Append a native-output effect. See [`VgmSequencer::add_output_effect`].
    pub fn add_output_effect<T: NativeEffect + Clone + 'static>(
        &mut self,
        chip: Chip,
        instance: Instance,
        output: usize,
        effect: T,
    ) -> Result<EffectId<T>, MdxSequencerError> {
        self.player
            .add_output_effect(chip, instance, output, effect)
            .map_err(MdxSequencerError::Playback)
    }

    /// Create a stereo output group. See [`VgmSequencer::create_output_group`].
    pub fn create_output_group(
        &mut self,
        chip: Chip,
        instance: Instance,
        outputs: &[usize],
    ) -> Result<OutputGroupId, MdxSequencerError> {
        self.player
            .create_output_group(chip, instance, outputs)
            .map_err(MdxSequencerError::Playback)
    }

    /// Remove an output group. See [`VgmSequencer::remove_output_group`].
    pub fn remove_output_group(&mut self, group: &OutputGroupId) -> Result<(), MdxSequencerError> {
        self.player
            .remove_output_group(group)
            .map_err(MdxSequencerError::Playback)
    }

    /// Append a stereo group effect. See [`VgmSequencer::add_group_effect`].
    pub fn add_group_effect<T: StereoEffect + Clone + 'static>(
        &mut self,
        group: &OutputGroupId,
        effect: T,
    ) -> Result<EffectId<T>, MdxSequencerError> {
        self.player
            .add_group_effect(group, effect)
            .map_err(MdxSequencerError::Playback)
    }

    /// Access an installed effect. See [`VgmSequencer::effect_mut`].
    pub fn effect_mut<T: 'static>(
        &mut self,
        id: &EffectId<T>,
    ) -> Result<&mut T, MdxSequencerError> {
        self.player
            .effect_mut(id)
            .map_err(MdxSequencerError::Playback)
    }

    /// Remove an effect. See [`VgmSequencer::remove_effect`].
    pub fn remove_effect<T>(&mut self, id: &EffectId<T>) -> Result<(), MdxSequencerError> {
        self.player
            .remove_effect(id)
            .map_err(MdxSequencerError::Playback)
    }

    /// Reorder an effect within its chain. See [`VgmSequencer::move_effect`].
    pub fn move_effect<T>(
        &mut self,
        id: &EffectId<T>,
        position: usize,
    ) -> Result<(), MdxSequencerError> {
        self.player
            .move_effect(id, position)
            .map_err(MdxSequencerError::Playback)
    }

    /// Return the playable chip instances in this sequencer.
    pub fn chip_instances(&self) -> impl Iterator<Item = (Chip, Instance)> + '_ {
        self.player.chip_instances()
    }

    /// Transform a typed chip write immediately before it reaches its chip.
    ///
    /// See [`VgmSequencer::filter_write`] for timing, observer, and checkpoint
    /// semantics. Registering another filter for the same type replaces it.
    pub fn filter_write<C, F>(&mut self, filter: F)
    where
        C: VgmWriteCallbackTarget,
        F: FnMut(Instance, C, usize) -> C + 'a,
    {
        self.player.filter_write(filter);
    }

    /// Register a typed chip-write callback.
    ///
    /// The callback receives the instance, typed write, absolute output-frame
    /// position at this sequencer's sample rate, and state changes when state
    /// tracking is enabled. Register callbacks before rendering.
    pub fn on_write<C, F>(&mut self, callback: F)
    where
        C: VgmWriteCallbackTarget,
        F: FnMut(Instance, C, usize, Option<Vec<StateEvent>>) + 'a,
    {
        self.player.on_write(callback);
    }

    /// Register a callback only for writes that produced tracked state changes.
    ///
    /// Enable the corresponding tracker before rendering. This replaces any
    /// existing callback for the same write type. When its bounded event
    /// buffer fills, events are dropped without stopping playback; check
    /// [`Self::take_state_event_overflow`] after rendering.
    pub fn on_state_change<C, F>(&mut self, callback: F)
    where
        C: VgmWriteCallbackTarget,
        F: FnMut(Instance, usize, Vec<StateEvent>) + 'a,
    {
        self.player.on_state_change::<C, F>(callback);
    }

    /// Report and clear whether any state-only events were dropped.
    pub fn take_state_event_overflow(&mut self) -> bool {
        self.player.take_state_event_overflow()
    }

    /// Enable state tracking for a chip type and instance.
    pub fn track_state<S>(&mut self, instance: Instance, clock: f32)
    where
        S: VgmStateTracker,
    {
        self.player.track_state::<S>(instance, clock);
    }

    /// Enable state tracking using the effective clock of each matching voice.
    pub fn track_state_with_voice_clocks<S>(&mut self)
    where
        S: VgmStateTracker,
    {
        self.player.track_state_with_voice_clocks::<S>();
    }

    /// Return the current command-stream position in 44.1 kHz samples.
    ///
    /// MDX generators handle their own loops, so this position does not reset
    /// at a loop boundary. Command processing can run ahead of emitted PCM.
    pub fn current_sample(&self) -> usize {
        self.player.current_sample()
    }

    /// Return the number of output frames returned by this sequencer.
    ///
    /// This uses the configured output rate and advances monotonically across
    /// loops. It does not account for downstream buffering or device latency.
    pub fn position_frames(&self) -> usize {
        self.player.position_frames()
    }

    /// Return the output-frame position where the configured fade begins.
    ///
    /// The position is available once rendering has reached the loop boundary
    /// and can be discovered ahead of the returned PCM. It is absent before
    /// that boundary and for playback without a configured fade.
    pub fn fade_start_frame(&self) -> Option<usize> {
        self.player.fade_start_frame()
    }

    /// Capture the exact state at the current output-block boundary.
    pub fn checkpoint(&mut self) -> Result<VgmSequencerCheckpoint, MdxSequencerError> {
        self.player
            .checkpoint()
            .map_err(MdxSequencerError::Playback)
    }

    /// Restore a checkpoint created by this MDX sequencer.
    ///
    /// Any restore error permanently prevents further playback or restoration;
    /// create a new sequencer instead.
    pub fn restore_checkpoint(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
    ) -> Result<(), MdxSequencerError> {
        self.player
            .restore_checkpoint(checkpoint)
            .map_err(MdxSequencerError::Playback)
    }

    /// Restore a checkpoint and advance to an output-frame position.
    pub fn seek_to_frame(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
        target: usize,
    ) -> Result<(), MdxSequencerError> {
        self.player
            .seek_to_frame(checkpoint, target)
            .map_err(MdxSequencerError::Playback)
    }

    /// Restore a checkpoint and seek while allowing the caller to cancel.
    pub fn seek_to_frame_with_cancel(
        &mut self,
        checkpoint: &VgmSequencerCheckpoint,
        target: usize,
        is_cancelled: impl FnMut() -> bool,
    ) -> Result<(), MdxSequencerError> {
        self.player
            .seek_to_frame_with_cancel(checkpoint, target, is_cancelled)
            .map_err(MdxSequencerError::Playback)
    }

    /// Return the next block, or `None` after playback finishes.
    pub fn render(&mut self) -> Result<Option<StereoBlock>, MdxSequencerError> {
        self.player.render().map_err(MdxSequencerError::Playback)
    }
}

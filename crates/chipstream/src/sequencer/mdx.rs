//! Incremental MDX playback and stereo audio rendering.

use std::borrow::Cow;
use std::error::Error;
use std::fmt;

use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::convert::{
    AdpcmMode, MdxConvertError, MdxToVgmOptions, to_vgm_stream_generator,
    to_vgm_stream_generator_with_loop_marker,
};
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::VgmStream;

use crate::audio::StereoBlock;

use super::factory::ChipFactoryRegistry;
use super::vgm::StreamFactory;
use super::{
    Chip, Instance, PlaybackOptions, StateEvent, VgmSequencer, VgmSequencerCheckpoint,
    VgmSequencerError, VgmStateTracker, VgmWriteCallbackTarget,
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
#[derive(Debug)]
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
/// The parsed MDX/PDX, decoded sample cache, resampler, and output buffers
/// consume memory. Legacy ADPCM and PCM8/PCM8A use soundlog's driver and its
/// OKIM6258 re-encoding path. In the default `Through` mode, embedded fadeout
/// does not attenuate legacy ADPCM; `Resample` and `Lpf` use the software
/// mixer and apply it. Commands, timing, PCM handling, and limits follow
/// [`soundlog::mdx::convert`]. Output is not clipped.
///
/// [`Self::on_write`] reports typed chip writes while rendering, with optional
/// state-change data registered through [`Self::track_state`].
/// After a rendering error the sequencer cannot be resumed.
pub struct MdxSequencer<'a> {
    player: VgmSequencer<'a>,
}

impl<'a> MdxSequencer<'a> {
    /// Parse owned MDX and optional PDX buffers, and play once with default limits.
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
    ) -> Result<Self, MdxSequencerError> {
        Self::from_mdx_with_factories(
            mdx,
            pdx,
            output_rate,
            chunk_frames,
            ChipFactoryRegistry::new(),
        )
    }

    /// Parse owned MDX/PDX buffers and use selected external chip factories.
    pub fn from_mdx_with_factories(
        mdx: Vec<u8>,
        pdx: Option<Vec<u8>>,
        output_rate: u32,
        chunk_frames: usize,
        factories: ChipFactoryRegistry,
    ) -> Result<Self, MdxSequencerError> {
        let normalized =
            match normalize_mdx_for_playback(&mdx).map_err(MdxSequencerError::Compatibility)? {
                Cow::Borrowed(_) => mdx,
                Cow::Owned(normalized) => normalized,
            };
        let package = MdxPackage::parse_owned(normalized, pdx).map_err(MdxSequencerError::Parse)?;
        Self::new_with_factories(package, output_rate, chunk_frames, factories)
    }

    /// Play a parsed package once using the default playback limits.
    ///
    /// Equivalent to [`Self::with_options`] with default options.
    pub fn new(
        package: MdxPackage,
        output_rate: u32,
        chunk_frames: usize,
    ) -> Result<Self, MdxSequencerError> {
        Self::new_with_factories(
            package,
            output_rate,
            chunk_frames,
            ChipFactoryRegistry::new(),
        )
    }

    /// Play a parsed package once with selected external chip factories.
    pub fn new_with_factories(
        package: MdxPackage,
        output_rate: u32,
        chunk_frames: usize,
        factories: ChipFactoryRegistry,
    ) -> Result<Self, MdxSequencerError> {
        Self::with_options_with_factories(
            package,
            output_rate,
            chunk_frames,
            MdxSequencerOptions::default(),
            factories,
        )
    }

    /// Play a parsed package with explicit driver options.
    ///
    /// `playback.loop_count` is applied by the MDX driver, not the VGM stream. Set
    /// `max_ticks` to `None` for an indefinite loop without a time limit.
    /// `Some(n)` selects a finite number of traversals; `Some(0)` means one.
    /// For F1 loops, the intro and first traversal are followed by `n - 1`
    /// further traversals. Finite nested repeats are unaffected.
    /// A configured fade begins at the selected loop boundary. Synchronized
    /// F1 tracks use their shared boundary; other unbounded song loops use
    /// their own boundaries. Embedded fades or natural endings can finish
    /// playback earlier. The driver continues looping during the fade.
    ///
    /// The default tick limit is 100,000 (about 17 minutes at tempo t120).
    /// Command-per-tick and decoded-PCM limits are independently configurable.
    pub fn with_options(
        package: MdxPackage,
        output_rate: u32,
        chunk_frames: usize,
        options: MdxSequencerOptions,
    ) -> Result<Self, MdxSequencerError> {
        Self::with_options_with_factories(
            package,
            output_rate,
            chunk_frames,
            options,
            ChipFactoryRegistry::new(),
        )
    }

    /// Play a parsed package with explicit options and external chip factories.
    pub fn with_options_with_factories(
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
            (generator, Some(marker))
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
                        .map_err(|error| error.to_string())?;
                Ok((VgmStream::from_generator(generator), Some(marker)))
            } else {
                let generator = to_vgm_stream_generator(replay_package.clone(), options)
                    .map_err(|error| error.to_string())?;
                Ok((VgmStream::from_generator(generator), None))
            }
        });
        let mut player = VgmSequencer::from_mdx_stream(
            VgmStream::from_generator(generator),
            stream_factory,
            options.ym2151_clock,
            has_pcm.then_some(options.okim6258_clock),
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

    /// Change the gain of a chip, including already buffered PCM.
    pub fn set_gain(
        &mut self,
        chip: Chip,
        instance: Instance,
        gain: f32,
    ) -> Result<(), MdxSequencerError> {
        self.player
            .set_gain(chip, instance, gain)
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
    /// This follows [`VgmSequencer::current_sample`], including its loop-relative
    /// position and command-processing lookahead semantics.
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

//! Incremental MDX playback and stereo audio rendering.

use std::borrow::Cow;
use std::error::Error;
use std::fmt;

use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::convert::{
    MdxConvertError, to_vgm_stream_generator, to_vgm_stream_generator_with_loop_marker,
};
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::VgmStream;

use crate::audio::StereoBlock;

use super::factory::ChipFactoryRegistry;
use super::vgm::StreamFactory;
use super::{
    Chip, Instance, StateEvent, VgmSequencer, VgmSequencerCheckpoint, VgmSequencerError,
    VgmStateTracker, VgmWriteCallbackTarget,
};

/// Playback options for the incremental MDX driver.
pub use soundlog::mdx::convert::MdxToVgmOptions as MdxSequencerOptions;

/// Failure to parse, initialize, or render an MDX package.
#[derive(Debug)]
pub enum MdxSequencerError {
    Compatibility(String),
    Parse(soundlog::ParseError),
    Conversion(MdxConvertError),
    Playback(VgmSequencerError),
}

impl fmt::Display for MdxSequencerError {
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
    /// Parse owned MDX and optional PDX buffers, and play once with defaults.
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
        let options = MdxSequencerOptions {
            loop_count: Some(1),
            ..MdxSequencerOptions::default()
        };
        Self::with_options_and_fadeout_with_factories(
            package,
            output_rate,
            chunk_frames,
            options,
            None,
            factories,
        )
    }

    /// Play a parsed package with explicit driver options.
    ///
    /// `loop_count` is applied by the MDX driver, not the VGM stream. Set
    /// `max_ticks` to `None` for an indefinite loop without a time limit.
    /// Unlike [`Self::new`], the default options use `loop_count: None`.
    /// `Some(n)` selects a finite number of traversals; `Some(0)` means one.
    /// For F1 loops, the intro and first traversal are followed by `n - 1`
    /// further traversals. Finite nested repeats are unaffected.
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
        Self::with_options_and_fadeout_with_factories(
            package,
            output_rate,
            chunk_frames,
            options,
            None,
            factories,
        )
    }

    /// Play with a linear fade after the selected MDX loop boundary.
    ///
    /// The duration uses 44,100 Hz samples. Zero disables the fade; infinite
    /// playback and songs without a loop are unaffected. Embedded MDX
    /// fadeouts may end playback earlier. Synchronized F1 tracks use their
    /// shared boundary; an unbounded backward jump or count-zero repeat
    /// starts the fade as soon as it reaches the configured count.
    /// With other unbounded loops present, F1 tracks use independent track
    /// limits. The driver continues an unbounded repeat during the fade rather
    /// than playing the commands after it. Natural endings can finish earlier.
    pub fn with_options_and_fadeout(
        package: MdxPackage,
        output_rate: u32,
        chunk_frames: usize,
        options: MdxSequencerOptions,
        fadeout_samples: Option<u32>,
    ) -> Result<Self, MdxSequencerError> {
        Self::with_options_and_fadeout_with_factories(
            package,
            output_rate,
            chunk_frames,
            options,
            fadeout_samples,
            ChipFactoryRegistry::new(),
        )
    }

    /// Play with a fadeout and external factories for selected chip types.
    pub fn with_options_and_fadeout_with_factories(
        package: MdxPackage,
        output_rate: u32,
        chunk_frames: usize,
        options: MdxSequencerOptions,
        fadeout_samples: Option<u32>,
        factories: ChipFactoryRegistry,
    ) -> Result<Self, MdxSequencerError> {
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

    /// Enable state tracking for a chip type and instance.
    pub fn track_state<S>(&mut self, instance: Instance, clock: f32)
    where
        S: VgmStateTracker,
    {
        self.player.track_state::<S>(instance, clock);
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

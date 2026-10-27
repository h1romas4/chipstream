//! Capturing sound chip output as consistently configured native blocks.

use std::error::Error;
use std::fmt;

use soundlog::chip::Chip;

use crate::chip::{ChipError, SampleRate, SoundChip, StereoMute};

/// A chip's native output path, independent of any sequencer or mixer.
///
/// Each capture reflects the chip configuration at the start of that capture.
/// Apply register writes or configuration changes between captures so a block
/// never spans a change in rate or chip-controlled stereo mute.
pub struct ChipRoute<C> {
    chip: C,
}

impl<C: SoundChip> ChipRoute<C> {
    /// Create a route for an existing chip.
    pub fn new(chip: C) -> Self {
        Self { chip }
    }

    /// Access the chip without changing its state.
    pub fn chip(&self) -> &C {
        &self.chip
    }

    /// Access the chip to apply writes or change its configuration.
    ///
    /// ```compile_fail
    /// use chipstream::audio::ChipRoute;
    /// use chipstream::chip::{Okim6258, SoundChip};
    /// use soundlog::chip::PwmSpec;
    ///
    /// let mut route = ChipRoute::new(Okim6258::new(8_000_000).unwrap());
    /// route.chip_mut().write(PwmSpec { register: 1, value: 42 }).unwrap();
    /// ```
    pub fn chip_mut(&mut self) -> &mut C {
        &mut self.chip
    }

    /// Recover the chip and its current state.
    pub fn into_inner(self) -> C {
        self.chip
    }

    /// Generate native interleaved samples into caller-owned storage.
    ///
    /// The caller chooses how many complete native frames to generate and
    /// schedules captures around writes. The route itself does not allocate,
    /// convert sample rates, normalize, apply pan, mix, or process effects;
    /// the chip implementation may allocate. Those stages consume the
    /// returned samples and their metadata.
    ///
    /// Invalid metadata or buffer lengths are rejected before generating
    /// audio. On a chip error, the [`SoundChip::generate`] contract applies.
    pub fn capture<'a>(&mut self, buffer: &'a mut [i32]) -> Result<NativeBlock<'a>, RouteError> {
        let sample_rate = self.chip.sample_rate_ratio();
        let channels = self.chip.channels();
        if sample_rate.numerator() == 0 {
            return Err(RouteError::InvalidSampleRate);
        }
        if channels == 0 {
            return Err(RouteError::InvalidChannelCount);
        }
        if !buffer.len().is_multiple_of(channels) {
            return Err(RouteError::InvalidBufferLength {
                length: buffer.len(),
                channels,
            });
        }
        let chip_type = self.chip.chip_type().clone();
        let stereo_mute = self.chip.stereo_mute();
        self.chip.generate(buffer).map_err(RouteError::Chip)?;
        Ok(NativeBlock {
            chip_type,
            sample_rate,
            channels,
            stereo_mute,
            samples: buffer,
        })
    }
}

/// A single, consistently configured block of a chip's native output.
///
/// Samples are interleaved in the chip's native order and amplitude. No
/// assumption is made that multiple native outputs are left/right channels.
#[derive(Debug)]
pub struct NativeBlock<'a> {
    chip_type: Chip,
    sample_rate: SampleRate,
    channels: usize,
    stereo_mute: Option<StereoMute>,
    samples: &'a [i32],
}

impl NativeBlock<'_> {
    /// Chip type that generated this block.
    pub fn chip_type(&self) -> &Chip {
        &self.chip_type
    }

    /// Native frames per second for this block, rounded down to whole Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.floor_hz()
    }

    /// Native rate without rounding when the backend exposes a fractional rate.
    pub fn sample_rate_ratio(&self) -> SampleRate {
        self.sample_rate
    }

    /// Number of native output values per frame.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Separate chip-controlled stereo mute, if present.
    pub fn stereo_mute(&self) -> Option<StereoMute> {
        self.stereo_mute
    }

    /// Unmodified, interleaved native samples.
    pub fn samples(&self) -> &[i32] {
        self.samples
    }

    /// Number of complete native frames in this block.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }
}

/// Errors while capturing a chip's native output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    /// A chip reported a zero native sample rate.
    InvalidSampleRate,
    /// A chip reported zero native outputs per frame.
    InvalidChannelCount,
    /// The supplied buffer cannot hold a whole number of native frames.
    InvalidBufferLength { length: usize, channels: usize },
    /// The chip failed to generate the block.
    Chip(ChipError),
}

impl fmt::Display for RouteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate => write!(f, "chip reported a zero sample rate"),
            Self::InvalidChannelCount => write!(f, "chip reported zero output channels"),
            Self::InvalidBufferLength { length, channels } => write!(
                f,
                "buffer length {length} is not a multiple of {channels} channels"
            ),
            Self::Chip(error) => write!(f, "chip generation failed: {error}"),
        }
    }
}

impl Error for RouteError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Chip(error) => Some(error),
            _ => None,
        }
    }
}

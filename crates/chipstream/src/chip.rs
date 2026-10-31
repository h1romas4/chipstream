//! Native sound-chip interfaces and implementations.

#[cfg(feature = "c140")]
mod c140;
#[cfg(feature = "ndsif")]
pub mod ndsif;
#[cfg(any(feature = "ndsif", feature = "okim6258", feature = "okim6295"))]
mod oki_adpcm;
#[cfg(feature = "okim6258")]
mod okim6258;
#[cfg(feature = "okim6295")]
mod okim6295;
#[cfg(feature = "pwm")]
mod pwm;
#[cfg(feature = "segapcm")]
mod segapcm;
#[cfg(feature = "sn76496")]
mod sn76496;
mod write;
mod ymfm;

use std::any::Any;
use std::error::Error;
use std::fmt;
use std::num::NonZeroU32;

use soundlog::chip::Chip;

#[cfg(feature = "c140")]
pub use c140::{C140, C140Type};
#[cfg(feature = "ndsif")]
pub use ndsif::{
    NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent, NdsifOkim6258, NdsifRegisterEvent,
    NdsifSoundChip, NdsifTransport, register_okim6258_factory,
};
#[cfg(feature = "okim6258")]
pub use okim6258::{ClockDivider, Okim6258, OutputBits};
#[cfg(feature = "okim6295")]
pub use okim6295::Okim6295;
#[cfg(feature = "pwm")]
pub use pwm::Pwm;
#[cfg(feature = "segapcm")]
pub use segapcm::SegaPcm;
#[cfg(feature = "sn76496")]
pub use sn76496::{Sn76496, Sn76496Config};
pub use write::{ChipRegisterWrite, ChipWrite};
pub use ymfm::YmfmChip;

/// Chip-controlled stereo output mutes, separate from user pan or gain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StereoMute {
    pub left: bool,
    pub right: bool,
}

/// Native sample rate as a rational number of frames per second.
///
/// The numerator may be zero when a backend reports invalid metadata; a route
/// rejects such a rate. The denominator is always nonzero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleRate {
    numerator: u32,
    denominator: NonZeroU32,
}

impl SampleRate {
    /// Create an integer rate in frames per second.
    pub fn from_hz(hz: u32) -> Self {
        Self {
            numerator: hz,
            denominator: NonZeroU32::MIN,
        }
    }

    /// Create a rate as `numerator / denominator` frames per second.
    pub fn from_ratio(numerator: u32, denominator: NonZeroU32) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    /// Numerator in frames per second.
    pub fn numerator(self) -> u32 {
        self.numerator
    }

    /// Nonzero denominator.
    pub fn denominator(self) -> NonZeroU32 {
        self.denominator
    }

    /// Rate rounded down to whole frames per second.
    pub fn floor_hz(self) -> u32 {
        self.numerator / self.denominator.get()
    }
}

/// A sound chip producing native-rate, interleaved samples.
///
/// The output layout and amplitude are backend- and chip-specific. A frame
/// consists of [`channels`](Self::channels) consecutive `i32` samples. The
/// rate, channel count, and stereo mute must remain unchanged during a single
/// [`generate`](Self::generate) call.
pub trait SoundChip {
    /// The soundlog chip type represented by this instance.
    fn chip_type(&self) -> &Chip;

    /// Input clock in Hz.
    fn clock(&self) -> u32;

    /// Native output rate in frames per second.
    ///
    /// Rounded down to whole Hz. Some chips can change their rate through
    /// register writes or configuration.
    fn sample_rate(&self) -> u32;

    /// Native output rate without rounding where the backend exposes it.
    ///
    /// Implementations with fractional rates should override this method.
    /// The rounded-down value must agree with [`sample_rate`](Self::sample_rate).
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_hz(self.sample_rate())
    }

    /// Number of native output values per frame.
    fn channels(&self) -> usize;

    /// External stereo mutes controlled by the chip, if present.
    ///
    /// A chip's internal channel panning may instead already be reflected in
    /// its native output. `None` means no separate mute mask is required.
    fn stereo_mute(&self) -> Option<StereoMute> {
        None
    }

    /// Apply a soundlog register-write specification to a compatible chip.
    ///
    /// Writes take effect immediately. Concrete chip types reject incompatible
    /// specs at compile time. [`YmfmChip`] and `Box<dyn SoundChip>` can
    /// represent multiple chip types and return [`ChipError::ChipMismatch`]
    /// when the spec does not match the actual chip.
    ///
    /// Specs such as `C140Spec` and `SegaPcmSpec` preserve 16-bit addresses;
    /// `PwmSpec` preserves its full value. For multi-port chips, the spec's
    /// port selects the register bank. Custom chips can implement
    /// [`ChipWrite`] for additional write types.
    ///
    /// ```compile_fail
    /// use chipstream::chip::{SoundChip, YmfmChip};
    /// use soundlog::chip::Chip;
    ///
    /// let mut chip = YmfmChip::new(Chip::Ym2151, 4_000_000).unwrap();
    /// chip.write("not a register write").unwrap();
    /// ```
    fn write<S>(&mut self, spec: S) -> Result<(), ChipError>
    where
        Self: Sized + ChipWrite<S>,
    {
        self.apply_typed_write(spec)
    }

    /// Apply a typed register write through dynamic dispatch.
    ///
    /// Implementations must reject writes for a different chip without
    /// changing state.
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError>;

    /// Apply a typed write at an absolute output-frame position.
    ///
    /// The frame position uses `output_rate` frames per second. Ordinary
    /// emulators can use the default implementation; scheduled external
    /// backends can override it to enqueue writes at the corresponding time.
    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        _sample_frame: usize,
        _output_rate: u32,
    ) -> Result<(), ChipError> {
        self.apply_register_write(write)
    }

    /// Commit scheduled external events through an absolute output frame.
    ///
    /// Sequencers call this after all chip voices have rendered a block, so
    /// transports can order events from different voices before committing
    /// them to a device.
    fn flush_scheduled_until(
        &mut self,
        _sample_frame: usize,
        _output_rate: u32,
    ) -> Result<(), ChipError> {
        Ok(())
    }

    /// Finish a scheduled external stream at an absolute output frame.
    fn finish_scheduled(
        &mut self,
        _sample_frame: usize,
        _output_rate: u32,
    ) -> Result<(), ChipError> {
        Ok(())
    }

    /// Write a contiguous portion of a chip's sample ROM.
    ///
    /// Chips without sample ROM return an error. Existing bytes outside the
    /// written region are preserved.
    fn write_rom(&mut self, _start: usize, _data: &[u8]) -> Result<(), ChipError> {
        Err(ChipError::UnsupportedRom(self.chip_type().clone()))
    }

    /// Overwrite a whole number of native output frames.
    ///
    /// On an error, the buffer and chip state are unchanged.
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError>;

    /// Reset the emulated chip using the backend's reset operation.
    ///
    /// This does not necessarily recreate the chip or reset external callback
    /// state; exact reset behavior is backend- and chip-specific.
    fn reset(&mut self);

    /// Capture the complete emulated state for a later restore.
    ///
    /// External chip implementations may leave the default implementation in
    /// place when they do not support snapshots. Built-in backends capture
    /// their internal state and configuration. State held outside a backend
    /// by callbacks must be captured separately by its owner.
    fn save_state(&mut self) -> Result<Box<dyn SoundChipState>, ChipError> {
        Err(ChipError::UnsupportedStateSnapshot(
            self.chip_type().clone(),
        ))
    }

    /// Restore a state captured from the same concrete backend.
    fn restore_state(&mut self, _state: &dyn SoundChipState) -> Result<(), ChipError> {
        Err(ChipError::UnsupportedStateSnapshot(
            self.chip_type().clone(),
        ))
    }
}

/// Type-erased state captured from a [`SoundChip`].
pub trait SoundChipState: Any {
    /// Access the concrete state for backend-specific restoration.
    fn as_any(&self) -> &dyn Any;
}

impl<T: Any> SoundChipState for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(any(
    feature = "c140",
    feature = "okim6258",
    feature = "okim6295",
    feature = "pwm",
    feature = "segapcm",
    feature = "sn76496"
))]
pub(crate) fn save_cloned_state<T: Clone + Any>(state: &T) -> Box<dyn SoundChipState> {
    Box::new(state.clone())
}

#[cfg(any(
    feature = "c140",
    feature = "okim6258",
    feature = "okim6295",
    feature = "pwm",
    feature = "segapcm",
    feature = "sn76496"
))]
pub(crate) fn restore_cloned_state<T: Clone + Any>(
    current: &mut T,
    state: &dyn SoundChipState,
    chip: Chip,
) -> Result<(), ChipError> {
    let Some(state) = state.as_any().downcast_ref::<T>() else {
        return Err(ChipError::InvalidStateSnapshot(chip));
    };
    *current = state.clone();
    Ok(())
}

impl<T: SoundChip + ?Sized> SoundChip for Box<T> {
    fn chip_type(&self) -> &Chip {
        (**self).chip_type()
    }

    fn clock(&self) -> u32 {
        (**self).clock()
    }

    fn sample_rate(&self) -> u32 {
        (**self).sample_rate()
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        (**self).sample_rate_ratio()
    }

    fn channels(&self) -> usize {
        (**self).channels()
    }

    fn stereo_mute(&self) -> Option<StereoMute> {
        (**self).stereo_mute()
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        (**self).apply_register_write(write)
    }

    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        (**self).apply_register_write_at(write, sample_frame, output_rate)
    }

    fn flush_scheduled_until(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        (**self).flush_scheduled_until(sample_frame, output_rate)
    }

    fn finish_scheduled(&mut self, sample_frame: usize, output_rate: u32) -> Result<(), ChipError> {
        (**self).finish_scheduled(sample_frame, output_rate)
    }

    fn write_rom(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        (**self).write_rom(start, data)
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        (**self).generate(buffer)
    }

    fn reset(&mut self) {
        (**self).reset()
    }

    fn save_state(&mut self) -> Result<Box<dyn SoundChipState>, ChipError> {
        (**self).save_state()
    }

    fn restore_state(&mut self, state: &dyn SoundChipState) -> Result<(), ChipError> {
        (**self).restore_state(state)
    }
}

/// Errors creating or using a sound chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChipError {
    /// The backend does not implement this soundlog chip type.
    UnsupportedChip(Chip),
    /// The typed write belongs to a different chip.
    ChipMismatch { expected: Chip, actual: Chip },
    /// The chip has no writable sample ROM.
    UnsupportedRom(Chip),
    /// The sample ROM write exceeds the supported size.
    RomTooLarge,
    /// The input clock is too low to produce a nonzero native sample rate.
    InvalidClock { clock: u32, minimum: u32 },
    /// An external factory created a chip at a different clock than requested.
    ClockMismatch {
        chip: Chip,
        expected: u32,
        actual: u32,
    },
    /// An external backend failed with a backend-specific error.
    External { chip: Chip, message: String },
    /// A chip-specific configuration is invalid.
    InvalidConfiguration { chip: Chip, reason: &'static str },
    /// The chip backend could not create an instance.
    CreationFailed(Chip),
    /// This chip does not have the requested register bank.
    InvalidRegisterBank { chip: Chip, bank: u8 },
    /// This chip does not implement the requested register.
    InvalidRegister { chip: Chip, register: u8 },
    /// The register cannot accept the requested value.
    InvalidRegisterValue {
        chip: Chip,
        register: u8,
        value: u32,
    },
    /// The output buffer does not contain a whole number of frames.
    InvalidBufferLength { length: usize, channels: usize },
    /// The buffer is too large for the backend's native interface.
    BufferTooLarge { length: usize },
    /// This chip backend does not support state snapshots.
    UnsupportedStateSnapshot(Chip),
    /// The state snapshot belongs to a different chip backend.
    InvalidStateSnapshot(Chip),
}

impl fmt::Display for ChipError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedChip(chip) => write!(f, "unsupported chip: {chip:?}"),
            Self::ChipMismatch { expected, actual } => {
                write!(f, "write for {expected:?} cannot be applied to {actual:?}")
            }
            Self::UnsupportedRom(chip) => write!(f, "{chip:?} has no writable sample ROM"),
            Self::RomTooLarge => write!(f, "sample ROM exceeds the supported size"),
            Self::InvalidClock { clock, minimum } => {
                write!(f, "chip clock {clock} Hz is below the minimum {minimum} Hz")
            }
            Self::ClockMismatch {
                chip,
                expected,
                actual,
            } => write!(
                f,
                "{chip:?} backend clock is {actual} Hz, expected {expected} Hz"
            ),
            Self::External { chip, message } => {
                write!(f, "external {chip:?} backend failed: {message}")
            }
            Self::InvalidConfiguration { chip, reason } => {
                write!(f, "invalid {chip:?} configuration: {reason}")
            }
            Self::CreationFailed(chip) => write!(f, "failed to create chip: {chip:?}"),
            Self::InvalidRegisterBank { chip, bank } => {
                write!(f, "invalid register bank {bank} for {chip:?}")
            }
            Self::InvalidRegister { chip, register } => {
                write!(f, "invalid register 0x{register:02x} for {chip:?}")
            }
            Self::InvalidRegisterValue {
                chip,
                register,
                value,
            } => {
                write!(
                    f,
                    "invalid value 0x{value:x} for {chip:?} register 0x{register:02x}"
                )
            }
            Self::InvalidBufferLength { length, channels } => write!(
                f,
                "buffer length {length} is not a multiple of {channels} channels"
            ),
            Self::BufferTooLarge { length } => {
                write!(f, "buffer length {length} exceeds the backend limit")
            }
            Self::UnsupportedStateSnapshot(chip) => {
                write!(f, "{chip:?} does not support state snapshots")
            }
            Self::InvalidStateSnapshot(chip) => {
                write!(f, "state snapshot is incompatible with {chip:?}")
            }
        }
    }
}

impl Error for ChipError {}

#[cfg(any(feature = "c140", feature = "okim6295", feature = "segapcm"))]
fn write_rom_data(rom: &mut Vec<u8>, start: usize, data: &[u8]) -> Result<(), ChipError> {
    const MAX_ROM_BYTES: usize = 64 * 1024 * 1024;
    let end = start
        .checked_add(data.len())
        .filter(|end| *end <= MAX_ROM_BYTES)
        .ok_or(ChipError::RomTooLarge)?;
    if rom.len() < end {
        rom.resize(end, 0);
    }
    rom[start..end].copy_from_slice(data);
    Ok(())
}

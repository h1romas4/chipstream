use soundlog::chip::Chip;
use ymfm_sys::ffi::{self, ChipType};
use ymfm_sys::{ChipPtr, InterfaceCallbacks, InterfaceHandler};

use super::{ChipError, ChipRegisterWrite, SoundChip};

/// A native-rate Yamaha chip backed by ymfm-sys.
///
/// Default callbacks do not implement external memory, timers, busy state, or
/// IRQ handling. Use [`Self::with_handler`] when those features are required.
/// Sample-based chips need an external-read callback to play external data.
/// Callbacks and thread safety follow ymfm-sys; real-time safety and `Send`
/// are not guaranteed.
///
/// Supported chips are YM2151, YM2203, YM2413, YM2608, YM2610B, YM2612,
/// YM3526, Y8950, YM3812, YMF262, and YMF278B. Reset does not reset external
/// callback state and is not necessarily equivalent to creating a new chip;
/// for example, ymfm's YM2612 reset does not clear DAC state.
pub struct YmfmChip {
    chip_type: Chip,
    clock: u32,
    banks: u8,
    inner: ChipPtr,
}

impl YmfmChip {
    /// Create a chip using ymfm-sys's default callbacks.
    pub fn new(chip: Chip, clock: u32) -> Result<Self, ChipError> {
        Self::with_handler(chip, clock, InterfaceHandler::default())
    }

    /// Create a chip with callbacks for external data, timers, and I/O.
    ///
    /// Missing callbacks retain ymfm-sys's default behavior. The caller owns
    /// the backing data and timer policy used by the supplied handler.
    pub fn with_handler(
        chip: Chip,
        clock: u32,
        handler: InterfaceHandler,
    ) -> Result<Self, ChipError> {
        // These minima are the default ymfm sample-rate divisors. ymfm-sys
        // divides by the resulting rate while constructing the native chip.
        let (kind, minimum, banks) = match chip {
            Chip::Ym2151 => (ChipType::Ym2151, 64, 1),
            Chip::Ym2203 => (ChipType::Ym2203, 4, 1),
            Chip::Ym2413 => (ChipType::Ym2413, 72, 1),
            Chip::Ym2608 => (ChipType::Ym2608, 8, 2),
            Chip::Ym2610b => (ChipType::Ym2610B, 16, 2),
            Chip::Ym2612 => (ChipType::Ym2612, 144, 2),
            Chip::Ym3526 => (ChipType::Ym3526, 72, 1),
            Chip::Y8950 => (ChipType::Y8950, 72, 1),
            Chip::Ym3812 => (ChipType::Ym3812, 72, 1),
            Chip::Ymf262 => (ChipType::Ymf262, 288, 2),
            Chip::Ymf278b => (ChipType::Ymf278B, 768, 3),
            _ => return Err(ChipError::UnsupportedChip(chip)),
        };
        Self::create(chip, clock, handler, kind, minimum, banks)
    }

    /// Create a YM2610 or YM2610B with external-data callbacks.
    ///
    /// Both revisions are identified as `Chip::Ym2610b` by soundlog.
    pub fn with_ym2610_variant(
        clock: u32,
        is_b: bool,
        handler: InterfaceHandler,
    ) -> Result<Self, ChipError> {
        Self::create(
            Chip::Ym2610b,
            clock,
            handler,
            if is_b {
                ChipType::Ym2610B
            } else {
                ChipType::Ym2610
            },
            16,
            2,
        )
    }

    fn create(
        chip: Chip,
        clock: u32,
        handler: InterfaceHandler,
        kind: ChipType,
        minimum: u32,
        banks: u8,
    ) -> Result<Self, ChipError> {
        if clock < minimum {
            return Err(ChipError::InvalidClock { clock, minimum });
        }
        let inner = ffi::create_chip_with_callbacks(
            kind,
            clock,
            Box::new(InterfaceCallbacks::new(handler)),
        );
        if inner.is_null() {
            return Err(ChipError::CreationFailed(chip));
        }
        Ok(Self {
            chip_type: chip,
            clock,
            banks,
            inner,
        })
    }
}

impl SoundChip for YmfmChip {
    fn chip_type(&self) -> &Chip {
        &self.chip_type
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.inner.sample_rate()
    }

    fn channels(&self) -> usize {
        self.inner.channels() as usize
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        if bank >= self.banks {
            return Err(ChipError::InvalidRegisterBank {
                chip: self.chip_type.clone(),
                bank,
            });
        }
        let offset = u32::from(bank) * 2;
        self.inner.pin_mut().write(offset, register);
        self.inner.pin_mut().write(offset + 1, value);
        Ok(())
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if !buffer.len().is_multiple_of(self.channels()) {
            return Err(ChipError::InvalidBufferLength {
                length: buffer.len(),
                channels: self.channels(),
            });
        }
        if buffer.len() > u32::MAX as usize {
            return Err(ChipError::BufferTooLarge {
                length: buffer.len(),
            });
        }
        self.inner.pin_mut().generate(buffer);
        Ok(())
    }

    fn reset(&mut self) {
        self.inner.pin_mut().reset();
    }
}

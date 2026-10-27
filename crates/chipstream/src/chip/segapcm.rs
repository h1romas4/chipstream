//! Based on MAME's SegaPCM implementation, revision
//! `70743c6fb2602a5c2666c679b618706eabfca2ad`.
//! Copyright (c) Hiromitsu Shioya and Olivier Galibert.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

/// Sega 16-channel PCM chip with stereo native output.
///
/// Writes use 16-bit addresses. ROM writes load or update sample data;
/// unloaded addresses read as zero and reset preserves ROM. Bank shift and
/// mask can be configured with [`Self::with_banking`].
pub struct SegaPcm {
    clock: u32,
    bank_shift: u8,
    bank_mask: u8,
    ram: [u8; 0x800],
    fraction: [u8; 16],
    rom: Vec<u8>,
}

impl SegaPcm {
    /// Create a chip with the default bank shift and mask used by the port.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        Self::with_banking(clock, 12, 0x70)
    }

    /// Create a chip with the shift and mask from the VGM interface field.
    ///
    /// A zero mask selects the hardware's default mask, `0x70`.
    pub fn with_banking(clock: u32, bank_shift: u8, bank_mask: u8) -> Result<Self, ChipError> {
        if clock < 128 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 128,
            });
        }
        if bank_shift > 21 {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::SegaPcm,
                reason: "bank shift exceeds the 21-bit ROM address space",
            });
        }
        let mask = if bank_mask == 0 { 0x70 } else { bank_mask };
        Ok(Self {
            clock,
            bank_shift,
            bank_mask: mask & (0x1f_ffff_u32 >> bank_shift) as u8,
            ram: [0xff; 0x800],
            fraction: [0; 16],
            rom: Vec::new(),
        })
    }

    /// Read a register from the chip's mirrored 0x800-byte register RAM.
    pub fn read_register(&self, address: u16) -> u8 {
        self.ram[usize::from(address) & 0x7ff]
    }

    fn tick(&mut self) -> [i32; 2] {
        let mut output = [0; 2];
        for channel in 0..16 {
            let base = channel * 8;
            let control = self.ram[base + 0x86];
            if control & 1 != 0 {
                continue;
            }
            let mut address = (u32::from(self.ram[base + 0x85]) << 16)
                | (u32::from(self.ram[base + 0x84]) << 8)
                | u32::from(self.fraction[channel]);
            if address >> 16 == u32::from(self.ram[base + 6]) + 1 {
                if control & 2 != 0 {
                    self.ram[base + 0x86] |= 1;
                    self.fraction[channel] = 0;
                    continue;
                }
                address =
                    (u32::from(self.ram[base + 5]) << 16) | (u32::from(self.ram[base + 4]) << 8);
            }
            let bank = u32::from(control & self.bank_mask) << self.bank_shift;
            let sample_address = bank + (address >> 8);
            let sample =
                i32::from(self.rom.get(sample_address as usize).copied().unwrap_or(0)) - 128;
            output[0] += sample * i32::from(self.ram[base + 2] & 0x7f);
            output[1] += sample * i32::from(self.ram[base + 3] & 0x7f);
            address = (address + u32::from(self.ram[base + 7])) & 0xff_ffff;
            self.ram[base + 0x84] = (address >> 8) as u8;
            self.ram[base + 0x85] = (address >> 16) as u8;
            self.fraction[channel] = address as u8;
        }
        output
    }
}

impl SoundChip for SegaPcm {
    fn chip_type(&self) -> &Chip {
        &Chip::SegaPcm
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 128
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(128).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        let address = (usize::from(bank) << 8) | usize::from(register);
        self.ram[address & 0x7ff] = value;
        Ok(())
    }
    fn write_rom(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        write_rom_data(&mut self.rom, start, data)
    }
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if !buffer.len().is_multiple_of(2) {
            return Err(ChipError::InvalidBufferLength {
                length: buffer.len(),
                channels: 2,
            });
        }
        for frame in buffer.as_chunks_mut::<2>().0 {
            frame.copy_from_slice(&self.tick());
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.ram = [0xff; 0x800];
        self.fraction = [0; 16];
    }
}

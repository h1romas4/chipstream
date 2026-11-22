//! Based on MAME's X1-010 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Luca Elia.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const VOLUME_BASE: i32 = 2 * 32 * 256 / 30;

/// Seta X1-010 16-voice PCM and wavetable generator.
///
/// The 8 KiB register, envelope and waveform RAM is written through
/// [`SoundChip::write`]; sample ROM is supplied with [`SoundChip::write_rom`].
/// Unloaded ROM reads as zero. Reset clears RAM and playback positions but
/// preserves ROM. Out-of-range wavetable and envelope addresses are rejected
/// rather than accessing outside RAM.
#[derive(Clone)]
pub struct X1010 {
    clock: u32,
    ram: [u8; 0x2000],
    sample_offsets: [u32; 16],
    envelope_offsets: [u32; 16],
    rom: Vec<u8>,
}

impl X1010 {
    /// Create an X1-010 with the specified input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 512 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 512,
            });
        }
        Ok(Self {
            clock,
            ram: [0; 0x2000],
            sample_offsets: [0; 16],
            envelope_offsets: [0; 16],
            rom: Vec::new(),
        })
    }

    /// Read a byte of the chip's register and waveform RAM.
    pub fn read_register(&self, address: u16) -> Result<u8, ChipError> {
        self.ram
            .get(usize::from(address))
            .copied()
            .ok_or(ChipError::InvalidRegisterBank {
                chip: Chip::X1010,
                bank: (address >> 8) as u8,
            })
    }

    fn write_register(&mut self, address: u16, value: u8) -> Result<(), ChipError> {
        let address = usize::from(address);
        if address >= self.ram.len() {
            return Err(ChipError::InvalidRegisterBank {
                chip: Chip::X1010,
                bank: (address >> 8) as u8,
            });
        }
        if address < 128 && address % 8 == 0 && self.ram[address] & 1 == 0 && value & 1 != 0 {
            let channel = address / 8;
            self.sample_offsets[channel] = 0;
            self.envelope_offsets[channel] = 0;
        }
        self.ram[address] = value;
        Ok(())
    }

    fn validate_waveforms(&self) -> Result<(), ChipError> {
        for channel in 0..16 {
            let base = channel * 8;
            if self.ram[base] & 3 == 3 && (self.ram[base + 1] >= 32 || self.ram[base + 5] >= 64) {
                return Err(ChipError::InvalidConfiguration {
                    chip: Chip::X1010,
                    reason: "active waveform or envelope index exceeds RAM",
                });
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> [i32; 2] {
        let mut output = [0; 2];
        for channel in 0..16 {
            let base = channel * 8;
            let status = self.ram[base];
            if status & 1 == 0 {
                continue;
            }
            let divider = u32::from(status >> 7);
            let (sample, volume) = if status & 2 == 0 {
                let start = u32::from(self.ram[base + 4]) << 12;
                let end = (256 - u32::from(self.ram[base + 5])) << 12;
                let address = start + (self.sample_offsets[channel] >> 4);
                if address >= end {
                    self.ram[base] &= !1;
                    continue;
                }
                let sample = self.rom.get(address as usize).copied().unwrap_or(0) as i8;
                let frequency = u32::from(self.ram[base + 2]) >> divider;
                let step = if frequency == 0 { 4 } else { frequency };
                self.sample_offsets[channel] = self.sample_offsets[channel].wrapping_add(step);
                (sample, self.ram[base + 1])
            } else {
                let envelope_index = self.envelope_offsets[channel] >> 10;
                if status & 4 != 0 && envelope_index >= 128 {
                    self.ram[base] &= !1;
                    continue;
                }
                let envelope =
                    usize::from(self.ram[base + 5]) * 128 + (envelope_index as usize & 127);
                let wave = 0x1000
                    + usize::from(self.ram[base + 1]) * 128
                    + ((self.sample_offsets[channel] >> 10) as usize & 127);
                let frequency =
                    u32::from(u16::from_le_bytes([self.ram[base + 2], self.ram[base + 3]]))
                        >> divider;
                self.sample_offsets[channel] = self.sample_offsets[channel].wrapping_add(frequency);
                self.envelope_offsets[channel] =
                    self.envelope_offsets[channel].wrapping_add(u32::from(self.ram[base + 4]));
                (self.ram[wave] as i8, self.ram[envelope])
            };
            output[0] += i32::from(sample) * i32::from(volume >> 4) * VOLUME_BASE;
            output[1] += i32::from(sample) * i32::from(volume & 15) * VOLUME_BASE;
        }
        output.map(|value| value / 256)
    }
}

impl SoundChip for X1010 {
    fn chip_type(&self) -> &Chip {
        &Chip::X1010
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 512
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(512).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(u16::from_be_bytes([bank, register]), value)
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
        self.validate_waveforms()?;
        for frame in buffer.as_chunks_mut::<2>().0 {
            *frame = self.tick();
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.ram.fill(0);
        self.sample_offsets = [0; 16];
        self.envelope_offsets = [0; 16];
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::X1010)
    }
}

//! Based on MAME's C140 implementation, revision
//! `1bc5484698bb746c6f9f7dae58b005d2472f1215`.
//! Copyright (c) R. Belmont.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

/// C140 board ROM wiring or the related 219 ASIC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C140Type {
    System2,
    System21,
    Asic219,
}

#[derive(Clone, Copy, Default)]
struct Voice {
    active: bool,
    phase: u32,
    position: i32,
    previous: i32,
    last: i32,
    difference: i32,
    bank: u8,
    mode: u8,
    start: i32,
    end: i32,
    loop_start: i32,
}

/// Namco C140/219 sample player with native-rate stereo output.
///
/// Writes use 16-bit addresses. ROM writes load or update sample data;
/// unloaded addresses read as zero and reset preserves ROM. [`C140Type`]
/// selects the System 2, System 21, or ASIC 219 mapping. The native rate is
/// clock/576; the VGM sequencer also accepts legacy sample-rate headers.
pub struct C140 {
    clock: u32,
    kind: C140Type,
    registers: [u8; 0x200],
    voices: [Voice; 24],
    mulaw: [i16; 256],
    lfsr: u16,
    rom: Vec<u8>,
}

impl C140 {
    /// Create a chip from its master clock and board type.
    pub fn new(clock: u32, kind: C140Type) -> Result<Self, ChipError> {
        if clock < 576 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 576,
            });
        }
        let mulaw = match kind {
            C140Type::Asic219 => {
                let mut table = [0; 256];
                let mut level = 0_i32;
                for i in 0..128 {
                    table[i] = (level << 5) as i16;
                    table[i + 128] = !table[i] & !31;
                    level += if i < 16 {
                        1
                    } else if i < 24 {
                        2
                    } else if i < 48 {
                        4
                    } else if i < 100 {
                        8
                    } else {
                        16
                    };
                }
                table
            }
            _ => std::array::from_fn(|i| {
                let value = i as u8 as i8;
                let exponent = i32::from(value & 7);
                let mantissa = i32::from((value >> 3).abs() & 31);
                let mut result = ((0x80 << exponent) & 0xff00)
                    + (mantissa << if exponent != 0 { exponent + 3 } else { 4 });
                if value < 0 {
                    result = -result;
                }
                result as i16
            }),
        };
        Ok(Self {
            clock,
            kind,
            registers: [0; 0x200],
            voices: [Voice::default(); 24],
            mulaw,
            lfsr: 0x1234,
            rom: Vec::new(),
        })
    }

    /// Read a register from the mirrored register space.
    pub fn read_register(&self, address: u16) -> u8 {
        let mut address = usize::from(address) & 0x1ff;
        if self.kind == C140Type::Asic219 && address >= 0x1f8 && address & 1 != 0 {
            address -= 8;
        }
        self.registers[address]
    }

    fn write_address(&mut self, address: usize, value: u8) {
        let mut address = address & 0x1ff;
        let asic = self.kind == C140Type::Asic219;
        if asic && address >= 0x1f8 && address & 1 != 0 {
            address -= 8;
        }
        self.registers[address] = value;
        let voice_limit = if asic { 0x100 } else { 0x180 };
        if address < voice_limit && address & 15 == 5 {
            let index = address >> 4;
            if value & 0x80 == 0 {
                self.voices[index].active = false;
                return;
            }
            let base = index * 16;
            let read = |offset| {
                i32::from(u16::from_be_bytes([
                    self.registers[base + offset],
                    self.registers[base + offset + 1],
                ]))
            };
            let shift = u32::from(asic);
            self.voices[index] = Voice {
                active: true,
                bank: self.registers[base + 4],
                mode: value,
                start: read(6) << shift,
                end: read(8) << shift,
                loop_start: read(10) << shift,
                ..Voice::default()
            };
        }
    }

    fn sample(&self, address: u32) -> u8 {
        let offset = match self.kind {
            C140Type::System2 => ((address & 0x20_0000) >> 2) | (address & 0x07_ffff),
            C140Type::System21 => ((address & 0x30_0000) >> 1) | (address & 0x07_ffff),
            C140Type::Asic219 => address | 1,
        };
        self.rom.get(offset as usize).copied().unwrap_or(0)
    }

    fn tick(&mut self) -> [i32; 2] {
        const BANKS: [usize; 4] = [0x1f7, 0x1f1, 0x1f3, 0x1f5];
        let asic = self.kind == C140Type::Asic219;
        let mut output = [0; 2];
        for i in 0..if asic { 16 } else { 24 } {
            let mut voice = self.voices[i];
            let base = i * 16;
            let frequency = u32::from(u16::from_be_bytes([
                self.registers[base + 2],
                self.registers[base + 3],
            ]));
            if !voice.active || frequency == 0 {
                continue;
            }
            voice.phase += frequency * 2;
            let advance = voice.phase >> 16;
            voice.phase &= 0xffff;
            voice.position += advance as i32;
            let noise = asic && voice.mode & 4 != 0;
            if voice.position >= voice.end - voice.start {
                if voice.mode & 0x10 != 0 || noise {
                    voice.position = voice.loop_start - voice.start;
                } else {
                    self.voices[i].active = false;
                    continue;
                }
            }
            if advance != 0 {
                voice.previous = voice.last;
                if noise {
                    self.lfsr = (self.lfsr >> 1) ^ if self.lfsr & 1 != 0 { 0xfff6 } else { 0 };
                    voice.last = i32::from(self.lfsr);
                } else {
                    let group_bank = if asic {
                        u32::from(self.registers[BANKS[i / 4]] & 3) * 0x20000
                    } else {
                        0
                    };
                    let address = group_bank
                        + (u32::from(voice.bank) << 16)
                        + (voice.start + voice.position) as u32;
                    let byte = self.sample(address);
                    voice.last = if asic {
                        if voice.mode & 1 != 0 {
                            i32::from(self.mulaw[usize::from(byte)]) >> 5
                        } else {
                            i32::from(byte as i8) << 3
                        }
                    } else if voice.mode & 8 != 0 {
                        i32::from(self.mulaw[usize::from(byte)]) >> 4
                    } else {
                        i32::from(byte as i8) << 4
                    };
                }
                if asic && voice.mode & 0x40 != 0 {
                    voice.last = -voice.last;
                }
                voice.difference = voice.last - voice.previous;
            }
            // Split the 16-bit phase multiplication to avoid a 64-bit intermediate.
            let high = voice.difference * (voice.phase >> 8) as i32;
            let low = voice.difference * (voice.phase & 0xff) as i32;
            let sample = voice.previous + (high >> 8) + (((high & 255) * 256 + low) >> 16);
            let left = i32::from(self.registers[base + 1]) * 32 / 24;
            let right = i32::from(self.registers[base]) * 32 / 24;
            let shift = if noise {
                13
            } else if asic {
                8
            } else {
                9
            };
            let left_sample = sample * left;
            output[0] += (if asic && voice.mode & 8 != 0 {
                -left_sample
            } else {
                left_sample
            }) >> shift;
            output[1] += (sample * right) >> shift;
            self.voices[i] = voice;
        }
        [output[0] * 4, output[1] * 4]
    }
}

impl SoundChip for C140 {
    fn chip_type(&self) -> &Chip {
        &Chip::C140
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 576
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(576).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_address((usize::from(bank) << 8) | usize::from(register), value);
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
        self.registers = [0; 0x200];
        self.voices = [Voice::default(); 24];
        self.lfsr = 0x1234;
    }
}

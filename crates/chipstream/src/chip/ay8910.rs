//! AY-3-8910 sound generation based on libvgm's MAME-derived AY8910 core,
//! revision `c8b998b606895990c409a512b86c5509070f9f0d`.
//! Copyright (c) Couriersud and contributors; see `THIRD_PARTY_NOTICES.md`.

use std::num::NonZeroU32;

use soundlog::chip::Chip;

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};

/// AY-3-8910/8912/8913 PSG with three independent channel outputs.
///
/// The native sample rate is the input clock divided by eight. Output levels
/// follow the normalized, 1 kΩ load model in libvgm's AY8910 core. I/O port
/// writes are accepted but external I/O is not emulated.
#[derive(Clone)]
pub struct Ay8910 {
    clock: u32,
    registers: [u8; 16],
    tone_count: [u16; 3],
    tone_output: [bool; 3],
    noise_count: u8,
    noise_prescaler: bool,
    noise: u32,
    envelope_count: u32,
    envelope_step: i8,
    envelope_attack: u8,
    envelope_hold: bool,
    envelope_alternate: bool,
    envelope_holding: bool,
    volume_table: [i32; 16],
    envelope_table: [i32; 16],
}

impl Ay8910 {
    /// Create an AY-3-8910-compatible PSG.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 8 {
            return Err(ChipError::InvalidClock { clock, minimum: 8 });
        }
        let mut chip = Self {
            clock,
            registers: [0; 16],
            tone_count: [0; 3],
            tone_output: [false; 3],
            noise_count: 0,
            noise_prescaler: false,
            noise: 1,
            envelope_count: 0,
            envelope_step: 15,
            envelope_attack: 0,
            envelope_hold: true,
            envelope_alternate: false,
            envelope_holding: false,
            volume_table: Self::levels(true),
            envelope_table: Self::levels(false),
        };
        chip.reset();
        Ok(chip)
    }

    fn levels(zero_is_off: bool) -> [i32; 16] {
        const RESISTANCES: [f64; 16] = [
            15950.0, 15350.0, 15090.0, 14760.0, 14275.0, 13620.0, 12890.0, 11370.0, 10600.0,
            8590.0, 7190.0, 5985.0, 4820.0, 3945.0, 3017.0, 2345.0,
        ];
        let mut values = [0.0; 16];
        for (index, resistance) in RESISTANCES.iter().enumerate() {
            let pullup = if zero_is_off && index == 0 {
                0.0
            } else {
                1.0 / 800_000.0
            };
            let conductance = 1.0 / resistance;
            values[index] =
                (conductance + pullup) / (1.0 / 8_000_000.0 + 1.0 / 1000.0 + conductance + pullup);
        }
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        values.map(|value| (16384.0 * (value - min) / (max - min)) as i32)
    }

    fn write_register(&mut self, register: usize, value: u8) {
        self.registers[register] = value;
        if register == 13 {
            self.envelope_attack = if value & 4 != 0 { 15 } else { 0 };
            self.envelope_hold = value & 8 == 0 || value & 1 != 0;
            self.envelope_alternate = if value & 8 == 0 {
                self.envelope_attack != 0
            } else {
                value & 2 != 0
            };
            self.envelope_step = 15;
            self.envelope_holding = false;
        }
    }

    fn tick(&mut self) -> [i32; 3] {
        for channel in 0..3 {
            let period = u16::from(self.registers[channel * 2])
                | (u16::from(self.registers[channel * 2 + 1] & 15) << 8);
            self.tone_count[channel] += 1;
            if self.tone_count[channel] >= period {
                self.tone_count[channel] = 0;
                self.tone_output[channel] = !self.tone_output[channel];
            }
        }
        self.noise_count += 1;
        if self.noise_count >= self.registers[6] & 31 {
            self.noise_count = 0;
            self.noise_prescaler = !self.noise_prescaler;
            if self.noise_prescaler {
                let bit = (self.noise ^ (self.noise >> 3)) & 1;
                self.noise = (self.noise >> 1) | (bit << 16);
            }
        }
        if !self.envelope_holding {
            let period = u32::from(self.registers[11]) | (u32::from(self.registers[12]) << 8);
            self.envelope_count += 1;
            if self.envelope_count >= period * 2 {
                self.envelope_count = 0;
                self.envelope_step -= 1;
                if self.envelope_step < 0 {
                    if self.envelope_hold {
                        if self.envelope_alternate {
                            self.envelope_attack ^= 15;
                        }
                        self.envelope_holding = true;
                        self.envelope_step = 0;
                    } else {
                        if self.envelope_alternate {
                            self.envelope_attack ^= 15;
                        }
                        self.envelope_step = 15;
                    }
                }
            }
        }
        let mut output = [0; 3];
        for (channel, sample) in output.iter_mut().enumerate() {
            let enabled = (self.tone_output[channel] || self.registers[7] & (1 << channel) != 0)
                && (self.noise & 1 != 0 || self.registers[7] & (8 << channel) != 0);
            let volume = self.registers[8 + channel];
            *sample = if volume & 0x10 != 0 {
                let level = if enabled {
                    self.envelope_step as u8 ^ self.envelope_attack
                } else {
                    0
                };
                self.envelope_table[usize::from(level)]
            } else {
                self.volume_table[if enabled { usize::from(volume & 15) } else { 0 }]
            };
        }
        output
    }
}

impl SoundChip for Ay8910 {
    fn chip_type(&self) -> &Chip {
        &Chip::Ay8910
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate_ratio().floor_hz()
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(8).unwrap())
    }

    fn channels(&self) -> usize {
        3
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        if bank != 0 {
            return Err(ChipError::InvalidRegisterBank {
                chip: Chip::Ay8910,
                bank,
            });
        }
        if register > 15 {
            return Err(ChipError::InvalidRegister {
                chip: Chip::Ay8910,
                register,
            });
        }
        self.write_register(usize::from(register), value);
        Ok(())
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if !buffer.len().is_multiple_of(3) {
            return Err(ChipError::InvalidBufferLength {
                length: buffer.len(),
                channels: 3,
            });
        }
        for frame in buffer.as_chunks_mut::<3>().0 {
            *frame = self.tick();
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.registers = [0; 16];
        self.tone_count = [0; 3];
        self.tone_output = [false; 3];
        self.noise_count = 0;
        self.noise_prescaler = false;
        self.noise = 1;
        self.envelope_count = 0;
        self.write_register(13, 0);
    }

    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Ay8910)
    }
}

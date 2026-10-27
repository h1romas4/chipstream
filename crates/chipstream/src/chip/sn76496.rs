//! Based on MAME's SN76496 implementation, revision
//! `ee1e4f9683a4953cb9d88f9256017fcbc38e3144`.
//! Copyright (c) Nicola Salmoria.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

/// Configuration of the SN76496 family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sn76496Config {
    /// Length of the noise shift register (1 through 31 bits).
    pub shift_width: u8,
    /// Bit mask of the two white-noise feedback taps.
    pub feedback: u16,
    pub negate: bool,
    pub stereo: bool,
    pub divider_8: bool,
    pub frequency_zero_is_1024: bool,
    pub xnor: bool,
}

impl Default for Sn76496Config {
    fn default() -> Self {
        Self {
            shift_width: 16,
            feedback: 0x0009,
            negate: true,
            stereo: false,
            divider_8: true,
            frequency_zero_is_1024: false,
            xnor: false,
        }
    }
}

/// SN76489/SN76496-compatible PSG with optional Game Gear stereo.
///
/// Register 0 accepts PSG latch/data writes and register 1 Game Gear stereo
/// masks. [`Sn76496Config`] controls feedback, width, divider, polarity, and
/// stereo behavior. The native rate is clock/16 with the divider enabled,
/// or clock/2 without it.
pub struct Sn76496 {
    clock: u32,
    config: Sn76496Config,
    registers: [u16; 8],
    selected: usize,
    volumes: [i32; 4],
    periods: [i32; 4],
    counters: [i32; 4],
    outputs: [bool; 4],
    noise: u32,
    stereo_mask: u8,
}

impl Sn76496 {
    /// Create a Sega-compatible PSG.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        Self::with_config(clock, Sn76496Config::default())
    }

    /// Create a PSG using explicit noise, clock-divider and stereo settings.
    pub fn with_config(clock: u32, config: Sn76496Config) -> Result<Self, ChipError> {
        let divisor = if config.divider_8 { 16 } else { 2 };
        if clock < divisor {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: divisor,
            });
        }
        if !(1..=31).contains(&config.shift_width)
            || config.feedback.count_ones() != 2
            || u32::from(config.feedback) >= (1_u32 << config.shift_width)
        {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "invalid noise shift width or feedback",
            });
        }
        let mut chip = Self {
            clock,
            config,
            registers: [0; 8],
            selected: 0,
            volumes: [0; 4],
            periods: [0; 4],
            counters: [0; 4],
            outputs: [false; 4],
            noise: 0,
            stereo_mask: 0xff,
        };
        chip.reset();
        Ok(chip)
    }

    /// Write the Game Gear per-channel stereo routing mask.
    pub fn set_stereo_mask(&mut self, mask: u8) {
        self.stereo_mask = mask;
    }

    fn write_data(&mut self, value: u8) {
        const VOLUME: [i32; 16] = [
            8191, 6506, 5168, 4105, 3260, 2590, 2057, 1634, 1298, 1031, 819, 650, 516, 410, 326, 0,
        ];
        if value & 0x80 != 0 {
            self.selected = usize::from((value >> 4) & 7);
            if self.config.xnor && self.selected == 6 && (self.registers[6] as u8 ^ value) & 4 != 0
            {
                self.noise = 1 << (self.config.shift_width - 1);
            }
            self.registers[self.selected] =
                (self.registers[self.selected] & 0x3f0) | u16::from(value & 15);
        }
        let index = self.selected;
        let channel = index / 2;
        match index {
            0 | 2 | 4 => {
                if value & 0x80 == 0 {
                    self.registers[index] =
                        (self.registers[index] & 15) | (u16::from(value & 0x3f) << 4);
                }
                self.periods[channel] =
                    if self.registers[index] == 0 && self.config.frequency_zero_is_1024 {
                        1024
                    } else {
                        i32::from(self.registers[index])
                    };
                if index == 4 && self.registers[6] & 3 == 3 {
                    self.periods[3] = self.periods[2] * 2;
                }
            }
            1 | 3 | 5 | 7 => {
                self.registers[index] = u16::from(value & 15);
                self.volumes[channel] = VOLUME[usize::from(value & 15)];
            }
            6 => {
                self.registers[6] = u16::from(value & 15);
                self.periods[3] = if value & 3 == 3 {
                    self.periods[2] * 2
                } else {
                    1 << (5 + (value & 3))
                };
                if !self.config.xnor {
                    self.noise = 1 << (self.config.shift_width - 1);
                }
            }
            _ => unreachable!(),
        }
    }

    fn tick(&mut self) -> [i32; 2] {
        for channel in 0..3 {
            self.counters[channel] -= 1;
            if self.counters[channel] <= 0 {
                self.outputs[channel] = !self.outputs[channel];
                self.counters[channel] = self.periods[channel];
            }
        }
        self.counters[3] -= 1;
        if self.counters[3] <= 0 {
            let taps = u32::from(self.config.feedback);
            let first = 1_u32 << taps.trailing_zeros();
            let second = taps & !first;
            let active = self.registers[6] & 4 != 0;
            let rhs = if self.config.xnor { second } else { 0 };
            let bit = (self.noise & first != 0) != (active && (self.noise & second != rhs));
            self.noise >>= 1;
            if bit {
                self.noise |= 1 << (self.config.shift_width - 1);
            }
            self.outputs[3] = self.noise & 1 != 0;
            self.counters[3] = self.periods[3];
        }
        let mut output = [0; 2];
        for channel in 0..4 {
            if self.outputs[channel] {
                if !self.config.stereo || self.stereo_mask & (0x10 << channel) != 0 {
                    output[0] += self.volumes[channel];
                }
                if !self.config.stereo || self.stereo_mask & (1 << channel) != 0 {
                    output[1] += self.volumes[channel];
                }
            }
        }
        if self.config.negate {
            output[0] = -output[0];
            output[1] = -output[1];
        }
        output
    }
}

impl SoundChip for Sn76496 {
    fn chip_type(&self) -> &Chip {
        &Chip::Sn76489
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate_ratio().floor_hz()
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(
            self.clock,
            NonZeroU32::new(if self.config.divider_8 { 16 } else { 2 }).unwrap(),
        )
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        if bank != 0 {
            return Err(ChipError::InvalidRegisterBank {
                chip: Chip::Sn76489,
                bank,
            });
        }
        match register {
            0 => self.write_data(value),
            1 => self.set_stereo_mask(value),
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: Chip::Sn76489,
                    register,
                });
            }
        }
        Ok(())
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
        self.registers = [0; 8];
        self.selected = if self.config.frequency_zero_is_1024 {
            0
        } else {
            3
        };
        self.volumes = [0; 4];
        self.periods = [0; 4];
        self.counters = [0; 4];
        self.outputs = [false; 4];
        self.noise = 1 << (self.config.shift_width - 1);
        self.outputs[3] = self.noise & 1 != 0;
        self.stereo_mask = 0xff;
    }
}

//! Based on MAME's C6280 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Charles MacDonald.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.
//!
//! [`Huc6280`] emulates the six-channel Hudson PSG.

use super::{ChipError, ChipRegisterWrite, SoundChip};
use soundlog::chip::Chip;

const SCALE: [u8; 16] = [0, 3, 5, 7, 9, 11, 13, 15, 16, 19, 21, 23, 25, 27, 29, 31];

#[derive(Clone, Copy, Default)]
struct Channel {
    frequency: u16,
    control: u8,
    balance: u8,
    waveform: [u8; 32],
    index: u8,
    dda: u8,
    noise_control: u8,
    noise_counter: i32,
    noise_seed: u32,
    tick: i32,
}

/// Hudson HuC6280 stereo PSG with wavetable, DDA, noise, and LFO modes.
///
/// The native sample rate equals the input clock. Noise and LFO follow MAME's
/// implementation, whose behavior is not fully verified against hardware.
#[derive(Clone)]
pub struct Huc6280 {
    clock: u32,
    selected: u8,
    enabled: u8,
    balance: u8,
    lfo_frequency: u8,
    lfo_control: u8,
    channels: [Channel; 8],
    volume: [i32; 32],
}

impl Huc6280 {
    /// Create a HuC6280 PSG using the input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock == 0 {
            return Err(ChipError::InvalidClock { clock, minimum: 1 });
        }
        let mut volume = [0; 32];
        let mut level = 65535.0 / 6.0 / 32.0;
        let divisor = 10_f64.powf(1.5 / 20.0);
        for value in &mut volume[..31] {
            *value = level as i32;
            level /= divisor;
        }
        let mut chip = Self {
            clock,
            selected: 0,
            enabled: 0,
            balance: 0,
            lfo_frequency: 0,
            lfo_control: 0,
            channels: [Channel::default(); 8],
            volume,
        };
        chip.reset();
        Ok(chip)
    }

    fn write_register(&mut self, register: u8, value: u8) {
        let channel = &mut self.channels[usize::from(self.selected)];
        match register & 15 {
            0 => self.selected = value & 7,
            1 => self.balance = value,
            2 => channel.frequency = (channel.frequency & 0xf00) | u16::from(value),
            3 => channel.frequency = (channel.frequency & 0xff) | (u16::from(value & 15) << 8),
            4 => {
                if channel.control & 0x40 != 0 && value & 0x40 == 0 {
                    channel.index = 0;
                }
                if channel.control & 0x80 == 0 && value & 0x80 != 0 {
                    channel.tick = i32::from(channel.frequency);
                }
                channel.control = value;
                self.enabled = (self.enabled & !(1 << self.selected))
                    | (u8::from(value & 0x80 != 0) << self.selected);
            }
            5 => channel.balance = value,
            6 => {
                if channel.control & 0x40 != 0 {
                    channel.dda = value & 31;
                } else {
                    channel.waveform[usize::from(channel.index)] = value & 31;
                    if channel.control & 0x80 == 0 {
                        channel.index = (channel.index + 1) & 31;
                    }
                }
            }
            7 => channel.noise_control = value,
            8 => self.lfo_frequency = value,
            9 => self.lfo_control = value,
            _ => {}
        }
    }

    fn step_wave(channel: &mut Channel, period: i32) -> i32 {
        let data = i32::from(channel.waveform[usize::from(channel.index)]);
        channel.tick -= 1;
        if channel.tick <= 0 {
            channel.tick = period;
            channel.index = (channel.index + 1) & 31;
        }
        data
    }

    fn tick(&mut self) -> [i32; 2] {
        let mut output = [0; 2];
        for index in 0..6 {
            if self.enabled & (1 << index) == 0 {
                continue;
            }
            let channel = &self.channels[index];
            let channel_level = channel.control & 31;
            let levels = [
                (31 - SCALE[usize::from(self.balance >> 4)])
                    + (31 - channel_level)
                    + (31 - SCALE[usize::from(channel.balance >> 4)]),
                (31 - SCALE[usize::from(self.balance & 15)])
                    + (31 - channel_level)
                    + (31 - SCALE[usize::from(channel.balance & 15)]),
            ];
            let data = if index >= 4 && channel.noise_control & 0x80 != 0 {
                let channel = &mut self.channels[index];
                let data = if channel.noise_seed & 1 != 0 { 31 } else { 0 };
                channel.noise_counter -= 1;
                if channel.noise_counter <= 0 {
                    channel.noise_counter = i32::from((channel.noise_control & 31) ^ 31) << 6;
                    let seed = channel.noise_seed;
                    let bit = (seed ^ (seed >> 1) ^ (seed >> 11) ^ (seed >> 12) ^ (seed >> 17)) & 1;
                    channel.noise_seed = (seed >> 1) | (bit << 17);
                }
                data
            } else if channel.control & 0x40 != 0 {
                i32::from(channel.dda)
            } else if self.lfo_control & 3 != 0 && index < 2 {
                if index == 0 {
                    let (destination, source) = self.channels.split_at_mut(1);
                    let destination = &mut destination[0];
                    let source = &mut source[0];
                    let source_period = if source.frequency == 0 {
                        4096
                    } else {
                        i32::from(source.frequency)
                    };
                    let mut period = if destination.frequency == 0 {
                        4096
                    } else {
                        i32::from(destination.frequency)
                    };
                    if self.lfo_control & 0x80 != 0 {
                        source.tick = source_period * i32::from(self.lfo_frequency);
                        source.index = 0;
                    } else {
                        let data =
                            Self::step_wave(source, source_period * i32::from(self.lfo_frequency));
                        period += (data - 16) * (1 << (((self.lfo_control & 3) - 1) * 2));
                    }
                    Self::step_wave(destination, period)
                } else {
                    i32::from(channel.waveform[usize::from(channel.index)])
                }
            } else {
                let channel = &mut self.channels[index];
                let period = if channel.frequency == 0 {
                    4096
                } else {
                    i32::from(channel.frequency)
                };
                Self::step_wave(channel, period)
            };
            for side in 0..2 {
                output[side] += self.volume[usize::from(levels[side].min(31))] * (data - 16);
            }
        }
        output
    }
}

impl SoundChip for Huc6280 {
    fn chip_type(&self) -> &Chip {
        &Chip::Huc6280
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.clock
    }

    fn channels(&self) -> usize {
        2
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (_, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(register, value);
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
        for (index, channel) in self.channels[..6].iter_mut().enumerate() {
            channel.index = 0;
            if index >= 4 {
                channel.noise_seed = 1;
            }
        }
    }

    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Huc6280)
    }
}

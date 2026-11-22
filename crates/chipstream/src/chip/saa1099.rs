//! Based on MAME's SAA1099 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Juergen Buchmueller and Manuel Abadia.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.
//!
//! [`Saa1099`] emulates six stereo tone and noise channels.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

#[derive(Clone, Copy, Default)]
struct Channel {
    frequency: u8,
    octave: u8,
    tone_enabled: bool,
    noise_enabled: bool,
    amplitude: [i32; 2],
    envelope: [i32; 2],
    counter: i32,
    level: bool,
}

impl Channel {
    fn period(&self) -> i32 {
        (511 - i32::from(self.frequency)) << (8 - self.octave)
    }
}

#[derive(Clone, Copy)]
struct Noise {
    counter: i32,
    level: u32,
}

impl Default for Noise {
    fn default() -> Self {
        Self {
            counter: 0,
            level: u32::MAX,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Envelope {
    enabled: bool,
    reverse_right: bool,
    mode: u8,
    three_bit: bool,
    external_clock: bool,
    step: u8,
}

/// Philips SAA1099 with six channels and stereo native output.
///
/// The native sample rate is clock/256. Register writes also model the
/// address-select operation, including its effect on externally clocked
/// envelopes.
#[derive(Clone)]
pub struct Saa1099 {
    clock: u32,
    channels: [Channel; 6],
    noise: [Noise; 2],
    noise_params: [u8; 2],
    envelopes: [Envelope; 2],
    enabled: bool,
}

impl Saa1099 {
    /// Create an SAA1099 using the input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 256 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 256,
            });
        }
        Ok(Self {
            clock,
            channels: [Channel::default(); 6],
            noise: [Noise::default(); 2],
            noise_params: [0; 2],
            envelopes: [Envelope::default(); 2],
            enabled: false,
        })
    }

    fn envelope_level(mode: u8, step: u8) -> u8 {
        match mode {
            0 => 0,
            1 => 15,
            2 => 15_u8.saturating_sub(step),
            3 => 15 - step % 16,
            4 => match step {
                0..=15 => step,
                16..=31 => 31 - step,
                _ => 0,
            },
            5 => {
                let phase = step % 32;
                if phase < 16 { phase } else { 31 - phase }
            }
            6 => {
                if step < 16 {
                    step
                } else {
                    0
                }
            }
            7 => step % 16,
            _ => unreachable!(),
        }
    }

    fn clock_envelope(&mut self, index: usize) {
        let env = &mut self.envelopes[index];
        let values = if env.enabled {
            env.step = ((env.step + 1) & 0x3f) | (env.step & 0x20);
            let level = Self::envelope_level(env.mode, env.step);
            let mask = if env.three_bit { 14 } else { 15 };
            [
                i32::from(level & mask),
                i32::from(if env.reverse_right { 15 - level } else { level } & mask),
            ]
        } else {
            [16; 2]
        };
        for channel in &mut self.channels[index * 3..index * 3 + 3] {
            channel.envelope = values;
        }
    }

    fn write_register(&mut self, register: u8, value: u8) {
        let register = register & 0x1f;
        if register == 0x18 || register == 0x19 {
            for index in 0..2 {
                if self.envelopes[index].external_clock {
                    self.clock_envelope(index);
                }
            }
        }
        match register {
            0x00..=0x05 => {
                self.channels[usize::from(register)].amplitude = [
                    i32::from(value & 15) * 32767 / 16,
                    i32::from(value >> 4) * 32767 / 16,
                ];
            }
            0x08..=0x0d => self.channels[usize::from(register - 8)].frequency = value,
            0x10..=0x12 => {
                let index = usize::from(register - 0x10) * 2;
                self.channels[index].octave = value & 7;
                self.channels[index + 1].octave = (value >> 4) & 7;
            }
            0x14 | 0x15 => {
                for (index, channel) in self.channels.iter_mut().enumerate() {
                    let enabled = value & (1 << index) != 0;
                    if register == 0x14 {
                        channel.tone_enabled = enabled;
                    } else {
                        channel.noise_enabled = enabled;
                    }
                }
            }
            0x16 => self.noise_params = [value & 3, (value >> 4) & 3],
            0x18 | 0x19 => {
                self.envelopes[usize::from(register - 0x18)] = Envelope {
                    enabled: value & 0x80 != 0,
                    reverse_right: value & 1 != 0,
                    mode: (value >> 1) & 7,
                    three_bit: value & 0x10 != 0,
                    external_clock: value & 0x20 != 0,
                    step: 0,
                };
            }
            0x1c => {
                self.enabled = value & 1 != 0;
                if value & 2 != 0 {
                    for channel in &mut self.channels {
                        channel.level = false;
                        channel.counter = channel.period();
                    }
                }
            }
            _ => {}
        }
    }

    fn tick(&mut self) -> [i32; 2] {
        if !self.enabled {
            return [0; 2];
        }
        let noise_periods = std::array::from_fn::<_, 2, _>(|index| {
            let param = self.noise_params[index];
            if param == 3 {
                self.channels[index * 3].period()
            } else {
                256 << param
            }
        });
        let mut output = [0; 2];
        for index in 0..6 {
            while self.channels[index].counter <= 0 {
                let channel = &mut self.channels[index];
                channel.counter += channel.period();
                channel.level = !channel.level;
                if index == 1 && !self.envelopes[0].external_clock {
                    self.clock_envelope(0);
                } else if index == 4 && !self.envelopes[1].external_clock {
                    self.clock_envelope(1);
                }
            }
            let channel = &mut self.channels[index];
            channel.counter -= 256;
            let tone = u8::from(channel.level);
            let noise = (self.noise[index / 3].level & 1) as u8;
            let level = match (channel.tone_enabled, channel.noise_enabled) {
                (true, true) => tone * if noise != 0 { 2 } else { 1 },
                (true, false) => tone,
                (false, true) => noise,
                (false, false) => 0,
            };
            if level != 0 {
                for (side, sample) in output.iter_mut().enumerate() {
                    *sample +=
                        channel.amplitude[side] * channel.envelope[side] / 16 / i32::from(level);
                }
            }
        }
        for (index, noise) in self.noise.iter_mut().enumerate() {
            while noise.counter <= 0 {
                noise.counter += noise_periods[index];
                let bit = u32::from((noise.level & 0x20000 == 0) != (noise.level & 0x400 == 0));
                noise.level = noise.level.wrapping_shl(1) | bit;
            }
            noise.counter -= 256;
        }
        [output[0] / 6, output[1] / 6]
    }
}

impl SoundChip for Saa1099 {
    fn chip_type(&self) -> &Chip {
        &Chip::Saa1099
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.clock / 256
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(256).unwrap())
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
        *self = Self::new(self.clock).expect("existing clock is valid");
    }

    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, self.chip_type().clone())
    }
}

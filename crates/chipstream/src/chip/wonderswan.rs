//! WonderSwan sound based on Tetorica's MAME adaptation, revision
//! `b4f2543c08f3a6b2c780d723263dee444ff13343`.
//! Copyright (c) Wilbert Pol.
//! Copyright (c) 2026, kyorohiro.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SoundChip};
use soundlog::chip::Chip;

const DIVIDER: u32 = 128;
const NOISE_TAPS: [u8; 8] = [14, 10, 13, 4, 8, 6, 9, 11];

#[derive(Clone, Copy)]
struct Channel {
    frequency: u16,
    period: u32,
    position: u32,
    left: u8,
    right: u8,
    on: bool,
    offset: u8,
    signal: u8,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            frequency: 0,
            period: 2048,
            position: 0,
            left: 0,
            right: 0,
            on: false,
            offset: 0,
            signal: 0,
        }
    }
}

/// WonderSwan wavetable, direct voice, sweep and noise audio.
///
/// The native rate is `clock / 128`, rounded down, as in MAME. Output is the
/// unipolar headphone or mono speaker DAC without an analog high-pass filter.
/// Headphone output takes precedence when enabled; otherwise the enabled
/// speaker is used. HyperVoice, sound DMA and system-volume gain are not
/// emulated.
/// Register writes use VGM offsets from `0x80`; memory writes address 16 KiB
/// of waveform RAM. VGM writes to `0x4000..=0x7fff` are outside the sound
/// device's addressable RAM and have no effect.
#[derive(Clone)]
pub struct WonderSwan {
    clock: u32,
    ram: Vec<u8>,
    channels: [Channel; 4],
    sweep_step: i8,
    sweep_time: u32,
    sweep_count: u32,
    noise_type: u8,
    noise_enable: bool,
    noise_output: u8,
    noise_shift: u16,
    sample_address: u16,
    voice: bool,
    sweep: bool,
    noise: bool,
    output_control: u8,
    sample_volume: u8,
}

impl WonderSwan {
    /// Create a WonderSwan sound device.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < DIVIDER {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: DIVIDER,
            });
        }
        Ok(Self {
            clock,
            ram: vec![0; 0x4000],
            channels: [Channel::default(); 4],
            sweep_step: 0,
            sweep_time: 8192,
            sweep_count: 0,
            noise_type: 0,
            noise_enable: false,
            noise_output: 0,
            noise_shift: 0,
            sample_address: 0,
            voice: false,
            sweep: false,
            noise: false,
            output_control: 0,
            sample_volume: 0,
        })
    }

    fn write_register(&mut self, register: u8, value: u8) {
        match register {
            0x00..=0x07 => {
                let channel = &mut self.channels[usize::from(register >> 1)];
                channel.frequency = if register & 1 == 0 {
                    (channel.frequency & 0x700) | u16::from(value)
                } else {
                    (channel.frequency & 0xff) | (u16::from(value & 7) << 8)
                };
                channel.period = 2048 - u32::from(channel.frequency);
            }
            0x08..=0x0b => {
                let channel = &mut self.channels[usize::from(register - 8)];
                channel.left = value >> 4;
                channel.right = value & 15;
            }
            0x0c => self.sweep_step = value as i8,
            0x0d => self.sweep_time = 8192 * (u32::from(value & 31) + 1),
            0x0e => {
                self.noise_type = value & 7;
                self.noise_enable = value & 16 != 0;
                if value & 8 != 0 {
                    self.noise_shift = 0;
                    self.noise_output = 0;
                }
            }
            0x0f => self.sample_address = u16::from(value) << 6,
            0x10 => {
                for (i, channel) in self.channels.iter_mut().enumerate() {
                    channel.on = value & (1 << i) != 0;
                }
                self.voice = value & 0x20 != 0;
                self.sweep = value & 0x40 != 0;
                self.noise = value & 0x80 != 0;
            }
            0x11 => self.output_control = value & 15,
            0x12 => self.noise_shift = (self.noise_shift & 0x7f00) | u16::from(value),
            0x13 => self.noise_shift = (self.noise_shift & 0xff) | (u16::from(value & 0x7f) << 8),
            0x14 => self.sample_volume = value & 15,
            _ => {}
        }
    }

    fn tick(&mut self) -> [i32; 2] {
        if self.output_control & 9 == 0 {
            return [0; 2];
        }
        let mut output = [0; 2];
        for index in 0..4 {
            let channel = &mut self.channels[index];
            if !channel.on {
                continue;
            }
            if index == 1 && self.voice {
                let data = (channel.left << 4) | channel.right;
                if self.sample_volume & 12 != 0 {
                    output[0] += i32::from(data >> u8::from(self.sample_volume & 8 == 0));
                }
                if self.sample_volume & 3 != 0 {
                    output[1] += i32::from(data >> u8::from(self.sample_volume & 2 == 0));
                }
            } else {
                let sample = channel.signal;
                channel.position += DIVIDER;
                while channel.position >= channel.period {
                    if index == 3 && self.noise {
                        channel.signal = if self.noise_output != 0 { 15 } else { 0 };
                    } else {
                        let address = (usize::from(self.sample_address)
                            + index * 16
                            + usize::from((channel.offset >> 1) & 15))
                            & 0x3fff;
                        channel.signal = (self.ram[address] >> ((channel.offset & 1) * 4)) & 15;
                        channel.offset = channel.offset.wrapping_add(1);
                    }
                    channel.position -= channel.period;
                    if index == 3 && self.noise_enable {
                        self.noise_output = (1
                            ^ (self.noise_shift >> 7)
                            ^ (self.noise_shift >> NOISE_TAPS[usize::from(self.noise_type)]))
                            as u8
                            & 1;
                        self.noise_shift = (self.noise_shift << 1) | u16::from(self.noise_output);
                    }
                }
                if index == 2 && self.sweep {
                    self.sweep_count += DIVIDER;
                    while self.sweep_count >= self.sweep_time {
                        self.sweep_count -= self.sweep_time;
                        channel.frequency =
                            (i32::from(channel.frequency) + i32::from(self.sweep_step)) as u16
                                & 0x7ff;
                        channel.period = 2048 - u32::from(channel.frequency);
                    }
                }
                output[0] += i32::from(channel.left) * i32::from(sample);
                output[1] += i32::from(channel.right) * i32::from(sample);
            }
        }
        if self.output_control & 8 != 0 {
            [output[0] * 32, output[1] * 32]
        } else {
            let shift = (self.output_control >> 1) & 3;
            let mono = (((output[0] & 0x3ff) + (output[1] & 0x3ff)) >> shift) & 0xff;
            [mono * 128; 2]
        }
    }
}

impl SoundChip for WonderSwan {
    fn chip_type(&self) -> &Chip {
        &Chip::WonderSwan
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / DIVIDER
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        match write {
            ChipRegisterWrite::WonderSwan(spec) => {
                if (0x4000..=0x7fff).contains(&spec.offset) {
                    Ok(())
                } else {
                    self.write_ram(usize::from(spec.offset), &[spec.value])
                }
            }
            ChipRegisterWrite::WonderSwanReg(spec) => {
                self.write_register(spec.register, spec.value);
                Ok(())
            }
            other => {
                other.parts_for(self.chip_type())?;
                unreachable!()
            }
        }
    }
    fn write_ram(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= self.ram.len())
            .ok_or(ChipError::RamOutOfRange {
                chip: Chip::WonderSwan,
                start,
                length: data.len(),
            })?;
        self.ram[start..end].copy_from_slice(data);
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
        super::restore_cloned_state(self, state, Chip::WonderSwan)
    }
}

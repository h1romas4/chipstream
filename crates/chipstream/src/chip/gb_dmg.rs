//! Game Boy DMG APU based on Tetorica's MAME adaptation, revision
//! `b4f2543c08f3a6b2c780d723263dee444ff13343`.
//! Copyright (c) Wilbert Pol and Anthony Kruize.
//! Copyright (c) 2026, kyorohiro.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SoundChip};
use soundlog::chip::Chip;

const RATE: u32 = 44_100;
const DUTY: [u8; 4] = [0x80, 0x81, 0xe1, 0x7e];
const NOISE_DIVISOR: [u32; 8] = [8, 16, 32, 48, 64, 80, 96, 112];
const INITIAL_WAVE: [u8; 16] = [
    0xac, 0xdd, 0xda, 0x48, 0x36, 0x02, 0xcf, 0x16, 0x2c, 0x04, 0xe5, 0x2c, 0xac, 0xdd, 0xda, 0x48,
];

#[derive(Clone, Copy, Default)]
struct Channel {
    reg: [u8; 5],
    on: bool,
    length: u8,
    length_counting: bool,
    length_enabled: bool,
    frequency: u16,
    frequency_counter: u16,
    sub_cycle: u32,
    duty: u8,
    envelope_enabled: bool,
    envelope_value: i8,
    envelope_direction: i8,
    envelope_time: u8,
    envelope_count: u8,
    signal: i8,
    frequency_shadow: u16,
    sweep_enabled: bool,
    sweep_neg_mode_used: bool,
    sweep_shift: u8,
    sweep_direction: i32,
    sweep_time: u8,
    sweep_count: u8,
    level: u8,
    wave_offset: u8,
    duty_count: u8,
    current_sample: u8,
    sample_reading: bool,
    noise_short: bool,
    noise_lfsr: u16,
}

impl Channel {
    fn dac_enabled(&self, wave: bool) -> bool {
        if wave {
            self.reg[0] & 0x80 != 0
        } else {
            self.reg[2] & 0xf8 != 0
        }
    }

    fn tick_length(&mut self, mask: u8) {
        if self.length_enabled {
            self.length = self.length.wrapping_add(1) & mask;
            if self.length == 0 {
                self.on = false;
                self.length_counting = false;
            }
        }
    }

    fn calculate_sweep(&mut self) -> i32 {
        self.sweep_neg_mode_used = self.sweep_direction < 0;
        let next = i32::from(self.frequency_shadow)
            + self.sweep_direction * i32::from(self.frequency_shadow >> self.sweep_shift);
        if next > 0x7ff {
            self.on = false;
        }
        next
    }

    fn tick_sweep(&mut self) {
        self.sweep_count = self.sweep_count.wrapping_sub(1) & 7;
        if self.sweep_count == 0 {
            self.sweep_count = self.sweep_time;
            if self.sweep_enabled && self.sweep_time > 0 {
                let next = self.calculate_sweep();
                if self.on && self.sweep_shift > 0 {
                    self.frequency = next as u16;
                    self.frequency_shadow = self.frequency;
                    self.reg[3] = self.frequency as u8;
                    self.reg[4] = (self.reg[4] & !7) | ((self.frequency >> 8) as u8 & 7);
                }
                self.calculate_sweep();
            }
        }
    }

    fn tick_envelope(&mut self) {
        if self.envelope_enabled {
            self.envelope_count = self.envelope_count.wrapping_sub(1) & 7;
            if self.envelope_count == 0 {
                self.envelope_count = self.envelope_time;
                if self.envelope_count != 0 {
                    let next = self.envelope_value + self.envelope_direction;
                    if (0..=15).contains(&next) {
                        self.envelope_value = next;
                    } else {
                        self.envelope_enabled = false;
                    }
                }
            }
        }
    }

    fn tick_square(&mut self) {
        if self.on {
            self.sub_cycle += 1;
            if self.sub_cycle >= 4 {
                self.sub_cycle = 0;
                self.frequency_counter += 1;
                if self.frequency_counter > 0x7ff {
                    self.frequency_counter = self.frequency;
                    self.duty_count = (self.duty_count + 1) & 7;
                    self.signal = ((DUTY[usize::from(self.duty)] >> self.duty_count) & 1) as i8;
                }
            }
        }
    }

    fn tick_wave(&mut self, ram: &[u8; 16]) {
        if self.on {
            self.sub_cycle += 1;
            if self.sub_cycle >= 2 {
                self.sub_cycle = 0;
                self.frequency_counter = (self.frequency_counter + 1) & 0x7ff;
                self.sample_reading = false;
                if self.frequency_counter == 0x7ff {
                    self.wave_offset = (self.wave_offset + 1) & 31;
                }
                if self.frequency_counter == 0 {
                    self.sample_reading = true;
                    self.current_sample = ram[usize::from(self.wave_offset / 2)];
                    if self.wave_offset & 1 == 0 {
                        self.current_sample >>= 4;
                    }
                    self.current_sample &= 15;
                    self.frequency_counter = self.frequency;
                }
            }
            self.signal = if self.level != 0 {
                (self.current_sample >> (self.level - 1)) as i8
            } else {
                0
            };
        }
    }

    fn tick_noise(&mut self) {
        let period = NOISE_DIVISOR[usize::from(self.reg[3] & 7)] << (self.reg[3] >> 4);
        self.sub_cycle += 1;
        if self.sub_cycle >= period {
            self.sub_cycle = 0;
            let feedback = ((self.noise_lfsr >> 1) ^ self.noise_lfsr) & 1;
            self.noise_lfsr = (self.noise_lfsr >> 1) | (feedback << 14);
            if self.noise_short {
                self.noise_lfsr = (self.noise_lfsr & !(1 << 6)) | (feedback << 6);
            }
            self.signal = ((!self.noise_lfsr) & 1) as i8;
        }
    }
}

/// Game Boy DMG APU with two pulse channels, wave RAM, and noise.
///
/// Output is integrated at 44,100 Hz. Register addresses use VGM offsets
/// relative to `0xff10`. DMG wave RAM access restrictions are emulated;
/// the hardware wave RAM corruption glitch and analog high-pass filter are not.
#[derive(Clone)]
pub struct GbDmg {
    clock: u32,
    channels: [Channel; 4],
    wave_ram: [u8; 16],
    power_on: bool,
    vol_left: u8,
    vol_right: u8,
    route: u8,
    frame_cycle: u32,
    frame_step: u8,
    tick_remaining: u32,
    last: [i32; 2],
}

impl GbDmg {
    /// Create a DMG APU with the specified input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock == 0 {
            return Err(ChipError::InvalidClock { clock, minimum: 1 });
        }
        let mut chip = Self {
            clock,
            channels: [Channel::default(); 4],
            wave_ram: INITIAL_WAVE,
            power_on: false,
            vol_left: 0,
            vol_right: 0,
            route: 0,
            frame_cycle: 0,
            frame_step: 0,
            tick_remaining: 0,
            last: [0; 2],
        };
        chip.reset();
        Ok(chip)
    }

    fn tick(&mut self) {
        if self.power_on {
            self.frame_cycle += 1;
            if self.frame_cycle >= 8192 {
                self.frame_cycle = 0;
                self.frame_step = (self.frame_step + 1) & 7;
                if self.frame_step == 2 || self.frame_step == 6 {
                    self.channels[0].tick_sweep();
                }
                if self.frame_step & 1 == 0 {
                    for (index, channel) in self.channels.iter_mut().enumerate() {
                        channel.tick_length(if index == 2 { 0xff } else { 0x3f });
                    }
                } else if self.frame_step == 7 {
                    for index in [0, 1, 3] {
                        self.channels[index].tick_envelope();
                    }
                }
            }
            self.channels[0].tick_square();
            self.channels[1].tick_square();
            self.channels[2].tick_wave(&self.wave_ram);
            self.channels[3].tick_noise();
        }
        self.last = [0; 2];
        for (index, channel) in self.channels.iter().enumerate() {
            if channel.on {
                let raw = i32::from(channel.signal)
                    * if index == 2 {
                        1
                    } else {
                        i32::from(channel.envelope_value)
                    };
                let sample = 15 - raw * 2;
                if self.route & (1 << (index + 4)) != 0 {
                    self.last[0] += sample;
                }
                if self.route & (1 << index) != 0 {
                    self.last[1] += sample;
                }
            }
        }
    }

    fn write_register(&mut self, register: u8, value: u8) -> Result<(), ChipError> {
        if (0x20..=0x2f).contains(&register) {
            let wave = &self.channels[2];
            if wave.on {
                if wave.sample_reading {
                    self.wave_ram[usize::from(wave.wave_offset / 2)] = value;
                }
            } else {
                self.wave_ram[usize::from(register - 0x20)] = value;
            }
            return Ok(());
        }
        if register > 0x2f {
            return Err(ChipError::InvalidRegister {
                chip: Chip::GbDmg,
                register,
            });
        }
        if !self.power_on && !matches!(register, 0x01 | 0x06 | 0x0b | 0x10 | 0x16) {
            return Ok(());
        }
        self.write_internal(register, value);
        Ok(())
    }

    fn write_internal(&mut self, register: u8, value: u8) {
        let length_clock = self.frame_step & 1 == 0;
        match register {
            0x00 => {
                let channel = &mut self.channels[0];
                let old = channel.reg[0];
                channel.reg[0] = value;
                channel.sweep_shift = value & 7;
                channel.sweep_direction = if value & 8 != 0 { -1 } else { 1 };
                channel.sweep_time = (value >> 4) & 7;
                if old & 8 != 0 && value & 8 == 0 && channel.sweep_neg_mode_used {
                    channel.on = false;
                }
            }
            0x01 | 0x06 | 0x0b | 0x10 => {
                let index = match register {
                    0x01 => 0,
                    0x06 => 1,
                    0x0b => 2,
                    _ => 3,
                };
                let channel = &mut self.channels[index];
                channel.reg[1] = value;
                if index < 2 && self.power_on {
                    channel.duty = value >> 6;
                }
                channel.length = if index == 2 { value } else { value & 63 };
                channel.length_counting = true;
            }
            0x02 | 0x07 | 0x11 => {
                let index = match register {
                    0x02 => 0,
                    0x07 => 1,
                    _ => 3,
                };
                let channel = &mut self.channels[index];
                channel.reg[2] = value;
                channel.envelope_value = (value >> 4) as i8;
                channel.envelope_direction = if value & 8 != 0 { 1 } else { -1 };
                channel.envelope_time = value & 7;
                if !channel.dac_enabled(false) {
                    channel.on = false;
                }
            }
            0x0a => {
                let channel = &mut self.channels[2];
                channel.reg[0] = value;
                if !channel.dac_enabled(true) {
                    channel.on = false;
                }
            }
            0x0c => {
                self.channels[2].reg[2] = value;
                self.channels[2].level = (value >> 5) & 3;
            }
            0x03 | 0x08 | 0x0d => {
                let index = match register {
                    0x03 => 0,
                    0x08 => 1,
                    _ => 2,
                };
                let channel = &mut self.channels[index];
                channel.reg[3] = value;
                if index != 0 || !channel.sweep_enabled {
                    channel.frequency = (u16::from(channel.reg[4] & 7) << 8) | u16::from(value);
                }
            }
            0x12 => {
                self.channels[3].reg[3] = value;
                self.channels[3].noise_short = value & 8 != 0;
            }
            0x04 | 0x09 | 0x0e | 0x13 => {
                let index = match register {
                    0x04 => 0,
                    0x09 => 1,
                    0x0e => 2,
                    _ => 3,
                };
                let channel = &mut self.channels[index];
                channel.reg[4] = value;
                let previous = channel.length_enabled;
                channel.length_enabled = value & 0x40 != 0;
                if index == 0 {
                    channel.frequency = (u16::from(value & 7) << 8) | u16::from(channel.reg[3]);
                }
                let mask = if index == 2 { 0xff } else { 0x3f };
                if !previous && length_clock && channel.length_counting && channel.length_enabled {
                    channel.tick_length(mask);
                }
                if value & 0x80 != 0 {
                    channel.on = true;
                    channel.length_counting = true;
                    channel.sub_cycle = 0;
                    if index != 2 {
                        channel.envelope_enabled = true;
                        channel.envelope_value = (channel.reg[2] >> 4) as i8;
                        channel.envelope_count = channel.envelope_time;
                        channel.signal = 0;
                    }
                    if index < 3 {
                        channel.frequency = (u16::from(value & 7) << 8) | u16::from(channel.reg[3]);
                        channel.frequency_counter = channel.frequency;
                        channel.duty_count = 0;
                    }
                    if index == 0 {
                        channel.sweep_count = channel.sweep_time;
                        channel.sweep_neg_mode_used = false;
                        channel.frequency_shadow = channel.frequency;
                        channel.sweep_enabled = channel.sweep_shift != 0 || channel.sweep_time != 0;
                        if channel.sweep_shift > 0 {
                            channel.calculate_sweep();
                        }
                    } else if index == 2 {
                        channel.wave_offset = 0;
                        channel.duty = 1;
                        channel.current_sample = 0;
                        channel.sample_reading = false;
                    } else if index == 3 {
                        channel.noise_lfsr = 0x7fff;
                    }
                    if !channel.dac_enabled(index == 2) {
                        channel.on = false;
                    }
                    if channel.length == 0 && channel.length_enabled && length_clock {
                        channel.tick_length(mask);
                    }
                } else if index == 1 || index == 2 || (index == 0 && !channel.sweep_enabled) {
                    channel.frequency = (u16::from(value & 7) << 8) | u16::from(channel.reg[3]);
                }
            }
            0x14 => {
                self.vol_left = value & 7;
                self.vol_right = (value >> 4) & 7;
            }
            0x15 => self.route = value,
            0x16 => {
                if value & 0x80 == 0 {
                    self.power_off();
                }
                self.power_on = value & 0x80 != 0;
            }
            _ => {}
        }
    }

    fn power_off(&mut self) {
        for register in [
            0x00, 0x02, 0x03, 0x04, 0x07, 0x08, 0x09, 0x0a, 0x0c, 0x0d, 0x0e, 0x11, 0x12, 0x13,
        ] {
            self.write_internal(register, 0);
        }
        self.channels[0].duty = 0;
        self.channels[0].frequency_shadow = 0;
        self.channels[0].sweep_neg_mode_used = false;
        self.channels[2].current_sample = 0;
        for channel in &mut self.channels {
            channel.on = false;
            channel.length_counting = false;
        }
        for index in [0, 1, 3] {
            self.channels[index].reg[1] = 0;
        }
        self.write_internal(0x14, 0);
        self.write_internal(0x15, 0);
    }
}

impl SoundChip for GbDmg {
    fn chip_type(&self) -> &Chip {
        &Chip::GbDmg
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        RATE
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (_, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(register, value)
    }
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if !buffer.len().is_multiple_of(2) {
            return Err(ChipError::InvalidBufferLength {
                length: buffer.len(),
                channels: 2,
            });
        }
        for frame in buffer.as_chunks_mut::<2>().0 {
            let mut remaining = self.clock;
            let mut sum = [0_i64; 2];
            while remaining != 0 {
                if self.tick_remaining == 0 {
                    self.tick();
                    self.tick_remaining = RATE;
                }
                let duration = remaining.min(self.tick_remaining);
                for (sum, sample) in sum.iter_mut().zip(self.last) {
                    *sum += i64::from(sample) * i64::from(duration);
                }
                remaining -= duration;
                self.tick_remaining -= duration;
            }
            frame[0] = (sum[0] * i64::from(self.vol_left + 1) * 32768
                / (480 * i64::from(self.clock))) as i32;
            frame[1] = (sum[1] * i64::from(self.vol_right + 1) * 32768
                / (480 * i64::from(self.clock))) as i32;
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.channels = [Channel::default(); 4];
        self.wave_ram = INITIAL_WAVE;
        self.power_on = false;
        self.vol_left = 0;
        self.vol_right = 0;
        self.route = 0;
        self.frame_cycle = 0;
        self.frame_step = 0;
        self.tick_remaining = 0;
        self.last = [0; 2];
        self.power_off();
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::GbDmg)
    }
}

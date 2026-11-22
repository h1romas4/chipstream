//! Virtual Boy VSU based on Rustual Boy revision
//! `447536260072cc8cdfeaac845267057be84df65d`.
//! Copyright (c) 2016-2020 Jake Taylor.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const CLOCKS_PER_SAMPLE: u32 = 120;
const NOISE_TAPS: [u8; 8] = [14, 10, 13, 4, 8, 6, 9, 11];

#[derive(Clone, Copy, Default)]
struct Channel {
    interval: u8,
    interval_counter: u8,
    left: u8,
    right: u8,
    frequency: u16,
    frequency_counter: u16,
    envelope: u8,
    envelope_data: u8,
    envelope_control: u8,
    envelope_counter: u8,
    bank: u8,
    phase: u8,
}

impl Channel {
    fn enabled(self) -> bool {
        self.interval & 0x80 != 0
    }

    fn step_interval(&mut self) {
        if self.enabled() && self.interval & 0x20 != 0 {
            self.interval_counter += 1;
            if self.interval_counter > self.interval & 31 {
                self.interval &= !0x80;
            }
        }
    }

    fn step_envelope(&mut self) {
        if self.envelope_control & 1 == 0 {
            return;
        }
        self.envelope_counter += 1;
        if self.envelope_counter > self.envelope_data & 7 {
            self.envelope_counter = 0;
            if self.envelope_data & 8 != 0 && self.envelope < 15 {
                self.envelope += 1;
            } else if self.envelope_data & 8 == 0 && self.envelope > 0 {
                self.envelope -= 1;
            } else if self.envelope_control & 2 != 0 {
                self.envelope = self.envelope_data >> 4;
            }
        }
    }

    fn frequency_tick(&mut self, frequency: u16) -> bool {
        self.frequency_counter += 1;
        if self.frequency_counter >= 2048 - frequency {
            self.frequency_counter = 0;
            true
        } else {
            false
        }
    }

    fn output(self, sample: u8) -> [u32; 2] {
        if !self.enabled() {
            return [0; 2];
        }
        let level = |volume: u8| {
            if volume == 0 || self.envelope == 0 {
                0
            } else {
                ((u32::from(volume) * u32::from(self.envelope)) >> 3) + 1
            }
        };
        [
            (u32::from(sample) * level(self.left)) >> 1,
            (u32::from(sample) * level(self.right)) >> 1,
        ]
    }
}

/// Virtual Boy's five wavetable channels and noise channel.
///
/// The clock is the VSU clock (normally 5 MHz); the native output rate is
/// `clock / 120`. The 6-bit waveforms, envelopes, duration, noise and channel
/// 5 sweep/modulation follow Rustual Boy. Output is unipolar and no analog
/// high-pass filter is applied. VGM offsets select four-byte-spaced VSU
/// addresses; wave and modulation RAM writes are ignored while any channel is
/// active. Register-driven state tracking does not reflect timed channel
/// expiration or sweep/modulation pitch changes.
#[derive(Clone)]
pub struct Vsu {
    clock: u32,
    waves: [[u8; 32]; 5],
    modulation: [i8; 32],
    channels: [Channel; 6],
    duration_clock: u32,
    envelope_clock: u32,
    noise_clock: u32,
    sweep_clock: u32,
    sweep_counter: u8,
    sweep_control: u8,
    sweep_frequency: u16,
    sweep_next: u16,
    modulation_phase: u8,
    noise_shift: u16,
    noise_output: u8,
}

impl Vsu {
    /// Create a VSU with the given sound clock.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < CLOCKS_PER_SAMPLE {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: CLOCKS_PER_SAMPLE,
            });
        }
        Ok(Self {
            clock,
            waves: [[0; 32]; 5],
            modulation: [0; 32],
            channels: [Channel::default(); 6],
            duration_clock: 0,
            envelope_clock: 0,
            noise_clock: 0,
            sweep_clock: 0,
            sweep_counter: 0,
            sweep_control: 0,
            sweep_frequency: 0,
            sweep_next: 0,
            modulation_phase: 0,
            noise_shift: 0,
            noise_output: 0,
        })
    }

    fn write_register(&mut self, offset: u16, value: u8) {
        let address = (offset << 2) & 0x7ff;
        match address {
            0x000..=0x27f => {
                if !self.channels.iter().any(|channel| channel.enabled()) {
                    self.waves[usize::from(address >> 7)][usize::from((address >> 2) & 31)] =
                        value & 63;
                }
            }
            0x280..=0x2ff => {
                if !self.channels.iter().any(|channel| channel.enabled()) {
                    self.modulation[usize::from((address - 0x280) >> 2)] = value as i8;
                }
            }
            0x400..=0x57f => {
                let index = usize::from((address - 0x400) >> 6);
                let register = address & 0x3f;
                let channel = &mut self.channels[index];
                match register {
                    0x00 => {
                        channel.interval = value;
                        if value & 0x20 != 0 {
                            channel.interval_counter = 0;
                        }
                        if value & 0x80 != 0 {
                            channel.envelope_counter = 0;
                            channel.frequency_counter = 0;
                            channel.phase = 0;
                            if index == 4 {
                                self.sweep_counter = 0;
                                self.modulation_phase = 0;
                            } else if index == 5 {
                                self.noise_shift = 0x7fff;
                            }
                        }
                    }
                    0x04 => {
                        channel.left = value >> 4;
                        channel.right = value & 15;
                    }
                    0x08 => {
                        channel.frequency = (channel.frequency & 0x700) | u16::from(value);
                        if index == 4 {
                            self.sweep_next = (self.sweep_next & 0x700) | u16::from(value);
                        }
                    }
                    0x0c => {
                        channel.frequency =
                            (channel.frequency & 0xff) | (u16::from(value & 7) << 8);
                        if index == 4 {
                            self.sweep_next =
                                (self.sweep_next & 0xff) | (u16::from(value & 7) << 8);
                        }
                    }
                    0x10 => {
                        channel.envelope_data = value;
                        channel.envelope = value >> 4;
                    }
                    0x14 => channel.envelope_control = value,
                    0x18 if index < 5 => channel.bank = value & 7,
                    0x1c if index == 4 => self.sweep_control = value,
                    _ => {}
                }
            }
            0x580 if value & 1 != 0 => {
                for channel in &mut self.channels {
                    channel.interval &= !0x80;
                }
            }
            _ => {}
        }
    }

    fn step_sweep(&mut self) {
        let interval = (self.sweep_control >> 4) & 7;
        self.sweep_counter += 1;
        if self.sweep_counter < interval {
            return;
        }
        self.sweep_counter = 0;
        self.sweep_frequency = self.sweep_next;
        let channel = &mut self.channels[4];
        if !channel.enabled() || channel.envelope_control & 0x40 == 0 || interval == 0 {
            return;
        }
        if channel.envelope_control & 0x10 != 0 {
            self.sweep_next = channel
                .frequency
                .wrapping_add(self.modulation[usize::from(self.modulation_phase)] as u16)
                & 0x7ff;
            if channel.envelope_control & 0x20 != 0 || self.modulation_phase < 31 {
                self.modulation_phase = (self.modulation_phase + 1) & 31;
            }
        } else {
            let shift = self.sweep_frequency >> (self.sweep_control & 7);
            self.sweep_next = if self.sweep_control & 8 != 0 {
                self.sweep_frequency.wrapping_add(shift)
            } else {
                self.sweep_frequency.wrapping_sub(shift)
            } & 0x7ff;
        }
    }

    fn tick(&mut self) {
        self.duration_clock += 1;
        if self.duration_clock == 19_200 {
            self.duration_clock = 0;
            for channel in &mut self.channels {
                channel.step_interval();
            }
        }
        self.envelope_clock += 1;
        if self.envelope_clock == 76_800 {
            self.envelope_clock = 0;
            for channel in &mut self.channels {
                channel.step_envelope();
            }
        }
        for (index, channel) in self.channels[..5].iter_mut().enumerate() {
            let frequency = if index == 4 {
                self.sweep_frequency
            } else {
                channel.frequency
            };
            if channel.frequency_tick(frequency) {
                channel.phase = (channel.phase + 1) & 31;
            }
        }
        self.sweep_clock += 1;
        let sweep_period = if self.sweep_control & 0x80 != 0 {
            38_400
        } else {
            4_800
        };
        if self.sweep_clock >= sweep_period {
            self.sweep_clock = 0;
            self.step_sweep();
        }
        self.noise_clock += 1;
        if self.noise_clock == 10 {
            self.noise_clock = 0;
            let channel = &mut self.channels[5];
            if channel.frequency_tick(channel.frequency) {
                let tap = NOISE_TAPS[usize::from((channel.envelope_control >> 4) & 7)];
                let bit = ((self.noise_shift >> 7) ^ (self.noise_shift >> tap)) & 1;
                self.noise_shift = ((self.noise_shift << 1) | bit) & 0x7fff;
                self.noise_output = if bit == 0 { 63 } else { 0 };
            }
        }
    }

    fn sample(&self) -> [i32; 2] {
        let mut output = [0_u32; 2];
        for (index, channel) in self.channels.iter().copied().enumerate() {
            let sample = if index == 5 {
                self.noise_output
            } else if channel.bank < 5 {
                self.waves[usize::from(channel.bank)][usize::from(channel.phase)]
            } else {
                0
            };
            let values = channel.output(sample);
            output[0] += values[0];
            output[1] += values[1];
        }
        [
            ((output[0] & 0xfff8) << 2) as i16 as i32,
            ((output[1] & 0xfff8) << 2) as i16 as i32,
        ]
    }
}

impl SoundChip for Vsu {
    fn chip_type(&self) -> &Chip {
        &Chip::Vsu
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / CLOCKS_PER_SAMPLE
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(CLOCKS_PER_SAMPLE).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(u16::from_be_bytes([bank, register]), value);
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
            for _ in 0..CLOCKS_PER_SAMPLE {
                self.tick();
            }
            frame.copy_from_slice(&self.sample());
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
        super::restore_cloned_state(self, state, Chip::Vsu)
    }
}

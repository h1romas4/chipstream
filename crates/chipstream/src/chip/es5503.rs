//! ES5503 DOC audio based on MAME revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) R. Belmont.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const WAVE_MASKS: [u32; 8] = [
    0x1ff00, 0x1fe00, 0x1fc00, 0x1f800, 0x1f000, 0x1e000, 0x1c000, 0x18000,
];

#[derive(Clone, Copy)]
struct Oscillator {
    frequency: u16,
    control: u8,
    volume: u8,
    data: u8,
    pointer: u32,
    size: u8,
    resolution: u8,
    accumulator: u32,
    previous_halt: bool,
}

impl Default for Oscillator {
    fn default() -> Self {
        Self {
            frequency: u16::MAX,
            control: u8::MAX,
            volume: u8::MAX,
            data: u8::MAX,
            pointer: 0x1ff00,
            size: 7,
            resolution: 7,
            accumulator: 0xffffff,
            previous_halt: true,
        }
    }
}

/// Ensoniq ES5503 with 32 oscillators and 128 KiB of waveform RAM.
///
/// The native rate changes when register `0xe1` changes the oscillator count.
/// Hardware output channels are mixed to stereo as in libvgm. Oscillators are
/// processed in complete rounds rather than individual clock slots; the ADC,
/// interrupt pin, and time-dependent register drift are not emulated. The high
/// byte of an ES5503 VGM register address is ignored, as in libvgm.
#[derive(Clone)]
pub struct Es5503 {
    clock: u32,
    output_channels: u8,
    oscillators: [Oscillator; 32],
    enabled: u8,
    scanning: bool,
    ram: Vec<u8>,
    volume: [i32; 256],
}

impl Es5503 {
    /// Create an ES5503 with one physical output channel.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        Self::with_output_channels(clock, 1)
    }

    /// Create an ES5503 with one to sixteen physical output channels.
    pub fn with_output_channels(clock: u32, output_channels: u8) -> Result<Self, ChipError> {
        if clock < 272 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 272,
            });
        }
        if !(1..=16).contains(&output_channels) {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Es5503,
                reason: "output channel count must be between 1 and 16",
            });
        }
        let conduct = |volume: f64| (2.77 * ((volume - 22.1) / 2.77).exp().ln_1p()).powf(1.104);
        let scale = 1.0042 / conduct(255.0);
        let volume = std::array::from_fn(|index| {
            if index == 0 {
                0
            } else {
                ((conduct(index as f64) * scale - 0.0042) * 65280.0).round() as i32
            }
        });
        Ok(Self {
            clock,
            output_channels,
            oscillators: [Oscillator::default(); 32],
            enabled: 1,
            scanning: false,
            ram: vec![0; 0x20000],
            volume,
        })
    }

    fn write_register(&mut self, register: u8, value: u8) -> Result<(), ChipError> {
        if register < 0xe0 {
            let osc = &mut self.oscillators[usize::from(register & 31)];
            match register & 0xe0 {
                0x00 => osc.frequency = (osc.frequency & 0xff00) | u16::from(value),
                0x20 => osc.frequency = (osc.frequency & 0xff) | (u16::from(value) << 8),
                0x40 => osc.volume = value,
                0x60 => {}
                0x80 => osc.pointer = (osc.pointer & 0x10000) | (u32::from(value) << 8),
                0xa0 => osc.control = value,
                0xc0 => {
                    osc.pointer = (osc.pointer & 0xffff) | (u32::from(value & 0x40) << 10);
                    osc.size = (value >> 3) & 7;
                    osc.resolution = value & 7;
                }
                _ => unreachable!(),
            }
        } else {
            match register {
                0xe0 | 0xe2 => {}
                0xe1 => {
                    self.enabled = ((value >> 1) & 31) + 1;
                    self.scanning = true;
                }
                _ => {
                    return Err(ChipError::InvalidRegister {
                        chip: Chip::Es5503,
                        register,
                    });
                }
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> [i32; 2] {
        if !self.scanning {
            return [0; 2];
        }
        let mut output = [0_i64; 2];
        let channel_mask = self.output_channels.next_power_of_two() - 1;
        for index in 0..usize::from(self.enabled) {
            let mut osc = self.oscillators[index];
            let mut partner = self.oscillators[index ^ 1];
            let even = index & 1 == 0;
            let halted = osc.control & 1 != 0;
            let mode = (osc.control >> 1) & 3;
            let partner_swapped = partner.control & 1 != 0 && (partner.control >> 1) & 3 == 3;
            if osc.previous_halt && mode & 1 != 0 {
                osc.accumulator = 0;
            }
            osc.previous_halt = halted;
            let shift = 9 + osc.resolution - osc.size;
            let top = 1_u32 << (17 + osc.resolution);
            let mask = (256_u32 << osc.size) - 1;
            let address = (osc.pointer & WAVE_MASKS[usize::from(osc.size)])
                + ((osc.accumulator >> shift) & mask);
            let sample = self.ram[address as usize];
            if sample != 0 {
                osc.data = sample;
            }
            let mut ended = false;
            if !halted {
                if sample != 0 {
                    if mode == 2 && !even {
                        if index < 31 && self.oscillators[index + 1].control & 1 == 0 {
                            self.oscillators[index + 1].volume = sample;
                        }
                    } else {
                        let gain = if index + 1 == usize::from(self.enabled) {
                            3
                        } else {
                            1
                        };
                        let value = i64::from(i32::from(sample) - 128)
                            * i64::from(self.volume[usize::from(osc.volume)])
                            * gain;
                        let channel = (osc.control >> 4) & channel_mask;
                        if channel < self.output_channels & !1 {
                            output[usize::from(channel & 1)] += value;
                        } else if channel < self.output_channels {
                            let centered = (value * 181) >> 8;
                            output[0] += centered;
                            output[1] += centered;
                        }
                    }
                }
                osc.accumulator = (osc.accumulator & (top - 1)) + u32::from(osc.frequency);
                if sample == 0 {
                    osc.control |= 1;
                    ended = true;
                } else if osc.accumulator >= top {
                    osc.accumulator &= top - 1;
                    ended = true;
                    match mode {
                        1 if !even || !partner_swapped => osc.control |= 1,
                        2 if even => {
                            osc.accumulator = 0;
                            partner.accumulator = 0;
                        }
                        3 => osc.control |= 1,
                        _ => {}
                    }
                }
            } else {
                ended = sample == 0;
                if even && partner_swapped && sample != 0 {
                    osc.control &= !1;
                }
            }
            if ended && even && mode == 3 && osc.control & 1 != 0 {
                partner.control &= !1;
            }
            self.oscillators[index] = osc;
            self.oscillators[index ^ 1] = partner;
        }
        [(output[0] >> 11) as i32, (output[1] >> 11) as i32]
    }
}

impl SoundChip for Es5503 {
    fn chip_type(&self) -> &Chip {
        &Chip::Es5503
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
            NonZeroU32::new(8 * (u32::from(self.enabled) + 2)).unwrap(),
        )
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (_, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(register, value)
    }
    fn write_ram(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= self.ram.len())
            .ok_or(ChipError::RamOutOfRange {
                chip: Chip::Es5503,
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
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Es5503)
    }
    fn reset(&mut self) {
        self.enabled = 1;
        self.scanning = false;
    }
}

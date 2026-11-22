//! RF5C68 and RF5C164 PCM playback based on MAME revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Olivier Galibert and Aaron Giles.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

#[derive(Clone, Copy, Default)]
struct Channel {
    enabled: bool,
    envelope: u8,
    pan: u8,
    start: u8,
    address: u32,
    step: u16,
    loop_start: u16,
}

/// Eight-channel RF5C68 or RF5C164 PCM generator with 64 KiB of sample RAM.
///
/// The 16-bit VGM write specifications address the selected 4 KiB RAM window;
/// [`SoundChip::write_ram`] instead uses absolute addresses. Reset preserves RAM.
/// The VGM sequencer resolves the current bank for RAM blocks and PCM copies.
/// Register write specifications for the other model return a chip mismatch.
#[derive(Clone)]
pub struct Rf5c {
    chip: Chip,
    clock: u32,
    channels: [Channel; 8],
    channel_bank: usize,
    wave_bank: usize,
    enabled: bool,
    ram: Vec<u8>,
}

impl Rf5c {
    /// Create an RF5C68 or RF5C164 with its input clock in hertz.
    pub fn new(chip: Chip, clock: u32) -> Result<Self, ChipError> {
        if !matches!(chip, Chip::Rf5c68 | Chip::Rf5c164) {
            return Err(ChipError::UnsupportedChip(chip));
        }
        if clock < 384 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 384,
            });
        }
        Ok(Self {
            chip,
            clock,
            channels: [Channel::default(); 8],
            channel_bank: 0,
            wave_bank: 0,
            enabled: false,
            ram: vec![0; 65536],
        })
    }

    fn write_register(&mut self, register: u8, value: u8) -> Result<(), ChipError> {
        let channel = &mut self.channels[self.channel_bank];
        match register {
            0 => channel.envelope = value,
            1 => channel.pan = value,
            2 => channel.step = (channel.step & 0xff00) | u16::from(value),
            3 => channel.step = (channel.step & 0x00ff) | (u16::from(value) << 8),
            4 => channel.loop_start = (channel.loop_start & 0xff00) | u16::from(value),
            5 => channel.loop_start = (channel.loop_start & 0x00ff) | (u16::from(value) << 8),
            6 => {
                channel.start = value;
                if !channel.enabled {
                    channel.address = u32::from(value) << 19;
                }
            }
            7 => {
                self.enabled = value & 0x80 != 0;
                if value & 0x40 != 0 {
                    self.channel_bank = usize::from(value & 7);
                } else {
                    self.wave_bank = usize::from(value & 15) << 12;
                }
            }
            8 => {
                for (index, channel) in self.channels.iter_mut().enumerate() {
                    channel.enabled = value & (1 << index) == 0;
                    if !channel.enabled {
                        channel.address = u32::from(channel.start) << 19;
                    }
                }
            }
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: self.chip.clone(),
                    register,
                });
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> [i32; 2] {
        let mut output = [0; 2];
        if !self.enabled {
            return output;
        }
        for channel in self.channels.iter_mut().filter(|channel| channel.enabled) {
            let mut sample = self.ram[((channel.address >> 11) & 0xffff) as usize];
            if sample == 0xff {
                channel.address = u32::from(channel.loop_start) << 11;
                sample = self.ram[usize::from(channel.loop_start)];
                if sample == 0xff {
                    continue;
                }
            }
            channel.address = channel.address.wrapping_add(u32::from(channel.step));
            let magnitude = i32::from(sample & 0x7f);
            let left = (magnitude * i32::from(channel.pan & 15) * i32::from(channel.envelope)) >> 5;
            let right =
                (magnitude * i32::from(channel.pan >> 4) * i32::from(channel.envelope)) >> 5;
            let sign = if sample & 0x80 != 0 { 1 } else { -1 };
            output[0] += left * sign;
            output[1] += right * sign;
        }
        for value in &mut output {
            if self.chip == Chip::Rf5c68 {
                *value &= !63;
            }
            *value = (*value).clamp(-32768, 32767);
        }
        output
    }
}

impl SoundChip for Rf5c {
    fn chip_type(&self) -> &Chip {
        &self.chip
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 384
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(384).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let expected = write.chip();
        if expected != self.chip {
            return Err(ChipError::ChipMismatch {
                expected,
                actual: self.chip.clone(),
            });
        }
        match write {
            ChipRegisterWrite::Rf5c68U8(spec) => self.write_register(spec.offset, spec.value),
            ChipRegisterWrite::Rf5c164U8(spec) => self.write_register(spec.offset, spec.value),
            ChipRegisterWrite::Rf5c68U16(spec) => {
                self.ram[self.wave_bank | (usize::from(spec.offset) & 0xfff)] = spec.value;
                Ok(())
            }
            ChipRegisterWrite::Rf5c164U16(spec) => {
                self.ram[self.wave_bank | (usize::from(spec.offset) & 0xfff)] = spec.value;
                Ok(())
            }
            _ => unreachable!("chip type checked above"),
        }
    }
    fn write_ram(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= self.ram.len())
            .ok_or(ChipError::RamOutOfRange {
                chip: self.chip.clone(),
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
        let Some(snapshot) = state.as_any().downcast_ref::<Self>() else {
            return Err(ChipError::InvalidStateSnapshot(self.chip.clone()));
        };
        if snapshot.chip != self.chip {
            return Err(ChipError::InvalidStateSnapshot(self.chip.clone()));
        }
        *self = snapshot.clone();
        Ok(())
    }
    fn reset(&mut self) {
        self.channels = [Channel::default(); 8];
        self.channel_bank = 0;
        self.wave_bank = 0;
        self.enabled = false;
    }
}

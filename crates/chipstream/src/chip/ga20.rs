//! Based on MAME's Irem GA20 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Acho A. Tang, R. Belmont, and Valley Bell.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

#[derive(Clone, Copy, Default)]
struct Channel {
    rate: u8,
    position: u32,
    counter: u16,
    volume: u16,
    playing: bool,
}

/// Irem GA20 four-channel PCM player.
///
/// The two native outputs carry the same mix, as in MAME; the chip's actual
/// stereo capabilities and end-address registers are not fully understood.
/// Sample ROM is loaded with [`SoundChip::write_rom`]. Unloaded addresses read
/// as zero (the sample terminator), and reset preserves the ROM.
#[derive(Clone)]
pub struct Ga20 {
    clock: u32,
    registers: [u8; 32],
    channels: [Channel; 4],
    rom: Vec<u8>,
}

impl Ga20 {
    /// Create a GA20 with the specified input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 4 {
            return Err(ChipError::InvalidClock { clock, minimum: 4 });
        }
        Ok(Self {
            clock,
            registers: [0; 32],
            channels: [Channel::default(); 4],
            rom: Vec::new(),
        })
    }

    /// Return the playback status for a channel numbered 0 through 3.
    pub fn channel_active(&self, channel: usize) -> Option<bool> {
        self.channels.get(channel).map(|voice| voice.playing)
    }

    fn write_register(&mut self, register: u8, value: u8) {
        let address = usize::from(register & 0x1f);
        self.registers[address] = value;
        let channel = &mut self.channels[address >> 3];
        match address & 7 {
            4 => channel.rate = value,
            5 => channel.volume = u16::from(value) * 256 / (u16::from(value) + 10),
            6 if value & 2 != 0 => {
                let base = address & !7;
                channel.position = u32::from(u16::from_le_bytes([
                    self.registers[base],
                    self.registers[base + 1],
                ])) << 4;
                channel.counter = 0x100;
                channel.playing = true;
            }
            6 => channel.playing = false,
            _ => {}
        }
    }

    fn tick(&mut self) -> i32 {
        let mut sum = 0;
        for channel in &mut self.channels {
            if channel.playing {
                let address = (channel.position & 0x0f_ffff) as usize;
                let sample = self.rom.get(address).copied().unwrap_or(0);
                if sample == 0 {
                    channel.playing = false;
                } else {
                    sum += (i32::from(sample) - 128) * i32::from(channel.volume);
                    channel.counter -= 1;
                    if channel.counter <= u16::from(channel.rate) {
                        channel.position = channel.position.wrapping_add(1);
                        channel.counter = 0x100;
                    }
                }
            }
        }
        sum / 4
    }
}

impl SoundChip for Ga20 {
    fn chip_type(&self) -> &Chip {
        &Chip::Ga20
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.clock / 4
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(4).unwrap())
    }

    fn channels(&self) -> usize {
        2
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (_, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(register, value);
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
            let sample = self.tick();
            *frame = [sample; 2];
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.registers = [0; 32];
        self.channels = [Channel::default(); 4];
    }

    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Ga20)
    }
}

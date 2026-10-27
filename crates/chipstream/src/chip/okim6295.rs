//! Based on MAME's OKIM6295 implementation, revision
//! `92ffd8d0269cd49b0ef256cd3912269387f8d318`.
//! Copyright (c) Mirko Buffoni and Aaron Giles.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::oki_adpcm::{INDEX_SHIFT, difference_table};
use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

#[derive(Clone, Copy, Default)]
struct Voice {
    playing: bool,
    start: u32,
    position: u32,
    count: u32,
    signal: i32,
    step: usize,
    volume: i32,
}

/// Four-voice OKIM6295 ADPCM decoder with mono native-rate output.
///
/// Accepts VGM command, clock, pin 7, and ROM-bank writes. Clock and pin 7
/// writes can change the native rate; the four clock bytes are committed
/// when register `0x0b` is written. [`Self::status`] reports active voices.
pub struct Okim6295 {
    clock: u32,
    clock_latch: u32,
    pin7: bool,
    voices: [Voice; 4],
    command: Option<u8>,
    bank: u8,
    nmk_enable: u8,
    nmk_banks: [u8; 4],
    rom: Vec<u8>,
    differences: [[i32; 16]; 49],
}

impl Okim6295 {
    /// Create a chip using the `/165` divider (pin 7 low).
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        Self::with_pin7(clock, false)
    }

    /// Create a chip with the initial pin 7 state.
    pub fn with_pin7(clock: u32, pin7: bool) -> Result<Self, ChipError> {
        let minimum = if pin7 { 132 } else { 165 };
        if clock < minimum {
            return Err(ChipError::InvalidClock { clock, minimum });
        }
        Ok(Self {
            clock,
            clock_latch: clock,
            pin7,
            voices: [Voice::default(); 4],
            command: None,
            bank: 0,
            nmk_enable: 0,
            nmk_banks: [0; 4],
            rom: Vec::new(),
            differences: difference_table(),
        })
    }

    /// Voice activity status; upper bits read as one.
    pub fn status(&self) -> u8 {
        self.voices
            .iter()
            .enumerate()
            .fold(0xf0, |status, (i, voice)| {
                status | (u8::from(voice.playing) << i)
            })
    }

    /// Change pin 7 without resetting playing voices.
    pub fn set_pin7(&mut self, high: bool) -> Result<(), ChipError> {
        let divisor = if high { 132 } else { 165 };
        if self.clock < divisor {
            return Err(ChipError::InvalidClock {
                clock: self.clock,
                minimum: divisor,
            });
        }
        self.pin7 = high;
        Ok(())
    }

    fn read_rom(&self, address: u32) -> u8 {
        let offset = if self.nmk_enable != 0 {
            if address < 0x400 && self.nmk_enable & 0x80 != 0 {
                (u32::from(self.nmk_banks[((address >> 8) & 3) as usize]) << 16) | (address & 0x3ff)
            } else {
                (u32::from(self.nmk_banks[((address >> 16) & 3) as usize]) << 16)
                    | (address & 0xffff)
            }
        } else {
            (u32::from(self.bank) * 0x40000) | address
        };
        self.rom.get(offset as usize).copied().unwrap_or(0)
    }

    fn write_command(&mut self, value: u8) {
        const VOLUME: [i32; 16] = [32, 22, 16, 11, 8, 6, 4, 3, 2, 0, 0, 0, 0, 0, 0, 0];
        if let Some(sample) = self.command.take() {
            let base = u32::from(sample) * 8;
            let read_address = |offset| {
                (u32::from(self.read_rom(base + offset)) << 16)
                    | (u32::from(self.read_rom(base + offset + 1)) << 8)
                    | u32::from(self.read_rom(base + offset + 2))
            };
            let start = read_address(0) & 0x3ffff;
            let end = read_address(3) & 0x3ffff;
            if start < end {
                for (i, voice) in self.voices.iter_mut().enumerate() {
                    if value & (0x10 << i) != 0 && !voice.playing {
                        *voice = Voice {
                            playing: true,
                            start,
                            position: 0,
                            count: 2 * (end - start + 1),
                            signal: 0,
                            step: 0,
                            volume: VOLUME[usize::from(value & 15)],
                        };
                    }
                }
            }
        } else if value & 0x80 != 0 {
            self.command = Some(value & 0x7f);
        } else {
            for (i, voice) in self.voices.iter_mut().enumerate() {
                if value & (8 << i) != 0 {
                    voice.playing = false;
                }
            }
        }
    }

    fn tick(&mut self) -> i32 {
        let mut output = 0;
        for i in 0..4 {
            let voice = self.voices[i];
            if !voice.playing {
                continue;
            }
            let byte = self.read_rom(voice.start + voice.position / 2);
            let nibble = if voice.position & 1 == 0 {
                byte >> 4
            } else {
                byte & 15
            };
            let voice = &mut self.voices[i];
            voice.signal = (voice.signal + self.differences[voice.step][usize::from(nibble)])
                .clamp(-2048, 2047);
            voice.step =
                (voice.step as i32 + INDEX_SHIFT[usize::from(nibble & 7)]).clamp(0, 48) as usize;
            output += voice.signal * voice.volume / 8;
            voice.position += 1;
            if voice.position >= voice.count {
                voice.playing = false;
            }
        }
        output
    }
}

impl SoundChip for Okim6295 {
    fn chip_type(&self) -> &Chip {
        &Chip::Okim6295
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
            NonZeroU32::new(if self.pin7 { 132 } else { 165 }).unwrap(),
        )
    }
    fn channels(&self) -> usize {
        1
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        if bank != 0 {
            return Err(ChipError::InvalidRegisterBank {
                chip: Chip::Okim6295,
                bank,
            });
        }
        match register {
            0 => self.write_command(value),
            0x08..=0x0a => {
                let shift = u32::from(register - 8) * 8;
                self.clock_latch =
                    (self.clock_latch & !(0xff << shift)) | (u32::from(value) << shift);
            }
            0x0b => {
                let clock = (self.clock_latch & 0x00ff_ffff) | (u32::from(value & 0x7f) << 24);
                let minimum = if self.pin7 { 132 } else { 165 };
                if clock < minimum {
                    return Err(ChipError::InvalidClock { clock, minimum });
                }
                self.clock = clock;
                self.clock_latch = clock;
            }
            0x0c => self.set_pin7(value != 0)?,
            0x0e => self.nmk_enable = value,
            0x0f => self.bank = value,
            0x10..=0x13 => self.nmk_banks[usize::from(register - 0x10)] = value,
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: Chip::Okim6295,
                    register,
                });
            }
        }
        Ok(())
    }
    fn write_rom(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        write_rom_data(&mut self.rom, start, data)
    }
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        for sample in buffer {
            *sample = self.tick();
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.voices = [Voice::default(); 4];
        self.command = None;
    }
}

//! Based on MAME's K054539 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Olivier Galibert.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const DPCM: [i32; 16] = [
    0, 256, 512, 1024, 2048, 4096, 8192, 16384, 0, -16384, -8192, -4096, -2048, -1024, -512, -256,
];

/// K054539 playback options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct K054539Config {
    /// Swap the native left and right outputs.
    pub reverse_stereo: bool,
    /// Mute the reverb return while continuing to update its RAM.
    pub disable_reverb: bool,
    /// Latch position writes until key-on while PCM is enabled.
    pub update_at_keyon: bool,
}

impl Default for K054539Config {
    fn default() -> Self {
        Self {
            reverse_stereo: false,
            disable_reverb: false,
            update_at_keyon: true,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Voice {
    position: i32,
    fraction: i32,
    value: i32,
    previous: i32,
}

/// Konami K054539 eight-voice PCM/DPCM generator with stereo reverb.
///
/// Sample ROM is provided via [`SoundChip::write_rom`]; unloaded addresses
/// read as zero. Timer and analog-input pins are not emulated. Reverb follows
/// MAME's approximate implementation. Reset preserves registers, positions,
/// ROM and configuration, but disables playback and clears reverb RAM.
#[derive(Clone)]
pub struct K054539 {
    clock: u32,
    config: K054539Config,
    registers: [u8; 0x230],
    position_latch: [[u8; 3]; 8],
    voices: [Voice; 8],
    ram: Vec<u8>,
    reverb_pos: usize,
    data_pointer: u32,
    data_bank: u8,
    rom: Vec<u8>,
    volume: [f64; 256],
    pan: [f64; 15],
}

impl K054539 {
    /// Create a chip using MAME's default key-on position latching.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        Self::with_config(clock, K054539Config::default())
    }

    /// Create a chip with explicit playback options.
    pub fn with_config(clock: u32, config: K054539Config) -> Result<Self, ChipError> {
        if clock < 384 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 384,
            });
        }
        Ok(Self {
            clock,
            config,
            registers: [0; 0x230],
            position_latch: [[0; 3]; 8],
            voices: [Voice::default(); 8],
            ram: vec![0; 0x8000],
            reverb_pos: 0,
            data_pointer: 0,
            data_bank: 0,
            rom: Vec::new(),
            volume: std::array::from_fn(|i| 10_f64.powf(-36.0 * i as f64 / 64.0 / 20.0) / 4.0),
            pan: std::array::from_fn(|i| (i as f64).sqrt() / 14_f64.sqrt()),
        })
    }

    fn rom_byte(&self, address: i32) -> u8 {
        self.rom
            .get((address as u32 & 0xff_ffff) as usize)
            .copied()
            .unwrap_or(0)
    }

    fn data_address(&self) -> usize {
        ((self.data_pointer & 0x3fff) | ((self.data_pointer & 0x10000) >> 2)) as usize
    }

    fn register_index(register: u16) -> Result<usize, ChipError> {
        let [bank, offset] = register.to_be_bytes();
        if bank > 2 {
            return Err(ChipError::InvalidRegisterBank {
                chip: Chip::K054539,
                bank,
            });
        }
        if register >= 0x230 {
            return Err(ChipError::InvalidRegister {
                chip: Chip::K054539,
                register: offset,
            });
        }
        Ok(usize::from(register))
    }

    /// Read a register, including the auto-incrementing ROM/RAM data port.
    pub fn read_register(&mut self, register: u16) -> Result<u8, ChipError> {
        let index = Self::register_index(register)?;
        if index == 0x22d {
            if self.registers[0x22f] & 0x10 == 0 {
                return Ok(0);
            }
            let value = if self.data_bank == 0x80 {
                self.ram[self.data_address()]
            } else {
                self.rom_byte((u32::from(self.data_bank) * 0x20000 + self.data_pointer) as i32)
            };
            self.data_pointer = (self.data_pointer + 1) & 0x1ffff;
            Ok(value)
        } else {
            Ok(self.registers[index])
        }
    }

    fn write_register(&mut self, register: u16, value: u8) -> Result<(), ChipError> {
        let index = Self::register_index(register)?;
        let latch = self.config.update_at_keyon && self.registers[0x22f] & 1 != 0;
        if latch && index < 0x100 && (0x0c..=0x0e).contains(&(index & 0x1f)) {
            self.position_latch[index >> 5][(index & 0x1f) - 0x0c] = value;
            return Ok(());
        }
        match index {
            0x214 => {
                for ch in 0..8 {
                    if value & (1 << ch) != 0 {
                        if latch {
                            let start = ch * 0x20 + 0x0c;
                            self.registers[start..start + 3]
                                .copy_from_slice(&self.position_latch[ch]);
                        }
                        if self.registers[0x22f] & 0x80 == 0 {
                            self.registers[0x22c] |= 1 << ch;
                        }
                    }
                }
            }
            0x215 if self.registers[0x22f] & 0x80 == 0 => self.registers[0x22c] &= !value,
            0x22d => {
                if self.data_bank == 0x80 {
                    let address = self.data_address();
                    self.ram[address] = value;
                }
                self.data_pointer = (self.data_pointer + 1) & 0x1ffff;
            }
            0x22e => {
                self.data_bank = value;
                self.data_pointer = 0;
            }
            _ => {}
        }
        self.registers[index] = value;
        Ok(())
    }

    fn word24(&self, offset: usize) -> i32 {
        i32::from(self.registers[offset])
            | (i32::from(self.registers[offset + 1]) << 8)
            | (i32::from(self.registers[offset + 2]) << 16)
    }

    fn reverb_sample(&self, pos: usize) -> i16 {
        i16::from_le_bytes([self.ram[pos * 2], self.ram[pos * 2 + 1]])
    }

    fn set_reverb_sample(&mut self, pos: usize, value: i16) {
        self.ram[pos * 2..pos * 2 + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn tick(&mut self) -> [i32; 2] {
        if self.registers[0x22f] & 1 == 0 {
            return [0; 2];
        }
        let reverb = if self.config.disable_reverb {
            0.0
        } else {
            f64::from(self.reverb_sample(self.reverb_pos))
        };
        let mut output = [reverb; 2];
        self.set_reverb_sample(self.reverb_pos, 0);
        for ch in 0..8 {
            if self.registers[0x22c] & (1 << ch) == 0 {
                continue;
            }
            let base = ch * 0x20;
            let mode = self.registers[0x200 + ch * 2];
            let looping = self.registers[0x201 + ch * 2] & 1 != 0;
            let delta = self.word24(base) * if mode & 0x20 != 0 { -1 } else { 1 };
            let step = if delta < 0 || mode & 0x20 != 0 { -1 } else { 1 };
            let mut pos = self.word24(base + 0x0c);
            let mut voice = self.voices[ch];
            if pos != voice.position {
                voice.fraction = 0;
                voice.value = 0;
                voice.previous = 0;
            }
            let format = mode & 0x0c;
            if format == 8 {
                pos <<= 1;
                voice.fraction <<= 1;
                if voice.fraction & 0x10000 != 0 {
                    voice.fraction &= 0xffff;
                    pos |= 1;
                }
            }
            if format != 0x0c {
                voice.fraction += delta;
                while voice.fraction & !0xffff != 0 {
                    voice.fraction -= step * 0x10000;
                    pos += step * if format == 4 { 2 } else { 1 };
                    voice.previous = voice.value;
                    let read = |p: i32| -> i32 {
                        match format {
                            0 => i32::from((u16::from(self.rom_byte(p)) << 8) as i16),
                            4 => i32::from(i16::from_le_bytes([
                                self.rom_byte(p),
                                self.rom_byte(p + 1),
                            ])),
                            _ => i32::from(self.rom_byte(p >> 1)),
                        }
                    };
                    let marker = if format == 8 { 0x88 } else { -32768 };
                    let mut value = read(pos);
                    if value == marker && looping {
                        pos = self.word24(base + 8) << u32::from(format == 8);
                        value = read(pos);
                    }
                    if value == marker {
                        if self.registers[0x22f] & 0x80 == 0 {
                            self.registers[0x22c] &= !(1 << ch);
                        }
                        voice.value = 0;
                        break;
                    }
                    voice.value = if format == 8 {
                        let nibble = if pos & 1 != 0 { value >> 4 } else { value & 15 };
                        (voice.previous + DPCM[nibble as usize]).clamp(-32768, 32767)
                    } else {
                        value
                    };
                }
            }
            if format == 8 {
                voice.fraction >>= 1;
                if pos & 1 != 0 {
                    voice.fraction |= 0x8000;
                }
                pos >>= 1;
            }
            let volume = usize::from(self.registers[base + 3]);
            let reverb_volume = (volume + usize::from(self.registers[base + 4])).min(255);
            let pan = match self.registers[base + 5] {
                p @ 0x81..=0x8f => usize::from(p - 0x81),
                p @ 0x11..=0x1f => usize::from(p - 0x11),
                _ => 7,
            };
            output[0] += f64::from(voice.value) * self.volume[volume] * self.pan[pan];
            output[1] += f64::from(voice.value) * self.volume[volume] * self.pan[14 - pan];
            let delay = usize::from(u16::from_le_bytes([
                self.registers[base + 6],
                self.registers[base + 7],
            ])) >> 3;
            let target = (delay + 2 * self.reverb_pos) & 0x1fff;
            let wet = (f64::from(voice.value) * self.volume[reverb_volume] / 2.0) as i16;
            self.set_reverb_sample(target, self.reverb_sample(target).wrapping_add(wet));
            voice.position = pos;
            self.voices[ch] = voice;
            if self.registers[0x22f] & 0x80 == 0 {
                for i in 0..3 {
                    self.registers[base + 0x0c + i] = (pos >> (8 * i)) as u8;
                }
            }
        }
        self.reverb_pos = (self.reverb_pos + 1) & 0x1fff;
        if self.config.reverse_stereo {
            output.swap(0, 1);
        }
        output.map(|value| value as i32)
    }
}

impl SoundChip for K054539 {
    fn chip_type(&self) -> &Chip {
        &Chip::K054539
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
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(u16::from_be_bytes([bank, register]), value)
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
            *frame = self.tick();
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.registers[0x22c] = 0;
        self.registers[0x22f] = 0;
        self.ram.fill(0);
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::K054539)
    }
}

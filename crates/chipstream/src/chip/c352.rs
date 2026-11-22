//! Based on MAME's C352 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) R. Belmont and superctr.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const BUSY: u16 = 0x8000;
const KEY_ON: u16 = 0x4000;
const KEY_OFF: u16 = 0x2000;
const LOOP_HISTORY: u16 = 0x0800;
const PHASE_RL: u16 = 0x0200;
const PHASE_FL: u16 = 0x0100;
const PHASE_R: u16 = 0x0080;
const LOOP_DIRECTION: u16 = 0x0040;
const LINK: u16 = 0x0020;
const NOISE: u16 = 0x0010;
const MULAW: u16 = 0x0008;
const FILTER_DISABLE: u16 = 0x0004;
const LOOP: u16 = 0x0002;
const REVERSE: u16 = 0x0001;

#[derive(Clone, Copy, Default)]
struct Voice {
    position: u32,
    counter: u32,
    sample: i16,
    previous: i16,
    volume_front: u16,
    volume_rear: u16,
    current_volume: [u8; 4],
    frequency: u16,
    flags: u16,
    bank: u16,
    start: u16,
    end: u16,
    loop_start: u16,
}

/// Namco C352 PCM chip with 32 voices and four native outputs.
///
/// Output order is front left, front right, rear left, rear right. Sample ROM
/// is supplied via [`SoundChip::write_rom`]; unloaded addresses read as zero.
/// Reset clears playback state but preserves ROM and the clock divider.
#[derive(Clone)]
pub struct C352 {
    clock: u32,
    divider: u32,
    voices: [Voice; 32],
    random: u16,
    control: u16,
    rom: Vec<u8>,
}

impl C352 {
    /// Create a C352 with the standard hardware clock divider of 576.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        Self::with_divider(clock, 576)
    }

    /// Create a C352 with a specific input-clock divider.
    pub fn with_divider(clock: u32, divider: u32) -> Result<Self, ChipError> {
        if divider == 0 {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::C352,
                reason: "clock divider must be nonzero",
            });
        }
        if clock < divider {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: divider,
            });
        }
        Ok(Self {
            clock,
            divider,
            voices: [Voice::default(); 32],
            random: 0x1234,
            control: 0,
            rom: Vec::new(),
        })
    }

    /// Read a 16-bit word-addressed register.
    pub fn read_register(&self, register: u16) -> u16 {
        if register < 0x100 {
            let voice = &self.voices[usize::from(register / 8)];
            match register % 8 {
                0 => voice.volume_front,
                1 => voice.volume_rear,
                2 => voice.frequency,
                3 => voice.flags,
                4 => voice.bank,
                5 => voice.start,
                6 => voice.end,
                _ => voice.loop_start,
            }
        } else if register == 0x200 {
            self.control
        } else {
            0
        }
    }

    fn write_register(&mut self, register: u16, value: u16) {
        if register < 0x100 {
            let voice = &mut self.voices[usize::from(register / 8)];
            match register % 8 {
                0 => voice.volume_front = value,
                1 => voice.volume_rear = value,
                2 => voice.frequency = value,
                3 => voice.flags = value,
                4 => voice.bank = value,
                5 => voice.start = value,
                6 => voice.end = value,
                _ => voice.loop_start = value,
            }
        } else if register == 0x200 {
            self.control = value;
        } else if register == 0x202 {
            for voice in &mut self.voices {
                if voice.flags & KEY_ON != 0 {
                    voice.position = (u32::from(voice.bank) << 16) | u32::from(voice.start);
                    voice.sample = 0;
                    voice.previous = 0;
                    voice.counter = 0xffff;
                    voice.flags = (voice.flags | BUSY) & !(KEY_ON | LOOP_HISTORY);
                    voice.current_volume = [0; 4];
                }
                if voice.flags & KEY_OFF != 0 {
                    voice.flags &= !(BUSY | KEY_OFF);
                    voice.counter = 0xffff;
                }
            }
        }
    }

    fn decode_mulaw(sample: u8) -> i16 {
        let value = i32::from(sample & 0x7f);
        let magnitude = match value {
            0..=15 => value,
            16..=23 => 16 + (value - 16) * 2,
            24..=47 => 32 + (value - 24) * 4,
            48..=99 => 128 + (value - 48) * 8,
            _ => 544 + (value - 100) * 16,
        } << 5;
        if sample & 0x80 != 0 {
            (!magnitude & 0xffe0) as i16
        } else {
            magnitude as i16
        }
    }

    fn fetch(voice: &mut Voice, rom: &[u8], random: &mut u16) {
        voice.previous = voice.sample;
        if voice.flags & NOISE != 0 {
            *random = (*random >> 1) ^ if *random & 1 != 0 { 0xfff6 } else { 0 };
            voice.sample = *random as i16;
            return;
        }
        let byte = rom
            .get((voice.position & 0xff_ffff) as usize)
            .copied()
            .unwrap_or(0);
        voice.sample = if voice.flags & MULAW != 0 {
            Self::decode_mulaw(byte)
        } else {
            i16::from(byte as i8) << 8
        };
        let position = voice.position as u16;
        if voice.flags & (LOOP | REVERSE) == (LOOP | REVERSE) {
            if voice.flags & LOOP_DIRECTION != 0 && position == voice.loop_start {
                voice.flags &= !LOOP_DIRECTION;
            } else if voice.flags & LOOP_DIRECTION == 0 && position == voice.end {
                voice.flags |= LOOP_DIRECTION;
            }
            voice.position = if voice.flags & LOOP_DIRECTION != 0 {
                voice.position.wrapping_sub(1)
            } else {
                voice.position.wrapping_add(1)
            };
        } else if position == voice.end {
            if voice.flags & (LINK | LOOP) == (LINK | LOOP) {
                voice.position = (u32::from(voice.start) << 16) | u32::from(voice.loop_start);
                voice.flags |= LOOP_HISTORY;
            } else if voice.flags & LOOP != 0 {
                voice.position = (voice.position & 0xff_0000) | u32::from(voice.loop_start);
                voice.flags |= LOOP_HISTORY;
            } else {
                voice.flags = (voice.flags | KEY_OFF) & !BUSY;
                voice.sample = 0;
            }
        } else if voice.flags & REVERSE != 0 {
            voice.position = voice.position.wrapping_sub(1);
        } else {
            voice.position = voice.position.wrapping_add(1);
        }
    }

    fn tick(&mut self) -> [i32; 4] {
        let mut output = [0_i32; 4];
        for voice in &mut self.voices {
            let mut sample = 0_i32;
            if voice.flags & BUSY != 0 {
                let next = voice.counter + u32::from(voice.frequency);
                if next & 0x10000 != 0 {
                    Self::fetch(voice, &self.rom, &mut self.random);
                }
                if (next ^ voice.counter) & 0x18000 != 0 {
                    let targets = [
                        (voice.volume_front >> 8) as u8,
                        voice.volume_front as u8,
                        (voice.volume_rear >> 8) as u8,
                        voice.volume_rear as u8,
                    ];
                    for (current, target) in voice.current_volume.iter_mut().zip(targets) {
                        if *current < target {
                            *current += 1;
                        } else if *current > target {
                            *current -= 1;
                        }
                    }
                }
                voice.counter = next & 0xffff;
                sample = i32::from(voice.sample);
                if voice.flags & FILTER_DISABLE == 0 {
                    let delta = i64::from(voice.sample) - i64::from(voice.previous);
                    sample = (i64::from(voice.previous)
                        + ((i64::from(voice.counter) * delta) >> 16))
                        as i16 as i32;
                }
            }
            for (index, out) in output.iter_mut().enumerate() {
                let phase = [PHASE_FL, PHASE_R, PHASE_RL, PHASE_R][index];
                let value = if voice.flags & phase != 0 {
                    -sample
                } else {
                    sample
                };
                *out += (value * i32::from(voice.current_volume[index])) >> 8;
            }
        }
        output.map(|sample| i32::from((sample >> 3) as i16))
    }
}

impl SoundChip for C352 {
    fn chip_type(&self) -> &Chip {
        &Chip::C352
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / self.divider
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(self.divider).unwrap())
    }
    fn channels(&self) -> usize {
        4
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.parts_for(self.chip_type())?;
        let value = u16::try_from(value).map_err(|_| ChipError::InvalidRegisterValue {
            chip: Chip::C352,
            register,
            value,
        })?;
        self.write_register(u16::from_be_bytes([bank, register]), value);
        Ok(())
    }
    fn write_rom(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        write_rom_data(&mut self.rom, start, data)
    }
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if !buffer.len().is_multiple_of(4) {
            return Err(ChipError::InvalidBufferLength {
                length: buffer.len(),
                channels: 4,
            });
        }
        for frame in buffer.as_chunks_mut::<4>().0 {
            *frame = self.tick();
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.voices = [Voice::default(); 32];
        self.random = 0x1234;
        self.control = 0;
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::C352)
    }
}

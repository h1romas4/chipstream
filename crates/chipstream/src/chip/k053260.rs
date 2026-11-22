//! Based on MAME's K053260 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Ernesto Corvi and Alex W. Jackson.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const PAN: [[i32; 2]; 8] = [
    [0, 0],
    [127, 0],
    [116, 52],
    [104, 73],
    [90, 90],
    [73, 104],
    [52, 116],
    [0, 127],
];
const ADPCM: [i8; 16] = [
    0, 1, 2, 4, 8, 16, 32, 64, -128, -64, -32, -16, -8, -4, -2, -1,
];

#[derive(Clone, Copy, Default)]
struct Voice {
    position: u32,
    counter: u16,
    output: i8,
    playing: bool,
    start: u32,
    length: u16,
    pitch: u16,
    volume: u8,
    pan: u8,
    looping: bool,
    adpcm: bool,
    reverse: bool,
}

impl Voice {
    fn write_register(&mut self, register: u8, value: u8) {
        match register & 7 {
            0 => self.pitch = (self.pitch & 0x0f00) | u16::from(value),
            1 => self.pitch = (self.pitch & 0x00ff) | (u16::from(value & 15) << 8),
            2 => self.length = (self.length & 0xff00) | u16::from(value),
            3 => self.length = (self.length & 0x00ff) | (u16::from(value) << 8),
            4 => self.start = (self.start & 0x1fff00) | u32::from(value),
            5 => self.start = (self.start & 0x1f00ff) | (u32::from(value) << 8),
            6 => self.start = (self.start & 0x00ffff) | (u32::from(value & 31) << 16),
            7 => self.volume = value & 0x7f,
            _ => unreachable!(),
        }
    }

    fn key_on(&mut self) {
        self.position = u32::from(self.adpcm);
        self.counter = 0x1000 - 64;
        self.output = 0;
        self.playing = true;
    }

    fn key_off(&mut self) {
        self.position = 0;
        self.output = 0;
        self.playing = false;
    }

    fn tick(&mut self, rom: &[u8]) -> [i32; 2] {
        self.counter += 64;
        while self.counter >= 0x1000 {
            self.counter = self.counter - 0x1000 + self.pitch;
            self.position += 1;
            let mut bytepos = self.position >> u32::from(self.adpcm);
            if bytepos > u32::from(self.length) {
                if self.looping {
                    self.position = 0;
                    self.output = 0;
                    bytepos = 0;
                } else {
                    self.playing = false;
                    return [0; 2];
                }
            }
            let address = if self.reverse {
                self.start.wrapping_sub(bytepos)
            } else {
                self.start.wrapping_add(bytepos)
            } & 0x1f_ffff;
            let mut data = rom.get(address as usize).copied().unwrap_or(0);
            if self.adpcm {
                if self.position & 1 != 0 {
                    data >>= 4;
                }
                self.output = self.output.wrapping_add(ADPCM[usize::from(data & 15)]);
            } else {
                self.output = data as i8;
            }
        }
        let pan = PAN[usize::from(self.pan)];
        pan.map(|gain| (i32::from(self.output) * i32::from(self.volume) * gain) >> 6)
    }
}

/// Konami K053260 four-voice PCM/KADPCM generator with stereo output.
///
/// Sample ROM is supplied through [`SoundChip::write_rom`]; unloaded ROM reads
/// return zero. The external YM2151 input, communication ports, and timer pins
/// are not exposed by this audio-only backend. As in MAME, reset clears voices
/// but preserves ROM, the output mode, and the previous key-on register.
#[derive(Clone)]
pub struct K053260 {
    clock: u32,
    voices: [Voice; 4],
    key_on: u8,
    mode: u8,
    rom: Vec<u8>,
}

impl K053260 {
    /// Create a K053260 with the specified input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 64 {
            return Err(ChipError::InvalidClock { clock, minimum: 64 });
        }
        Ok(Self {
            clock,
            voices: [Voice::default(); 4],
            key_on: 0,
            mode: 0,
            rom: Vec::new(),
        })
    }

    /// Return the playback status of a voice numbered 0 through 3.
    pub fn channel_active(&self, channel: usize) -> Option<bool> {
        self.voices.get(channel).map(|voice| voice.playing)
    }

    fn write_register(&mut self, register: u8, value: u8) {
        let register = register & 0x3f;
        if (8..=0x27).contains(&register) {
            self.voices[usize::from((register - 8) / 8)].write_register(register, value);
            return;
        }
        match register {
            0x28 => {
                let rising = value & !self.key_on;
                for (i, voice) in self.voices.iter_mut().enumerate() {
                    voice.reverse = value & (0x10 << i) != 0;
                    if rising & (1 << i) != 0 {
                        voice.key_on();
                    } else if value & (1 << i) == 0 {
                        voice.key_off();
                    }
                }
                self.key_on = value;
            }
            0x2a => {
                for (i, voice) in self.voices.iter_mut().enumerate() {
                    voice.looping = value & (1 << i) != 0;
                    voice.adpcm = value & (0x10 << i) != 0;
                }
            }
            0x2c | 0x2d => {
                let first = usize::from(register - 0x2c) * 2;
                self.voices[first].pan = value & 7;
                self.voices[first + 1].pan = (value >> 3) & 7;
            }
            0x2f => self.mode = value,
            _ => {}
        }
    }

    fn tick(&mut self) -> [i32; 2] {
        if self.mode & 2 == 0 {
            return [0; 2];
        }
        let mut result = [0; 2];
        for voice in &mut self.voices {
            if voice.playing {
                let sample = voice.tick(&self.rom);
                result[0] += sample[0];
                result[1] += sample[1];
            }
        }
        result.map(|sample| sample.clamp(-32768, 32767))
    }
}

impl SoundChip for K053260 {
    fn chip_type(&self) -> &Chip {
        &Chip::K053260
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 64
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(64).unwrap())
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
            *frame = self.tick();
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.voices = [Voice::default(); 4];
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::K053260)
    }
}

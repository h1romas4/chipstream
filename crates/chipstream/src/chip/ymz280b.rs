//! Based on MAME's YMZ280B implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Aaron Giles.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const INDEX_SCALE: [i32; 8] = [0xe6, 0xe6, 0xe6, 0xe6, 0x133, 0x199, 0x200, 0x266];

#[derive(Clone, Copy)]
struct Voice {
    playing: bool,
    key_on: bool,
    looping: bool,
    mode: u8,
    frequency: u16,
    level: u8,
    pan: u8,
    addresses: [u32; 4],
    position: u32,
    signal: i32,
    step: i32,
    loop_signal: i32,
    loop_step: i32,
    loop_count: u32,
    fraction: i32,
    previous: i16,
    current: i16,
    ramp: bool,
}

impl Default for Voice {
    fn default() -> Self {
        Self {
            playing: false,
            key_on: false,
            looping: false,
            mode: 0,
            frequency: 0,
            level: 0,
            pan: 0,
            addresses: [0; 4],
            position: 0,
            signal: 0,
            step: 0x7f,
            loop_signal: 0,
            loop_step: 0x7f,
            loop_count: 0,
            fraction: 512,
            previous: 0,
            current: 0,
            ramp: false,
        }
    }
}

impl Voice {
    fn start(&mut self) {
        self.playing = true;
        self.position = self.addresses[0];
        self.signal = 0;
        self.step = 0x7f;
        self.loop_signal = 0;
        self.loop_step = 0x7f;
        self.loop_count = 0;
        self.ramp = false;
    }

    fn frequency_step(&self) -> i32 {
        i32::from(self.frequency & if self.mode == 1 { 0xff } else { 0x1ff }) + 1
    }

    fn volumes(&self) -> [i32; 2] {
        let level = i32::from(self.level);
        let pan = i32::from(self.pan);
        if pan == 8 {
            [level, level]
        } else if pan < 8 {
            [level, if pan == 0 { 0 } else { level * (pan - 1) / 7 }]
        } else {
            [level * (15 - pan) / 7, level]
        }
    }

    fn next_sample(&mut self, rom: &[u8]) -> (i16, bool) {
        if !self.playing {
            if self.ramp {
                let value = i32::from(self.current);
                let next = if value < 0 {
                    -((-value * 15) >> 4)
                } else {
                    (value * 15) >> 4
                };
                self.ramp = next != 0;
                return (next as i16, false);
            }
            return (0, false);
        }
        let read = |address: u32| -> u8 {
            rom.get((address & 0xff_ffff) as usize)
                .copied()
                .unwrap_or(0)
        };
        let sample = match self.mode {
            1 => {
                let byte = read(self.position / 2);
                let nibble = if self.position & 1 == 0 {
                    byte >> 4
                } else {
                    byte & 15
                };
                let diff = i32::from((nibble & 7) * 2 + 1) * if nibble & 8 == 0 { 1 } else { -1 };
                self.signal = (self.signal + self.step * diff / 8).clamp(-32768, 32767);
                self.step =
                    ((self.step * INDEX_SCALE[usize::from(nibble & 7)]) >> 8).clamp(0x7f, 0x6000);
                self.position += 1;
                self.signal as i16
            }
            2 => {
                let value = i16::from(read(self.position / 2) as i8) * 256;
                self.position += 2;
                value
            }
            3 => {
                let address = self.position / 2;
                let value = i16::from_le_bytes([read(address), read(address + 1)]);
                self.position += 4;
                value
            }
            _ => return (0, false),
        };
        if self.looping {
            if self.mode == 1 && self.position == self.addresses[1] && self.loop_count == 0 {
                self.loop_signal = self.signal;
                self.loop_step = self.step;
            }
            if self.position >= self.addresses[2] && self.key_on {
                self.position = self.addresses[1];
                if self.mode == 1 {
                    self.signal = self.loop_signal;
                    self.step = self.loop_step;
                    self.loop_count = self.loop_count.wrapping_add(1);
                }
            }
        }
        let ended = self.position >= self.addresses[3];
        if ended {
            self.playing = false;
            self.ramp = true;
        }
        (sample, ended)
    }
}

/// Yamaha YMZ280B eight-voice PCM/ADPCM generator.
///
/// Sample memory is supplied via [`SoundChip::write_rom`]; unloaded addresses
/// read as zero. DSP bypass settings are accepted, but DSP effects and external
/// RAM writes are not emulated. End status is reported when the decoder reaches
/// the end, without emulating an IRQ pin.
/// The post-end decay is generated continuously across output blocks.
#[derive(Clone)]
pub struct Ymz280b {
    clock: u32,
    voices: [Voice; 8],
    rom: Vec<u8>,
    status: u8,
    irq_mask: u8,
    irq_enable: bool,
    key_on_enable: bool,
    external_enable: bool,
    external_high: u32,
    external_mid: u32,
    external_address: u32,
    external_latch: u8,
}

impl Ymz280b {
    /// Create a YMZ280B with the specified input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 192 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 192,
            });
        }
        Ok(Self {
            clock,
            voices: [Voice::default(); 8],
            rom: Vec::new(),
            status: 0,
            irq_mask: 0,
            irq_enable: false,
            key_on_enable: false,
            external_enable: false,
            external_high: 0,
            external_mid: 0,
            external_address: 0,
            external_latch: 0,
        })
    }

    fn rom_byte(&self, address: u32) -> u8 {
        self.rom
            .get((address & 0xff_ffff) as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Read and clear voice-end status bits.
    pub fn read_status(&mut self) -> u8 {
        let status = self.status;
        self.status = 0;
        status
    }

    /// Whether enabled and unmasked voice-end status is pending.
    pub fn irq_active(&self) -> bool {
        self.irq_enable && self.status & self.irq_mask != 0
    }

    /// Read the external ROM port and advance its address.
    pub fn read_external(&mut self) -> u8 {
        if !self.external_enable {
            return 0xff;
        }
        let result = self.external_latch;
        self.external_latch = self.rom_byte(self.external_address);
        self.external_address = (self.external_address + 1) & 0xff_ffff;
        result
    }

    fn write_register(&mut self, register: u8, value: u8) -> Result<(), ChipError> {
        if register < 0x80 {
            let voice = &mut self.voices[usize::from((register >> 2) & 7)];
            match register & 0xe3 {
                0 => voice.frequency = (voice.frequency & 0x100) | u16::from(value),
                1 => {
                    voice.frequency = (voice.frequency & 0xff) | (u16::from(value & 1) << 8);
                    voice.looping = value & 0x10 != 0;
                    let mode = (value >> 5) & 3;
                    let key_on = value & 0x80 != 0 && mode != 0;
                    if mode != 0 {
                        voice.mode = mode;
                    }
                    if !voice.key_on && key_on && self.key_on_enable {
                        voice.start();
                    } else if voice.key_on && !key_on {
                        voice.playing = false;
                        voice.ramp = false;
                    }
                    voice.key_on = key_on;
                }
                2 => voice.level = value,
                3 => voice.pan = value & 15,
                0x20..=0x23 | 0x40..=0x43 | 0x60..=0x63 => {
                    let index = usize::from(register & 3);
                    let shift = match register & 0x60 {
                        0x20 => 17,
                        0x40 => 9,
                        _ => 1,
                    };
                    voice.addresses[index] =
                        (voice.addresses[index] & !(0xff << shift)) | (u32::from(value) << shift);
                }
                _ => {}
            }
            return Ok(());
        }
        match register {
            0x80 if value != 0 && value & 0x88 != 0x88 => {
                return Err(ChipError::InvalidConfiguration {
                    chip: Chip::Ymz280b,
                    reason: "DSP registers are not supported",
                });
            }
            0x81 if value > 1 => {
                return Err(ChipError::InvalidConfiguration {
                    chip: Chip::Ymz280b,
                    reason: "DSP registers are not supported",
                });
            }
            0x82 if value != 0 => {
                return Err(ChipError::InvalidConfiguration {
                    chip: Chip::Ymz280b,
                    reason: "DSP registers are not supported",
                });
            }
            0x84 => self.external_high = u32::from(value) << 16,
            0x85 => self.external_mid = u32::from(value) << 8,
            0x86 => {
                self.external_address = self.external_high | self.external_mid | u32::from(value);
                if self.external_enable {
                    self.external_latch = self.rom_byte(self.external_address);
                }
            }
            0x87 if self.external_enable => {
                return Err(ChipError::InvalidConfiguration {
                    chip: Chip::Ymz280b,
                    reason: "external RAM writes are not supported",
                });
            }
            0xfe => self.irq_mask = value,
            0xff => {
                self.external_enable = value & 0x40 != 0;
                self.irq_enable = value & 0x10 != 0;
                let key_on_enable = value & 0x80 != 0;
                if self.key_on_enable && !key_on_enable {
                    for voice in &mut self.voices {
                        voice.playing = false;
                        voice.ramp = false;
                    }
                } else if !self.key_on_enable && key_on_enable {
                    for voice in &mut self.voices {
                        if voice.key_on && voice.looping {
                            voice.playing = true;
                            voice.ramp = false;
                        }
                    }
                }
                self.key_on_enable = key_on_enable;
            }
            _ => {}
        }
        Ok(())
    }

    fn tick(&mut self) -> [i32; 2] {
        let mut output = [0_i32; 2];
        for (index, voice) in self.voices.iter_mut().enumerate() {
            if !voice.playing && voice.current == 0 && voice.previous == 0 {
                voice.fraction = 512;
                continue;
            }
            if voice.fraction >= 512 {
                voice.fraction -= 512;
                voice.previous = voice.current;
                let (sample, ended) = voice.next_sample(&self.rom);
                voice.current = sample;
                if ended {
                    self.status |= 1 << index;
                }
            }
            let sample = (i32::from(voice.previous) * (512 - voice.fraction)
                + i32::from(voice.current) * voice.fraction)
                >> 9;
            let volumes = voice.volumes();
            for side in 0..2 {
                output[side] += sample * volumes[side] / 2;
            }
            voice.fraction += voice.frequency_step();
            if voice.fraction >= 512 {
                voice.fraction -= 512;
                voice.previous = voice.current;
                let (sample, ended) = voice.next_sample(&self.rom);
                voice.current = sample;
                if ended {
                    self.status |= 1 << index;
                }
            }
        }
        output.map(|value| value / 256)
    }
}

impl SoundChip for Ymz280b {
    fn chip_type(&self) -> &Chip {
        &Chip::Ymz280b
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 192
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(192).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (_, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(register, value)
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
        self.voices = [Voice::default(); 8];
        self.status = 0;
        self.irq_mask = 0;
        self.irq_enable = false;
        self.key_on_enable = false;
        self.external_enable = false;
        self.external_high = 0;
        self.external_mid = 0;
        self.external_address = 0;
        self.external_latch = 0;
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Ymz280b)
    }
}

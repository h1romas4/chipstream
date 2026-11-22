//! Based on MAME's MultiPCM and GEW implementations, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Miguel Angel Horna.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const BASE_TIMES: [f64; 64] = [
    0., 0., 0., 0., 6222.95, 4978.37, 4148.66, 3556.01, 3111.47, 2489.21, 2074.33, 1778., 1555.74,
    1244.63, 1037.19, 889.02, 777.87, 622.31, 518.59, 444.54, 388.93, 311.16, 259.32, 222.27,
    194.47, 155.60, 129.66, 111.16, 97.23, 77.82, 64.85, 55.60, 48.62, 38.91, 32.43, 27.80, 24.31,
    19.46, 16.24, 13.92, 12.15, 9.75, 8.12, 6.98, 6.08, 4.90, 4.08, 3.49, 3.04, 2.49, 2.13, 1.90,
    1.72, 1.41, 1.18, 1.04, 0.91, 0.73, 0.59, 0.50, 0.45, 0.45, 0.45, 0.45,
];
const LFO_FREQ: [f32; 8] = [0.168, 2.019, 3.196, 4.206, 5.215, 5.888, 6.224, 7.066];
const PITCH_LIMIT: [f32; 8] = [0., 3.378, 5.065, 6.750, 10.114, 20.170, 40.180, 79.307];
const AMP_LIMIT: [f32; 8] = [0., 0.4, 0.8, 1.5, 3., 6., 12., 24.];

#[derive(Clone, Copy, Default)]
struct Sample {
    start: u32,
    end: u32,
    loop_start: u32,
    format: u8,
}

#[derive(Clone, Copy, Default)]
enum EnvelopeState {
    #[default]
    Attack,
    Decay1,
    Decay2,
    Release,
}

#[derive(Clone, Copy, Default)]
struct Slot {
    registers: [u8; 11],
    playing: bool,
    sample: Sample,
    offset: u32,
    step: u32,
    previous: i32,
    level: u32,
    level_step: i32,
    envelope: i32,
    envelope_state: EnvelopeState,
    envelope_rates: [i32; 4],
    decay_level: i32,
    pitch_phase: u16,
    amp_phase: u16,
    lfo_step: u32,
}

/// Yamaha YMW258 MultiPCM 28-voice stereo sample generator.
///
/// Sample headers and PCM data are supplied with [`SoundChip::write_rom`].
/// ROM addresses outside the loaded data read as zero. The external DSP
/// interface is not emulated. Reset stops all voices while preserving registers,
/// ROM and board banking, as in MAME.
///
/// [`soundlog::chip::MultiPcmBankSpec`] controls the VGM board's external ROM
/// banking, not a physical chip register. Bit 0 selects the upper 512 KiB
/// window and bit 1 the lower; equal banks select a single 1 MiB window.
/// The legacy VGM write offsets `0x10`, `0x11`, and `0x12` also control
/// external Model 1 and Multi 32 ROM banking.
#[derive(Clone)]
pub struct MultiPcm {
    clock: u32,
    slots: [Slot; 28],
    selected_slot: Option<usize>,
    selected_register: u8,
    rom: Vec<u8>,
    banked: bool,
    split_banks: bool,
    bank_left: u16,
    bank_right: u16,
    attack: [i32; 64],
    decay: [i32; 64],
    pan: [[i32; 2]; 2048],
    envelope_table: [i32; 1024],
    pitch_table: [[i32; 256]; 8],
    amp_table: [[i32; 256]; 8],
}

impl MultiPcm {
    /// Create a MultiPCM using the input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 224 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 224,
            });
        }
        let mut attack = [0; 64];
        let mut decay = [0; 64];
        for i in 4..64 {
            attack[i] = ((0x400_u32 << 16) as f32 / (BASE_TIMES[i] * 44.1) as f32) as i32;
            decay[i] = ((0x400_u32 << 16) as f32 / (BASE_TIMES[i] * 14.32833 * 44.1) as f32) as i32;
        }
        attack[63] = 0x400 << 16;
        let pan = std::array::from_fn(|index| {
            let level = (index & 127) as f32;
            let p = index >> 7;
            let volume = 10_f32.powf((level * -24. / 64.) / 20.) / 4.;
            let attenuation = |amount: usize| {
                if amount == 7 {
                    0.
                } else {
                    10_f32.powf((amount as f32 * -12. / 4.) / 20.)
                }
            };
            let gains = if p == 8 {
                [0., 0.]
            } else if p == 0 {
                [1., 1.]
            } else if p & 8 != 0 {
                [1., attenuation(16 - p)]
            } else {
                [attenuation(p), 1.]
            };
            gains.map(|gain| (gain * volume * 4096.) as i32)
        });
        Ok(Self {
            clock,
            slots: [Slot::default(); 28],
            selected_slot: Some(0),
            selected_register: 0,
            rom: Vec::new(),
            banked: false,
            split_banks: false,
            bank_left: 0,
            bank_right: 0,
            attack,
            decay,
            pan,
            envelope_table: std::array::from_fn(|i| {
                let db = -(96.0_f32 - 96.0 * i as f32 / 1024.);
                (4096. * 10_f32.powf(db / 20.)) as i32
            }),
            pitch_table: std::array::from_fn(|depth| {
                std::array::from_fn(|i| {
                    (256. * 2_f32.powf(PITCH_LIMIT[depth] * (i as f32 - 128.) / 128. / 1200.))
                        as i32
                })
            }),
            amp_table: std::array::from_fn(|depth| {
                std::array::from_fn(|i| {
                    (256. * 10_f32.powf(-AMP_LIMIT[depth] * i as f32 / 256. / 20.)) as i32
                })
            }),
        })
    }

    fn read_rom(&self, address: u32) -> u8 {
        let mut address = address & 0x3f_ffff;
        if self.banked {
            address &= 0x1f_ffff;
            if address & 0x10_0000 != 0 {
                address = if !self.split_banks && self.bank_left == self.bank_right {
                    (u32::from(self.bank_right & !15) << 16) | (address & 0x0f_ffff)
                } else if address & 0x08_0000 != 0 {
                    (u32::from(self.bank_left & !7) << 16) | (address & 0x07_ffff)
                } else {
                    (u32::from(self.bank_right & !7) << 16) | (address & 0x07_ffff)
                };
            }
        }
        self.rom.get(address as usize).copied().unwrap_or(0)
    }

    fn update_step(&mut self, channel: usize) {
        let slot = &mut self.slots[channel];
        let pitch = u32::from(
            ((u16::from(slot.registers[3]) & 15) << 6) | u16::from(slot.registers[2] >> 2),
        );
        let octave = (slot.registers[3] >> 4).wrapping_sub(1) & 15;
        let rate = self.clock as f32 / 224.;
        let mut step = (4096. * (rate * (1024. + pitch as f32) / 1024.)) as u32;
        if octave & 8 != 0 {
            step >>= 16 - octave;
        } else {
            step = step.wrapping_shl(u32::from(octave));
        }
        slot.step = (step as f32 / rate) as u32;
    }

    fn update_envelope(&mut self, channel: usize) {
        let slot = &mut self.slots[channel];
        let octave = (slot.registers[3] >> 4) as i32;
        let octave = if octave & 8 != 0 { octave - 16 } else { octave };
        let pitch =
            ((u16::from(slot.registers[3] & 15) << 6) | u16::from(slot.registers[2] >> 2)) as i32;
        let scale = i32::from(slot.registers[9] >> 4);
        let adjustment = if scale == 15 {
            0
        } else {
            (octave + scale) * 2 + ((pitch >> 9) & 1)
        };
        let rate = |table: &[i32; 64], value: u8| {
            let index = if value == 0 {
                0
            } else if value == 15 {
                63
            } else {
                (4 * i32::from(value) + adjustment).clamp(0, 63) as usize
            };
            table[index]
        };
        slot.envelope_rates = [
            rate(&self.attack, slot.registers[7] >> 4),
            rate(&self.decay, slot.registers[7] & 15),
            rate(&self.decay, slot.registers[8] & 15),
            rate(&self.decay, slot.registers[9] & 15),
        ];
        slot.decay_level = 15 - i32::from(slot.registers[8] >> 4);
    }

    fn select_sample(&mut self, channel: usize) {
        let slot = &self.slots[channel];
        let index = u32::from(slot.registers[1]) | (u32::from(slot.registers[2] & 1) << 8);
        let base = index * 12;
        let header: [u8; 12] = std::array::from_fn(|i| self.read_rom(base + i as u32));
        let start =
            (u32::from(header[0]) << 16) | (u32::from(header[1]) << 8) | u32::from(header[2]);
        let slot = &mut self.slots[channel];
        slot.sample = Sample {
            start: start & 0x3f_ffff,
            format: ((start >> 20) & 0xfe) as u8,
            loop_start: u32::from(u16::from_be_bytes([header[3], header[4]])),
            end: 0x10000 - u32::from(u16::from_be_bytes([header[5], header[6]])),
        };
        slot.registers[6..11].copy_from_slice(&[
            header[7],
            header[8],
            header[9],
            header[10],
            header[11] & 15,
        ]);
        self.update_lfo(channel);
        self.update_envelope(channel);
        if self.slots[channel].playing {
            self.retrigger(channel);
        }
    }

    fn update_lfo(&mut self, channel: usize) {
        let slot = &mut self.slots[channel];
        let frequency = usize::from((slot.registers[6] >> 3) & 7);
        slot.lfo_step = (65536. * LFO_FREQ[frequency] / (self.clock as f32 / 224.)) as u32;
    }

    fn retrigger(&mut self, channel: usize) {
        self.update_envelope(channel);
        let slot = &mut self.slots[channel];
        slot.offset = 0;
        slot.previous = 0;
        slot.level = u32::from(slot.registers[5] >> 1) << 12;
        slot.envelope_state = EnvelopeState::Attack;
        slot.envelope = (0x3ff - 0x2a0) << 16;
    }

    fn write_slot(&mut self, channel: usize, register: u8, value: u8) {
        self.slots[channel].registers[usize::from(register)] = value;
        match register {
            1 => self.select_sample(channel),
            2 | 3 => self.update_step(channel),
            4 if value & 0x80 != 0 => {
                self.slots[channel].playing = true;
                self.retrigger(channel);
            }
            4 if self.slots[channel].playing => {
                if self.slots[channel].registers[9] & 15 == 15 {
                    self.slots[channel].playing = false;
                } else {
                    self.slots[channel].envelope_state = EnvelopeState::Release;
                }
            }
            5 => {
                let slot = &mut self.slots[channel];
                let dest = u32::from(value >> 1);
                if value & 1 != 0 {
                    slot.level = dest << 12;
                } else {
                    slot.level_step = if slot.level >> 12 > dest { -152 } else { 76 };
                }
            }
            6 | 10 if value != 0 => self.update_lfo(channel),
            7..=9 => self.update_envelope(channel),
            _ => {}
        }
    }

    fn write_register(&mut self, register: u8, value: u8) -> Result<(), ChipError> {
        match register {
            0 => {
                if let Some(channel) = self.selected_slot
                    && self.selected_register < 11
                {
                    self.write_slot(channel, self.selected_register, value);
                }
            }
            1 => {
                let n = usize::from(value & 31);
                self.selected_slot = (n % 8 != 7).then_some(n - n / 8);
            }
            2 => self.selected_register = value,
            0x0d => {
                return Err(ChipError::InvalidConfiguration {
                    chip: Chip::MultiPcm,
                    reason: "external DSP control is not supported",
                });
            }
            0x10 => {
                self.bank_left = u16::from(value) << 4;
                self.bank_right = self.bank_left;
                self.banked = true;
                self.split_banks = false;
            }
            0x11 | 0x12 => {
                if self.banked && !self.split_banks && self.bank_left == self.bank_right {
                    self.bank_right &= !15;
                    self.bank_left = self.bank_right + 8;
                }
                if register == 0x11 {
                    self.bank_right = u16::from(value) << 3;
                } else {
                    self.bank_left = u16::from(value) << 3;
                }
                self.banked = true;
                self.split_banks = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn envelope_tick(slot: &mut Slot, table: &[i32; 1024]) -> i32 {
        match slot.envelope_state {
            EnvelopeState::Attack => {
                slot.envelope += (((i64::from(0x817 << 15) - i64::from(slot.envelope))
                    * i64::from(slot.envelope_rates[0]))
                    >> 24) as i32;
                if slot.envelope >= 0x3ff << 16 {
                    slot.envelope = 0x3ff << 16;
                    slot.envelope_state = if slot.envelope_rates[1] >= 0x400 << 16 {
                        EnvelopeState::Decay2
                    } else {
                        EnvelopeState::Decay1
                    };
                }
            }
            EnvelopeState::Decay1 => {
                slot.envelope = (slot.envelope - slot.envelope_rates[1]).max(0);
                if slot.envelope >> 22 <= slot.decay_level {
                    slot.envelope_state = EnvelopeState::Decay2;
                }
            }
            EnvelopeState::Decay2 => {
                slot.envelope = (slot.envelope - slot.envelope_rates[2]).max(0)
            }
            EnvelopeState::Release => {
                slot.envelope = (slot.envelope - slot.envelope_rates[3]).max(0);
                if slot.envelope == 0 {
                    slot.playing = false;
                }
            }
        }
        table[(slot.envelope >> 16) as usize]
    }

    fn tick(&mut self) -> [i32; 2] {
        let mut output = [0; 2];
        for channel in 0..28 {
            let slot = self.slots[channel];
            if !slot.playing {
                continue;
            }
            let position = slot.offset >> 12;
            let address = slot.sample.start;
            let current = if slot.sample.format & 4 != 0 {
                let addr = address + (position >> 1) * 3;
                let hi = if position & 1 == 0 {
                    self.read_rom(addr)
                } else {
                    self.read_rom(addr + 2)
                };
                let low = self.read_rom(addr + 1);
                let lo = if position & 1 == 0 {
                    (low & 15) << 4
                } else {
                    low & 0xf0
                };
                i32::from(i16::from_be_bytes([hi, lo]))
            } else {
                i32::from(self.read_rom(address + position) as i8) << 8
            };
            let slot = &mut self.slots[channel];
            let fraction = (slot.offset & 4095) as i32;
            let mut sample = (current * fraction + slot.previous * (4096 - fraction)) >> 12;
            let mut step = slot.step;
            let vibrato = usize::from(slot.registers[6] & 7);
            if vibrato != 0 {
                slot.pitch_phase = slot.pitch_phase.wrapping_add(slot.lfo_step as u16);
                let phase = i32::from(slot.pitch_phase >> 8);
                let index = if phase < 64 {
                    phase * 2 + 128
                } else if phase < 128 {
                    383 - phase * 2
                } else if phase < 192 {
                    384 - phase * 2
                } else {
                    phase * 2 - 383
                };
                step = step.wrapping_mul((self.pitch_table[vibrato][index as usize] << 4) as u32)
                    >> 12;
            }
            slot.offset = slot.offset.wrapping_add(step);
            if position != slot.offset >> 12 {
                slot.previous = current;
            }
            if slot.offset >= slot.sample.end << 12 {
                slot.offset = slot
                    .offset
                    .wrapping_sub(slot.sample.end.wrapping_sub(slot.sample.loop_start) << 12);
            }
            let volume_index =
                ((slot.registers[0] as usize >> 4) << 7) | (slot.level as usize >> 12);
            if slot.level >> 12 != u32::from(slot.registers[5] >> 1) {
                slot.level = slot.level.wrapping_add_signed(slot.level_step);
            }
            let tremolo = usize::from(slot.registers[10] & 7);
            if tremolo != 0 {
                slot.amp_phase = slot.amp_phase.wrapping_add(slot.lfo_step as u16);
                let phase = usize::from(slot.amp_phase >> 8);
                let index = if phase < 128 {
                    255 - phase * 2
                } else {
                    phase * 2 - 256
                };
                sample = (sample * (self.amp_table[tremolo][index] << 4)) >> 12;
            }
            sample = (sample * Self::envelope_tick(slot, &self.envelope_table)) >> 10;
            for (side, out) in output.iter_mut().enumerate() {
                *out += (self.pan[volume_index][side] * sample) >> 12;
            }
        }
        output.map(|value| value.clamp(-32768, 32767))
    }
}

impl SoundChip for MultiPcm {
    fn chip_type(&self) -> &Chip {
        &Chip::MultiPcm
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 224
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(224).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        match write {
            ChipRegisterWrite::MultiPcm(spec) => self.write_register(spec.register, spec.value),
            ChipRegisterWrite::MultiPcmBank(spec) => {
                if spec.channel & !3 != 0 {
                    return Err(ChipError::InvalidRegister {
                        chip: Chip::MultiPcm,
                        register: spec.channel,
                    });
                }
                if spec.channel & 1 != 0 {
                    self.bank_left = spec.bank_offset;
                }
                if spec.channel & 2 != 0 {
                    self.bank_right = spec.bank_offset;
                }
                self.banked = true;
                self.split_banks = false;
                Ok(())
            }
            other => Err(ChipError::ChipMismatch {
                expected: other.chip(),
                actual: Chip::MultiPcm,
            }),
        }
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
        for slot in &mut self.slots {
            slot.playing = false;
        }
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::MultiPcm)
    }
}

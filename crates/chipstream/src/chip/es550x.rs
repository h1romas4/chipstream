//! ES5505/ES5506 synthesis based on MAME revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Aaron Giles. See `THIRD_PARTY_NOTICES.md`.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const STOP: u32 = 3;
const LOOP: u32 = 0x18;
const DIRECTION: u32 = 0x40;
const IRQ: u32 = 0x80;
const ADDRESS_FRAC: u32 = 11;
const REGION_SHIFT: usize = 28;
const REGION_SIZE: usize = 1 << REGION_SHIFT;
const MAX_REGION_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Default)]
struct Voice {
    control: u32,
    frequency: u64,
    start: u64,
    end: u64,
    position: u64,
    volume: [u32; 2],
    volume_ramp: [u8; 2],
    envelope_count: u32,
    filter: [u32; 2],
    filter_ramp: [u32; 2],
    filter_count: u8,
    poles: [i32; 6],
    external_bank: u64,
}

/// Ensoniq ES5505 or ES5506 sample synthesizer.
///
/// The VGM external voice-bank writes use offsets `0x40..=0x7f`.
/// ROM regions occupy separate 256 MiB windows in the `write_rom` address
/// space; sample words are little-endian. Unloaded sample addresses read zero.
/// The implementation has been tested with synthetic data but not verified
/// against a complete ES550x VGM recording.
#[derive(Clone)]
pub struct Es550x {
    chip: Chip,
    clock: u32,
    voices: [Voice; 32],
    page: u8,
    latch: u32,
    active: u8,
    rom: [Vec<u8>; 4],
}

impl Es550x {
    /// Create an ES5505 or ES5506 using its input clock.
    pub fn new(chip: Chip, clock: u32) -> Result<Self, ChipError> {
        if !matches!(chip, Chip::Es5505 | Chip::Es5506U8 | Chip::Es5506U16) {
            return Err(ChipError::UnsupportedChip(chip));
        }
        if clock < 512 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 512,
            });
        }
        let is_5505 = chip == Chip::Es5505;
        let voice = Voice {
            control: STOP,
            volume: [if is_5505 { 0x80 } else { 0x8000 }; 2],
            ..Voice::default()
        };
        Ok(Self {
            chip,
            clock,
            voices: [voice; 32],
            page: 0,
            latch: 0,
            active: 31,
            rom: std::array::from_fn(|_| Vec::new()),
        })
    }

    fn is_5505(&self) -> bool {
        self.chip == Chip::Es5505
    }

    fn write_byte(&mut self, offset: u8, value: u8) {
        if offset >= 0x40 {
            self.voices[usize::from(offset & 31)].external_bank = u64::from(value) << 20;
        } else if self.is_5505() {
            let shift = if offset & 1 == 0 { 8 } else { 0 };
            self.write_5505(offset / 2, u16::from(value) << shift, 0xff << shift);
        } else {
            let shift = 24 - 8 * u32::from(offset & 3);
            self.latch = (self.latch & !(0xff << shift)) | (u32::from(value) << shift);
            if offset & 3 == 3 {
                let value = self.latch;
                self.latch = 0;
                self.write_5506(offset / 4, value);
            }
        }
    }

    fn write_word(&mut self, offset: u8, value: u16) {
        if offset >= 0x40 {
            self.voices[usize::from(offset & 31)].external_bank = u64::from(value) << 20;
        } else {
            let [high, low] = value.to_be_bytes();
            self.write_byte(offset, high);
            self.write_byte(offset | 1, low);
        }
    }

    fn write_5506(&mut self, register: u8, value: u32) {
        if register == 15 {
            self.page = (value & 0x7f) as u8;
            return;
        }
        let voice = &mut self.voices[usize::from(self.page & 31)];
        match (self.page >> 5, register) {
            (0 | 1, 0) => voice.control = value & 0xffff,
            (0, 1) => voice.frequency = u64::from(value & 0x1ffff),
            (0, 2) => voice.volume[0] = value & 0xffff,
            (0, 3) => voice.volume_ramp[0] = (value >> 8) as u8,
            (0, 4) => voice.volume[1] = value & 0xffff,
            (0, 5) => voice.volume_ramp[1] = (value >> 8) as u8,
            (0, 6) => {
                voice.envelope_count = value & 0x1ff;
                voice.filter_count = 0;
            }
            (0, 7) => voice.filter[1] = value & 0xffff,
            (0, 8) => voice.filter_ramp[1] = ((value >> 8) & 0xff) | ((value & 1) << 31),
            (0, 9) => voice.filter[0] = value & 0xffff,
            (0, 10) => voice.filter_ramp[0] = ((value >> 8) & 0xff) | ((value & 1) << 31),
            (0, 11) => self.active = (value & 31) as u8,
            (1, 1) => voice.start = u64::from(value & 0xffff_f800),
            (1, 2) => voice.end = u64::from(value & 0xffff_ff80),
            (1, 3) => voice.position = u64::from(value),
            (1, 4..=9) => {
                let index = usize::from(register - 4);
                voice.poles[index] = ((value << 14) as i32) >> 14;
            }
            _ => {}
        }
    }

    fn write_5505(&mut self, register: u8, value: u16, mask: u16) {
        let update = |old: &mut u64, bits: u64, value: u64| {
            *old = (*old & !bits) | (value & bits);
        };
        if register == 15 {
            if mask & 0xff != 0 {
                self.page = value as u8 & 0x7f;
            }
            return;
        }
        if register == 13 {
            if mask & 0xff != 0 {
                self.active = value as u8 & 31;
            }
            return;
        }
        let voice = &mut self.voices[usize::from(self.page & 31)];
        let mask = u32::from(mask);
        let value = u32::from(value);
        match (self.page >> 5, register) {
            (0 | 1, 0) => {
                let mask = mask & 0x0fff;
                voice.control = 0xf000 | (voice.control & !mask) | (value & mask);
            }
            (0, 1) => update(
                &mut voice.frequency,
                u64::from(mask & 0xfffe) << 1,
                u64::from(value & 0xfffe) << 1,
            ),
            (0, 2 | 4 | 10) => {
                let target = match register {
                    2 => &mut voice.start,
                    4 => &mut voice.end,
                    _ => &mut voice.position,
                };
                update(
                    target,
                    u64::from(mask & 0x1fff) << 18,
                    u64::from(value) << 18,
                );
            }
            (0, 3 | 5 | 11) => {
                let target = match register {
                    3 => &mut voice.start,
                    5 => &mut voice.end,
                    _ => &mut voice.position,
                };
                let allowed = if register == 11 { 0xffff } else { 0xffe0 };
                update(
                    target,
                    u64::from(mask & allowed) << 2,
                    u64::from(value) << 2,
                );
            }
            (0, 6 | 7) => {
                let target = &mut voice.filter[usize::from(7 - register)];
                let mask = mask & 0xfff0;
                *target = (*target & !mask) | (value & mask);
            }
            (0, 8 | 9) if mask & 0xff00 != 0 => {
                voice.volume[usize::from(register - 8)] = value >> 8;
            }
            (1, 1..=6) => {
                let target = &mut voice.poles[usize::from(register - 1)];
                let bits = ((*target as u32) & !mask) | (value & mask);
                *target = i32::from(bits as i16);
            }
            _ => {}
        }
    }

    fn envelope(voice: &mut Voice, is_5505: bool) {
        if voice.envelope_count == 0 {
            return;
        }
        if is_5505 {
            voice.envelope_count = 0;
            return;
        }
        voice.envelope_count -= 1;
        for i in 0..2 {
            voice.volume[i] = (voice.volume[i] as i32 + i32::from(voice.volume_ramp[i] as i8))
                .clamp(0, 65535) as u32;
            let ramp = voice.filter_ramp[i];
            if ramp & 0x8000_0000 == 0 || voice.filter_count & 7 == 0 {
                voice.filter[i] =
                    (voice.filter[i] as i32 + i32::from(ramp as u8 as i8)).clamp(0, 65535) as u32;
            }
        }
        voice.filter_count = voice.filter_count.wrapping_add(1);
    }

    fn apply_filter(voice: &mut Voice, mut sample: i32, is_5505: bool) -> i32 {
        let low = |out: i32, cutoff: u32, previous: i32| {
            let value = i64::from(cutoff >> 4) * (i64::from(out) - i64::from(previous)) / 4096
                + i64::from(previous);
            value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        let high = |out: i32, cutoff: u32, previous: i32, delayed: i32| {
            let value = i64::from(out) - i64::from(delayed)
                + i64::from(cutoff >> 4) * i64::from(previous) / 8192
                + i64::from(previous / 2);
            value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
        };
        let [
            ref mut p4,
            ref mut p3,
            ref mut p3_old,
            ref mut p2,
            ref mut p2_old,
            ref mut p1,
        ] = voice.poles;
        sample = low(sample, voice.filter[0], *p1);
        *p1 = sample;
        sample = low(sample, voice.filter[0], *p2);
        *p2_old = *p2;
        *p2 = sample;
        let mode = (voice.control >> if is_5505 { 10 } else { 8 }) & 3;
        sample = match mode {
            0 => high(sample, voice.filter[1], *p3, *p2_old),
            2 => low(sample, voice.filter[1], *p3),
            _ => low(sample, voice.filter[0], *p3),
        };
        *p3_old = *p3;
        *p3 = sample;
        sample = if mode < 2 {
            high(sample, voice.filter[1], *p4, *p3_old)
        } else {
            low(sample, voice.filter[1], *p4)
        };
        *p4 = sample;
        sample
    }

    fn finish_voice(voice: &mut Voice, is_5505: bool, mask: u64) {
        let reverse = voice.control & DIRECTION != 0;
        if (!is_5505 && voice.control & 4 != 0)
            || if reverse {
                voice.position >= voice.start
            } else {
                voice.position <= voice.end
            }
        {
            return;
        }
        if voice.control & 0x20 != 0 {
            voice.control |= IRQ;
        }
        let excess = if reverse {
            voice.start.wrapping_sub(voice.position)
        } else {
            voice.position.wrapping_sub(voice.end)
        };
        voice.position = match voice.control & LOOP {
            0 | 0x10 if is_5505 => {
                voice.control |= 1;
                voice.position
            }
            0 => {
                voice.control |= 1;
                voice.position
            }
            8 | 0x10 => {
                if voice.control & LOOP == 0x10 {
                    voice.control = (voice.control & !LOOP) | 4;
                }
                if reverse {
                    voice.end.wrapping_sub(excess) & mask
                } else {
                    voice.start.wrapping_add(excess) & mask
                }
            }
            _ => {
                voice.control ^= DIRECTION;
                if reverse {
                    voice.start.wrapping_add(excess) & mask
                } else {
                    voice.end.wrapping_sub(excess) & mask
                }
            }
        };
    }

    fn tick(&mut self) -> [i32; 2] {
        let is_5505 = self.is_5505();
        let mask = if is_5505 {
            (1_u64 << 31) - 1
        } else {
            u64::from(u32::MAX)
        };
        let mut output = [0_i64; 2];
        for voice in &mut self.voices[..=usize::from(self.active)] {
            if !is_5505 && voice.start == voice.end {
                voice.control |= 1;
            }
            if voice.control & STOP != 0 {
                Self::envelope(voice, is_5505);
                continue;
            }
            let bank = if is_5505 {
                (voice.control >> 2) & 1
            } else {
                voice.control >> 14
            };
            let rom = &self.rom[bank as usize];
            let read = |address: u64| {
                let address = voice.external_bank + ((address & mask) >> ADDRESS_FRAC);
                let Some(offset) = usize::try_from(address)
                    .ok()
                    .and_then(|address| address.checked_mul(2))
                else {
                    return 0;
                };
                let a = rom.get(offset).copied().unwrap_or(0);
                let b = rom.get(offset + 1).copied().unwrap_or(0);
                let word = u16::from_le_bytes([a, b]);
                if !is_5505 && voice.control & 0x2000 != 0 {
                    let raw = (word & 0xff00) | 0x80;
                    let exponent = raw >> 13;
                    let mut mantissa = raw << 3;
                    if exponent != 0 {
                        mantissa = (mantissa >> 1) | (!mantissa & 0x8000);
                    }
                    i32::from((mantissa as i16) >> (7 - exponent)) << 1
                } else {
                    i32::from(word as i16) << u32::from(!is_5505)
                }
            };
            let fraction = (voice.position & 0x7ff) as i32;
            let sample = (read(voice.position) * (2048 - fraction)
                + read(voice.position.wrapping_add(2048)) * fraction)
                >> 11;
            let sample = Self::apply_filter(voice, sample, is_5505);
            Self::envelope(voice, is_5505);
            for (channel, out) in output.iter_mut().enumerate() {
                let volume = if is_5505 {
                    voice.volume[channel]
                } else {
                    voice.volume[channel] >> 4
                };
                let mantissa_bits = if is_5505 { 4 } else { 8 };
                let exponent = volume >> mantissa_bits;
                let mantissa = (volume & ((1 << mantissa_bits) - 1)) | (1 << mantissa_bits);
                let gain = (mantissa << (15 - mantissa_bits)) >> (16 - exponent);
                *out += (i64::from(sample >> u32::from(!is_5505)) * i64::from(gain)) >> 11;
            }
            voice.position = if voice.control & DIRECTION == 0 {
                voice.position.wrapping_add(voice.frequency)
            } else {
                voice.position.wrapping_sub(voice.frequency)
            } & mask;
            Self::finish_voice(voice, is_5505, mask);
        }
        output.map(|sample| (sample.clamp(-(1 << 19), (1 << 19) - 1) >> 4) as i32)
    }
}

impl SoundChip for Es550x {
    fn chip_type(&self) -> &Chip {
        &self.chip
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
            NonZeroU32::new(16 * (u32::from(self.active) + 1)).unwrap(),
        )
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        write.parts_for(&self.chip)?;
        match write {
            ChipRegisterWrite::Es5506U8(spec) => self.write_byte(spec.register, spec.value),
            ChipRegisterWrite::Es5506U16(spec) => self.write_word(spec.register, spec.value),
            _ => unreachable!(),
        }
        Ok(())
    }
    fn write_rom(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        let region = start >> REGION_SHIFT;
        let offset = start & (REGION_SIZE - 1);
        let end = offset
            .checked_add(data.len())
            .filter(|end| *end <= MAX_REGION_BYTES)
            .ok_or(ChipError::RomTooLarge)?;
        let rom = self.rom.get_mut(region).ok_or(ChipError::RomTooLarge)?;
        if rom.len() < end {
            rom.resize(end, 0);
        }
        rom[offset..end].copy_from_slice(data);
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
            *frame = self.tick();
        }
        Ok(())
    }
    fn reset(&mut self) {
        if !self.is_5505() {
            self.active = 31;
        }
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, self.chip.clone())
    }
}

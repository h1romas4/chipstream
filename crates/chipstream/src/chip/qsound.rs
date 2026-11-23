//! QSound HLE based on ValleyBell/qsound-hle revision
//! `68e63be325ddce6288adc3f571a4647f634046fb`.
//! Copyright (c) 2018 ValleyBell, Ian Karlsson.
//! See `THIRD_PARTY_NOTICES.md`.

use super::qsound_tables::{ADPCM_STEP, DRY, FILTER, FILTER_ALT, LINEAR, WET};
use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const INIT2: u16 = 0x61a;
const REFRESH1: u16 = 0x039;
const REFRESH2: u16 = 0x04f;
const NORMAL1: u16 = 0x314;
const NORMAL2: u16 = 0x6b2;
const DIVIDER: u32 = 2496;

#[derive(Clone, Copy, Default)]
struct Voice {
    bank: u16,
    address: i16,
    phase: u16,
    rate: u16,
    loop_len: i16,
    end: i16,
    volume: i16,
    echo: i16,
}

#[derive(Clone, Copy, Default)]
struct Adpcm {
    start: u16,
    end: u16,
    bank: u16,
    volume: i16,
    flag: u16,
    current_volume: i16,
    step: i16,
    address: u16,
}

#[derive(Clone)]
struct Fir {
    count: usize,
    pos: usize,
    table: u16,
    taps: [i16; 95],
    history: [i16; 95],
}

impl Default for Fir {
    fn default() -> Self {
        Self {
            count: 0,
            pos: 0,
            table: 0,
            taps: [0; 95],
            history: [0; 95],
        }
    }
}

impl Fir {
    fn refresh(&mut self, table: u16, count: usize) {
        self.pos = 0;
        self.count = count;
        let coefficients = if (0xf2e..0xfff).contains(&table) {
            FILTER_ALT.get(usize::from(table - 0xf2e)..)
        } else {
            let index = (i32::from(table) - 0xd53) / 95;
            if (0..5).contains(&index) {
                FILTER.get(index as usize * 95..)
            } else {
                None
            }
        };
        if let Some(coefficients) = coefficients.and_then(|v| v.get(..count)) {
            self.taps[..count].copy_from_slice(coefficients);
        }
    }

    fn process(&mut self, input: i16) -> i32 {
        let mut output = 0_i32;
        for tap in 0..self.count - 1 {
            let product = i32::from(self.taps[tap]) * i32::from(self.history[self.pos]);
            output = output.wrapping_sub(product.wrapping_shl(2));
            self.pos = (self.pos + 1) % (self.count - 1);
        }
        output = output.wrapping_sub(
            (i32::from(self.taps[self.count - 1]) * i32::from(input)).wrapping_shl(2),
        );
        self.history[self.pos] = input;
        self.pos = (self.pos + 1) % (self.count - 1);
        output
    }
}

#[derive(Clone)]
struct Delay {
    delay: i16,
    volume: i16,
    write: usize,
    read: usize,
    history: [i16; 51],
}

impl Default for Delay {
    fn default() -> Self {
        Self {
            delay: 0,
            volume: 0,
            write: 0,
            read: 0,
            history: [0; 51],
        }
    }
}

impl Delay {
    fn process(&mut self, input: i32) -> i32 {
        self.history[self.write] = (input >> 16) as i16;
        self.write = (self.write + 1) % 51;
        let output = i32::from(self.history[self.read]) * i32::from(self.volume);
        self.read = (self.read + 1) % 51;
        output
    }

    fn update(&mut self) {
        self.read = (self.write as i32 - i32::from(self.delay)).rem_euclid(51) as usize;
    }
}

#[derive(Clone)]
struct Echo {
    end: u16,
    feedback: i16,
    last: i16,
    pos: usize,
    history: [i16; 1024],
}

impl Default for Echo {
    fn default() -> Self {
        Self {
            end: 0,
            feedback: 0,
            last: 0,
            pos: 0,
            history: [0; 1024],
        }
    }
}

impl Echo {
    fn process(&mut self, input: i32, length: usize) -> i16 {
        let current = self.history[self.pos];
        let old = (i32::from(current) + i32::from(self.last)) >> 1;
        self.last = current;
        let value = input.wrapping_add((old * i32::from(self.feedback)).wrapping_shl(2));
        self.history[self.pos] = (value >> 16) as i16;
        self.pos += 1;
        if self.pos >= length {
            self.pos = 0;
        }
        old as i16
    }
}

/// Capcom QSound HLE with 16 PCM voices, three ADPCM voices, filters and echo.
///
/// The HLE does not require a DSP ROM.
/// The clock is the DSP clock in hertz. VGM's legacy 4 MHz clock is converted
/// by the sequencer; direct callers should pass the actual 60 MHz DSP clock.
/// Sample ROM data must be provided separately via [`SoundChip::write_rom`].
/// Entering mode 2 requires its initialization state before its normal state.
#[derive(Clone)]
pub struct Qsound {
    clock: u32,
    rom: Vec<u8>,
    voices: [Voice; 16],
    adpcm: [Adpcm; 3],
    pan: [u16; 19],
    output: [i16; 19],
    filter: [Fir; 2],
    alt_filter: [Fir; 2],
    wet: [Delay; 2],
    dry: [Delay; 2],
    echo: Echo,
    state: u16,
    next_state: u16,
    counter: usize,
    delay_update: u16,
    ready: bool,
    out: [i16; 2],
    legacy: bool,
    start_cache: [u16; 16],
    pitch_cache: [u16; 16],
}

impl Qsound {
    /// Create a QSound chip using the DSP input clock.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < DIVIDER {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: DIVIDER,
            });
        }
        Ok(Self {
            clock,
            rom: Vec::new(),
            voices: [Voice::default(); 16],
            adpcm: [Adpcm::default(); 3],
            pan: [0; 19],
            output: [0; 19],
            filter: std::array::from_fn(|_| Fir::default()),
            alt_filter: std::array::from_fn(|_| Fir::default()),
            wet: std::array::from_fn(|_| Delay::default()),
            dry: std::array::from_fn(|_| Delay::default()),
            echo: Echo::default(),
            state: 0,
            next_state: 0,
            counter: 0,
            delay_update: 0,
            ready: false,
            out: [0; 2],
            legacy: false,
            start_cache: [0; 16],
            pitch_cache: [0; 16],
        })
    }

    pub(crate) fn for_vgm(clock: u32) -> Result<Self, ChipError> {
        let legacy = clock < 5_000_000;
        let mut chip = Self::new(if legacy { clock * 15 } else { clock })?;
        chip.legacy = legacy;
        Ok(chip)
    }

    fn sample(rom: &[u8], bank: u16, address: u16) -> i16 {
        if bank & 0x8000 == 0 || rom.is_empty() {
            return 0;
        }
        let mask = rom.len().next_power_of_two() - 1;
        let address = ((usize::from(bank & 0x7fff) << 16) | usize::from(address)) & mask;
        i16::from(rom.get(address).copied().unwrap_or(0) as i8) << 8
    }

    fn write_register(&mut self, register: u8, value: u16) -> Result<(), ChipError> {
        if !self.ready {
            for _ in 0..32 {
                if matches!(self.state, NORMAL1 | NORMAL2)
                    && matches!(self.next_state, NORMAL1 | NORMAL2)
                {
                    break;
                }
                self.tick();
            }
            if !matches!(self.state, NORMAL1 | NORMAL2)
                || !matches!(self.next_state, NORMAL1 | NORMAL2)
            {
                return Err(ChipError::InvalidConfiguration {
                    chip: Chip::Qsound,
                    reason: "QSound DSP state did not become ready",
                });
            }
        }
        let signed = value as i16;
        if self.legacy && register < 0x80 {
            let channel = usize::from(register >> 3);
            match register & 7 {
                1 => self.start_cache[channel] = value,
                2 => {
                    if self.pitch_cache[channel] == 0 && value != 0 {
                        self.voices[channel].address = self.start_cache[channel] as i16;
                    }
                    self.pitch_cache[channel] = value;
                }
                3 => self.voices[channel].address = self.start_cache[channel] as i16,
                _ => {}
            }
        }
        match register {
            0x00..=0x7f => {
                let channel = usize::from(register >> 3);
                let voice = &mut self.voices[channel];
                match register & 7 {
                    0 => self.voices[(channel + 1) % 16].bank = value,
                    1 => voice.address = signed,
                    2 => voice.rate = value,
                    3 => voice.phase = value,
                    4 => voice.loop_len = signed,
                    5 => voice.end = signed,
                    6 => voice.volume = signed,
                    _ => {}
                }
            }
            0x80..=0x92 => self.pan[usize::from(register - 0x80)] = value,
            0x93 => self.echo.feedback = signed,
            0xba..=0xc9 => self.voices[usize::from(register - 0xba)].echo = signed,
            0xca..=0xd5 => {
                let index = usize::from((register - 0xca) / 4);
                let voice = &mut self.adpcm[index];
                match (register - 0xca) % 4 {
                    0 => voice.start = value,
                    1 => voice.end = value,
                    2 => voice.bank = value,
                    _ => voice.volume = signed,
                }
            }
            0xd6..=0xd8 => self.adpcm[usize::from(register - 0xd6)].flag = value,
            0xd9 => self.echo.end = value,
            0xda | 0xdc => self.filter[usize::from((register - 0xda) / 2)].table = value,
            0xdb | 0xdd => self.alt_filter[usize::from((register - 0xdb) / 2)].table = value,
            0xde | 0xe0 => self.wet[usize::from((register - 0xde) / 2)].delay = signed,
            0xdf | 0xe1 => self.dry[usize::from((register - 0xdf) / 2)].delay = signed,
            0xe2 => self.delay_update = value,
            0xe3 => {
                if value == NORMAL2 && self.alt_filter.iter().any(|filter| filter.count == 0) {
                    return Err(ChipError::InvalidConfiguration {
                        chip: Chip::Qsound,
                        reason: "QSound mode 2 filters have not been initialized",
                    });
                }
                self.next_state = value;
            }
            0xe4 | 0xe6 => self.wet[usize::from((register - 0xe4) / 2)].volume = signed,
            0xe5 | 0xe7 => self.dry[usize::from((register - 0xe5) / 2)].volume = signed,
            _ => {}
        }

        self.ready = false;
        Ok(())
    }

    fn init(&mut self) {
        if self.counter >= 2 {
            self.counter = 0;
            self.state = self.next_state;
            return;
        }
        if self.counter == 1 {
            self.counter += 1;
            return;
        }
        self.voices = [Voice::default(); 16];
        self.adpcm = [Adpcm::default(); 3];
        self.filter = std::array::from_fn(|_| Fir::default());
        self.alt_filter = std::array::from_fn(|_| Fir::default());
        self.wet = std::array::from_fn(|_| Delay::default());
        self.dry = std::array::from_fn(|_| Delay::default());
        self.echo = Echo::default();
        self.pan = [0x120; 19];
        self.output = [0; 19];
        for voice in &mut self.voices {
            voice.bank = 0x8000;
        }
        for voice in &mut self.adpcm {
            voice.bank = 0x8000;
        }
        if self.state == INIT2 {
            self.wet[0].delay = 1;
            self.filter[0].table = 0xf73;
            self.filter[1].table = 0xfa4;
            self.alt_filter[0].table = 0xf73;
            self.alt_filter[1].table = 0xfa4;
            self.echo.end = 0x53c + 6;
            self.next_state = REFRESH2;
        } else {
            self.dry[0].delay = 46;
            self.dry[1].delay = 48;
            self.filter[0].table = 0xdb2;
            self.filter[1].table = 0xe11;
            self.echo.end = 0x554 + 6;
            self.next_state = REFRESH1;
        }
        for delay in self.wet.iter_mut().chain(&mut self.dry) {
            delay.volume = 0x3fff;
        }
        self.delay_update = 1;
        self.ready = false;
        self.counter = 1;
    }

    fn tick(&mut self) -> [i16; 2] {
        match self.state {
            REFRESH1 | REFRESH2 => {
                let mode2 = self.state == REFRESH2;
                for ch in 0..2 {
                    let table = self.filter[ch].table;
                    self.filter[ch].refresh(table, if mode2 { 45 } else { 95 });
                    if mode2 {
                        let alt_table = self.alt_filter[ch].table;
                        self.alt_filter[ch].refresh(alt_table, 44);
                    }
                }
                self.state = if mode2 { NORMAL2 } else { NORMAL1 };
                self.next_state = self.state;
            }
            NORMAL1 | NORMAL2 => self.process(),
            _ => self.init(),
        }
        self.out
    }

    fn process(&mut self) {
        self.ready = true;
        let mut echo_input = 0_i32;
        for (index, voice) in self.voices.iter_mut().enumerate() {
            let sample = Self::sample(&self.rom, voice.bank, voice.address as u16);
            let output = ((i32::from(voice.volume) * i32::from(sample)) >> 14) as i16;
            echo_input = echo_input
                .wrapping_add((i32::from(output) * i32::from(voice.echo)).wrapping_shl(2));
            let mut phase = i32::from(voice.rate)
                + ((i32::from(voice.address) << 12) | i32::from(voice.phase >> 4));
            if phase >> 12 >= i32::from(voice.end) {
                phase -= i32::from(voice.loop_len) << 12;
            }
            phase = phase.clamp(-0x8000000, 0x7ffffff);
            voice.address = (phase >> 12) as i16;
            voice.phase = (phase << 4) as u16;
            self.output[index] = output;
        }
        let index = self.counter % 3;
        let low = self.counter / 3 != 0;
        let voice = &mut self.adpcm[index];
        if !low {
            if voice.address == voice.end {
                voice.current_volume = 0;
            }
            if voice.flag != 0 {
                self.output[16 + index] = 0;
                voice.flag = 0;
                voice.step = 10;
                voice.current_volume = voice.volume;
                voice.address = voice.start;
            }
        }
        let byte = (Self::sample(&self.rom, voice.bank, voice.address) >> 8) as i8;
        let nibble = if low {
            voice.address = voice.address.wrapping_add(1);
            (byte << 4) >> 4
        } else {
            byte >> 4
        };
        let step = i32::from(nibble);
        let mut delta = ((1 + (step * 2).abs()) * i32::from(voice.step)) >> 1;
        if step <= 0 {
            delta = -delta;
        }
        delta = (delta + i32::from(self.output[16 + index])).clamp(-32768, 32767);
        self.output[16 + index] = ((delta * i32::from(voice.current_volume)) >> 16) as i16;
        voice.step = ((i32::from(ADPCM_STEP[(8 + step) as usize]) * i32::from(voice.step)) >> 6)
            .clamp(1, 2000) as i16;
        let base = if self.state == NORMAL2 { 0x53c } else { 0x554 };
        let length = (i32::from(self.echo.end) - base).clamp(0, 1024) as usize;
        let echo = self.echo.process(echo_input, length);
        for ch in 0..2 {
            let mut wet = if ch == 1 { i32::from(echo) << 14 } else { 0 };
            let mut dry = if ch == 0 { i32::from(echo) << 14 } else { 0 };
            for index in 0..19 {
                let pan = self.pan[index].wrapping_sub(0x110).min(97) as usize;
                let table_index = if ch == 0 {
                    pan
                } else {
                    if pan <= 32 {
                        32 - pan
                    } else if (48..=80).contains(&pan) {
                        128 - pan
                    } else {
                        pan
                    }
                };
                let (d, w) = if table_index <= 32 {
                    (DRY[table_index], WET[table_index])
                } else if (48..=80).contains(&table_index) {
                    (LINEAR[table_index - 48], 0)
                } else {
                    (0, 0)
                };
                dry = dry.wrapping_sub(i32::from(self.output[index]) * i32::from(d));
                wet = wet.wrapping_sub(i32::from(self.output[index]) * i32::from(w));
            }
            dry = dry.clamp(-0x1fffffff, 0x1fffffff) << 2;
            wet = wet.clamp(-0x1fffffff, 0x1fffffff) << 2;
            wet = self.filter[ch].process((wet >> 16) as i16);
            if self.state == NORMAL2 {
                dry = self.alt_filter[ch].process((dry >> 16) as i16);
            }
            let output = self.wet[ch]
                .process(wet)
                .wrapping_add(self.dry[ch].process(dry))
                .wrapping_add(0x2000)
                >> 14;
            self.out[ch] = output.clamp(-0x7fff, 0x7fff) as i16;
            if self.delay_update != 0 {
                self.wet[ch].update();
                self.dry[ch].update();
            }
        }
        self.delay_update = 0;
        self.counter += 1;
        if self.counter > 5 {
            self.counter = 0;
            self.state = self.next_state;
        }
    }
}

impl SoundChip for Qsound {
    fn chip_type(&self) -> &Chip {
        &Chip::Qsound
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / DIVIDER
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(DIVIDER).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (_, register, value) = write.parts_for(self.chip_type())?;
        self.write_register(register, value as u16)
    }
    fn write_rom(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        const MAX_ROM: usize = 64 * 1024 * 1024;
        let end = start
            .checked_add(data.len())
            .filter(|&v| v <= MAX_ROM)
            .ok_or(ChipError::RomTooLarge)?;
        if end > self.rom.len() {
            self.rom.resize(end, 0xff);
        }
        self.rom[start..end].copy_from_slice(data);
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
            *frame = self.tick().map(i32::from);
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.ready = false;
        self.out = [0; 2];
        self.state = 0;
        self.counter = 0;
        self.next_state = 0;
        self.start_cache = [0; 16];
        self.pitch_cache = [0; 16];
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(Box::new(self.clone()))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        let state = state
            .as_any()
            .downcast_ref::<Self>()
            .ok_or(ChipError::InvalidStateSnapshot(Chip::Qsound))?;
        *self = state.clone();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_pitch_and_phase_restore_the_cached_start_address() {
        let mut chip = Qsound::for_vgm(4_000_000).unwrap();
        chip.write_register(0x01, 123).unwrap();
        chip.voices[0].address = 99;
        chip.write_register(0x02, 0x1000).unwrap();
        assert_eq!(chip.voices[0].address, 123);
        chip.voices[0].address = 77;
        chip.write_register(0x02, 0x2000).unwrap();
        assert_eq!(chip.voices[0].address, 77);
        chip.write_register(0x03, 0).unwrap();
        assert_eq!(chip.voices[0].address, 123);
        chip.write_register(0x02, 0).unwrap();
        chip.voices[0].address = 12;
        chip.write_register(0x02, 1).unwrap();
        assert_eq!(chip.voices[0].address, 123);
    }

    #[test]
    fn rejects_mode2_without_initializing_its_filters() {
        let mut chip = Qsound::new(60_000_000).unwrap();
        assert!(matches!(
            chip.write_register(0xe3, NORMAL2),
            Err(ChipError::InvalidConfiguration { .. })
        ));
    }
}

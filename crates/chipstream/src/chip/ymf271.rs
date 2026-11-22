//! YMF271 OPX synthesis based on MAME revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) superctr.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;

const GROUPS: [i8; 16] = [0, 1, 2, -1, 3, 4, 5, -1, 6, 7, 8, -1, 9, 10, 11, -1];
const MOD_LEVEL: [i32; 8] = [128, 64, 32, 16, 8, 256, 512, 1024];
const PMS: [i64; 8] = [0, 2, 3, 4, 6, 12, 24, 48];
const EG_INCREMENT: [u32; 64] = [
    0, 0, 0x10101010, 0x10101010, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010,
    0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010,
    0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010,
    0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010,
    0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x10101010,
    0x10111010, 0x11101110, 0x11111110, 0x10101010, 0x10111010, 0x11101110, 0x11111110, 0x11111111,
    0x21112111, 0x21212121, 0x22212221, 0x22222222, 0x42224222, 0x42424242, 0x44424442, 0x44444444,
    0x84448444, 0x84848484, 0x88848884, 0x88888888, 0x88888888, 0x88888888, 0x88888888,
];
const DETUNE: [[i32; 4]; 32] = [
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 0, 1, 2],
    [0, 1, 2, 2],
    [0, 1, 2, 3],
    [0, 1, 2, 3],
    [0, 1, 2, 3],
    [0, 1, 2, 4],
    [0, 1, 3, 4],
    [0, 1, 3, 4],
    [0, 1, 3, 5],
    [0, 2, 4, 5],
    [0, 2, 4, 6],
    [0, 2, 4, 6],
    [0, 2, 5, 7],
    [0, 2, 5, 8],
    [0, 3, 6, 8],
    [0, 3, 6, 9],
    [0, 3, 7, 10],
    [0, 4, 8, 11],
    [0, 4, 8, 12],
    [0, 4, 9, 13],
    [0, 5, 10, 14],
    [0, 5, 11, 16],
    [0, 6, 12, 17],
    [0, 6, 13, 19],
    [0, 7, 14, 20],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
    [0, 8, 16, 22],
];

#[derive(Clone, Copy)]
struct Algorithm {
    mods: [u8; 4],
    carriers: u8,
    feedback: u8,
}

const fn alg(mods: [u8; 4], carriers: u8, feedback: u8) -> Algorithm {
    Algorithm {
        mods,
        carriers,
        feedback,
    }
}

const ALG4: [Algorithm; 16] = [
    alg([0, 4, 1, 2], 8, 0),
    alg([0, 4, 1, 2], 8, 2),
    alg([0, 5, 0, 2], 8, 0),
    alg([0, 4, 0, 3], 8, 0),
    alg([0, 0, 1, 6], 8, 0),
    alg([0, 0, 1, 6], 8, 2),
    alg([0, 0, 1, 2], 12, 0),
    alg([0, 0, 1, 2], 12, 2),
    alg([0, 4, 0, 2], 9, 0),
    alg([0, 0, 0, 6], 9, 0),
    alg([0, 0, 1, 0], 14, 0),
    alg([0, 0, 1, 0], 14, 2),
    alg([0, 1, 1, 1], 14, 0),
    alg([0, 4, 0, 0], 11, 0),
    alg([0, 0, 1, 2], 13, 0),
    alg([0, 0, 0, 0], 15, 0),
];
const ALG2: [Algorithm; 4] = [
    alg([0, 1, 0, 0], 2, 0),
    alg([0, 1, 0, 0], 2, 1),
    alg([0, 0, 0, 0], 3, 0),
    alg([0, 1, 0, 0], 3, 0),
];
const ALG3: [Algorithm; 8] = [
    alg([0, 4, 1, 0], 2, 0),
    alg([0, 4, 1, 0], 2, 2),
    alg([0, 5, 0, 0], 2, 0),
    alg([0, 4, 0, 0], 3, 0),
    alg([0, 0, 1, 0], 6, 0),
    alg([0, 0, 1, 0], 6, 2),
    alg([0, 0, 0, 0], 7, 0),
    alg([0, 0, 1, 0], 7, 0),
];
const SINGLE: Algorithm = alg([0; 4], 1, 0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Envelope {
    Attack,
    Decay1,
    Decay2,
    Release,
    Off,
}

#[derive(Clone, Copy)]
struct Slot {
    lfo_freq: u8,
    ams: u8,
    pms: u8,
    lfo_wave: u8,
    detune: u8,
    multiple: u8,
    level: u8,
    key_scale: u8,
    attack: u8,
    decay: u8,
    sustain_rate: u8,
    sustain: u8,
    release: u8,
    fnum: u16,
    block: i8,
    fnum_latch: u8,
    accumulate: bool,
    feedback: u8,
    wave: u8,
    algorithm: u8,
    pan: [u8; 4],
    external: u8,
    pcm_start: u32,
    pcm_end: u32,
    pcm_loop: u32,
    pcm_rate: u8,
    pcm_12bit: bool,
    pcm_srcnote: u8,
    pcm_srcblock: u8,
    keycode: u8,
    envelope: Envelope,
    attenuation: i32,
    phase: u32,
    output: i32,
    sum: i32,
    history: [i32; 2],
    lfo_counter: u32,
    lfo_position: u8,
    pcm_position: u32,
    pcm_fraction: u32,
    pcm_ended: bool,
    modulators: [u8; 3],
    mod_count: u8,
    feedback_head: bool,
    feedback_target: Option<u8>,
    carrier: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            lfo_freq: 0,
            ams: 0,
            pms: 0,
            lfo_wave: 0,
            detune: 0,
            multiple: 0,
            level: 0,
            key_scale: 0,
            attack: 0,
            decay: 0,
            sustain_rate: 0,
            sustain: 0,
            release: 0,
            fnum: 0,
            block: 0,
            fnum_latch: 0,
            accumulate: false,
            feedback: 0,
            wave: 0,
            algorithm: 0,
            pan: [0; 4],
            external: 0,
            pcm_start: 0,
            pcm_end: 0,
            pcm_loop: 0,
            pcm_rate: 0,
            pcm_12bit: false,
            pcm_srcnote: 0,
            pcm_srcblock: 0,
            keycode: 0,
            envelope: Envelope::Off,
            attenuation: 1023,
            phase: 0,
            output: 0,
            sum: 0,
            history: [0; 2],
            lfo_counter: 0,
            lfo_position: 0,
            pcm_position: 0,
            pcm_fraction: 0,
            pcm_ended: false,
            modulators: [0; 3],
            mod_count: 0,
            feedback_head: false,
            feedback_target: None,
            carrier: false,
        }
    }
}

#[derive(Clone, Copy)]
struct Group {
    sync: u8,
    dirty: bool,
}

impl Default for Group {
    fn default() -> Self {
        Self {
            sync: 0,
            dirty: true,
        }
    }
}

/// Yamaha YMF271 OPX with 48 FM/PCM slots and eight native outputs.
///
/// The waveform and key-code approximations follow MAME. PFM requests are
/// accepted but ignored, as in MAME; alternate PCM looping is rejected.
/// Sample memory is supplied with [`SoundChip::write_rom`]; unloaded
/// addresses read as `0xff`.
#[derive(Clone)]
pub struct Ymf271 {
    clock: u32,
    slots: [Slot; 48],
    groups: [Group; 12],
    rom: Vec<u8>,
    logsin: [u16; 256],
    exponential: [u16; 256],
    eg_counter: u32,
    eg_phase: bool,
    end_status: u16,
    external_address: u32,
    external_read: bool,
    external_latch: u8,
    timer_a: u16,
    timer_b: u8,
    timer_control: u8,
    timer_a_remaining: Option<u32>,
    timer_b_remaining: Option<u32>,
    status: u8,
}

impl Ymf271 {
    /// Create a YMF271 using its master clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 384 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 384,
            });
        }

        let logsin = std::array::from_fn(|i| {
            let angle = (i as f64 + 0.5) * std::f64::consts::PI / 512.0;
            (-angle.sin().log2() * 256.0 + 0.5).floor() as u16
        });
        let exponential = std::array::from_fn(|i| {
            (2f64.powf(-((i + 1) as f64) / 256.0) * 2048.0 + 0.5).floor() as u16
        });
        Ok(Self {
            clock,
            slots: [Slot::default(); 48],
            groups: [Group::default(); 12],
            rom: Vec::new(),
            logsin,
            exponential,
            eg_counter: 0,
            eg_phase: false,
            end_status: 0,
            external_address: 0,
            external_read: false,
            external_latch: 0,
            timer_a: 0,
            timer_b: 0,
            timer_control: 0,
            timer_a_remaining: None,
            timer_b_remaining: None,
            status: 0,
        })
    }

    /// Read timer flags and PCM end flags for slots 0, 12, 24 and 36.
    /// The reported PCM end flags are cleared.
    pub fn read_status(&mut self) -> u8 {
        let end = self.end_status;
        self.end_status &= !0x0249;
        self.status & 3
            | ((end & 1) << 3) as u8
            | (((end >> 3) & 1) << 4) as u8
            | (((end >> 6) & 1) << 5) as u8
            | (((end >> 9) & 1) << 6) as u8
    }

    /// Read and clear the remaining eight PCM end flags.
    pub fn read_end_status(&mut self) -> u8 {
        let end = self.end_status;
        self.end_status &= !0x0db6;
        let mut result = 0;
        for (out, bit) in [1, 4, 7, 10, 2, 5, 8, 11].into_iter().enumerate() {
            result |= (((end >> bit) & 1) as u8) << out;
        }
        result
    }

    /// Read the external memory window and advance its address.
    /// Returns `0xff` when the window is configured for writing.
    pub fn read_external(&mut self) -> u8 {
        if !self.external_read {
            return 0xff;
        }
        let value = self.external_latch;
        self.external_address = (self.external_address + 1) & 0x7f_ffff;
        self.external_latch = self.read_rom(self.external_address);
        value
    }

    fn voice_slots(&self, bank: usize, group: usize) -> Vec<usize> {
        match (self.groups[group].sync, bank) {
            (0, 0) => vec![group, group + 12, group + 24, group + 36],
            (1, 0) => vec![group, group + 24],
            (1, 1) => vec![group + 12, group + 36],
            (2, 0) => vec![group, group + 12, group + 24],
            (2, 3) => vec![group + 36],
            (3, _) => vec![group + 12 * bank],
            _ => Vec::new(),
        }
    }

    fn write_slot(&mut self, index: usize, register: u8, value: u8) {
        let slot = &mut self.slots[index];
        match register {
            0 => {
                slot.external = value & 0xf8;
                if value & 1 != 0 {
                    slot.key_on();
                    if index.is_multiple_of(4) {
                        self.end_status &= !(1 << (index >> 2));
                    }
                } else if slot.envelope != Envelope::Off {
                    slot.envelope = Envelope::Release;
                }
            }
            1 => slot.lfo_freq = value,
            2 => {
                slot.lfo_wave = value & 3;
                slot.pms = (value >> 3) & 7;
                slot.ams = value >> 6;
            }
            3 => {
                slot.multiple = value & 15;
                slot.detune = (value >> 4) & 7;
            }
            4 => slot.level = value & 127,
            5 => {
                slot.attack = value & 31;
                slot.key_scale = value >> 5;
            }
            6 => slot.decay = value & 31,
            7 => slot.sustain_rate = value & 31,
            8 => {
                slot.release = value & 15;
                slot.sustain = value >> 4;
            }
            9 => {
                slot.fnum = (u16::from(slot.fnum_latch & 15) << 8) | u16::from(value);
                slot.block = ((slot.fnum_latch >> 4) as i8 ^ 8) - 8;
                slot.update_keycode(index);
            }
            10 => slot.fnum_latch = value,
            11 => {
                slot.wave = value & 7;
                slot.feedback = (value >> 4) & 7;
                slot.accumulate = value & 0x80 != 0;
                slot.update_keycode(index);
            }
            12 => {
                slot.algorithm = value & 15;
                self.groups[index % 12].dirty = true;
            }
            13 => {
                slot.pan[0] = value >> 4;
                slot.pan[1] = value & 15;
            }
            14 => {
                slot.pan[2] = value >> 4;
                slot.pan[3] = value & 15;
            }
            _ => {}
        }
    }

    fn write_fm(&mut self, bank: usize, register: u8, value: u8) {
        let group = GROUPS[usize::from(register & 15)];
        if group < 0 {
            return;
        }
        let group = group as usize;
        let operation = register >> 4;
        let slots = self.voice_slots(bank, group);
        if matches!(operation, 0 | 9 | 10 | 12 | 13 | 14)
            && !slots.is_empty()
            && self.groups[group].sync != 3
        {
            for slot in slots {
                self.write_slot(slot, operation, value);
            }
        } else {
            self.write_slot(group + bank * 12, operation, value);
        }
    }

    fn write_pcm(&mut self, register: u8, value: u8) {
        let group = GROUPS[usize::from(register & 15)];
        if group < 0 {
            return;
        }
        let index = group as usize * 4;
        let slot = &mut self.slots[index];
        let update = |old: u32, shift: u32, value: u8| {
            let mask = if shift == 16 { 0x7f } else { 0xff };
            (old & !(mask << shift)) | (u32::from(value) & mask) << shift
        };
        match register >> 4 {
            r @ 0..=2 => slot.pcm_start = update(slot.pcm_start, u32::from(r) * 8, value),
            r @ 3..=5 => slot.pcm_end = update(slot.pcm_end, u32::from(r - 3) * 8, value),
            r @ 6..=8 => slot.pcm_loop = update(slot.pcm_loop, u32::from(r - 6) * 8, value),
            9 => {
                slot.pcm_rate = value & 3;
                slot.pcm_12bit = value & 4 != 0;
                slot.pcm_srcnote = (value >> 3) & 3;
                slot.pcm_srcblock = value >> 5;
                slot.update_keycode(index);
            }
            _ => {}
        }
    }

    fn read_rom(&self, address: u32) -> u8 {
        self.rom
            .get((address & 0x7f_ffff) as usize)
            .copied()
            .unwrap_or(0xff)
    }

    fn write_util(&mut self, register: u8, value: u8) {
        if register < 16 {
            let group = GROUPS[usize::from(register)];
            if group >= 0 {
                self.groups[group as usize].sync = value & 3;
                self.groups[group as usize].dirty = true;
            }
            return;
        }
        match register {
            0x10 => self.timer_a = (self.timer_a & 3) | (u16::from(value) << 2),
            0x11 => self.timer_a = (self.timer_a & 0x3fc) | u16::from(value & 3),
            0x12 => self.timer_b = value,
            0x13 => {
                if !self.timer_control & value & 1 != 0 {
                    self.timer_a_remaining = Some(1024 - u32::from(self.timer_a));
                }
                if !self.timer_control & value & 2 != 0 {
                    self.timer_b_remaining = Some(16 * (256 - u32::from(self.timer_b)));
                }
                if value & 0x10 != 0 {
                    self.status &= !1;
                }
                if value & 0x20 != 0 {
                    self.status &= !2;
                }
                self.timer_control = value;
            }
            0x14 => self.external_address = (self.external_address & 0xffff00) | u32::from(value),
            0x15 => {
                self.external_address = (self.external_address & 0xff00ff) | (u32::from(value) << 8)
            }
            0x16 => {
                self.external_address =
                    (self.external_address & 0xffff) | (u32::from(value & 127) << 16);
                self.external_read = value & 128 != 0;
                if self.external_read {
                    self.external_latch = self.read_rom(self.external_address);
                }
            }
            0x17 => {
                self.external_address = (self.external_address + 1) & 0x7f_ffff;
                if !self.external_read
                    && let Some(byte) = self.rom.get_mut(self.external_address as usize)
                {
                    *byte = value;
                }
            }
            _ => {}
        }
    }

    fn connect(&mut self, algorithm: Algorithm, indices: &[usize]) {
        for (position, &index) in indices.iter().enumerate() {
            let slot = &mut self.slots[index];
            slot.mod_count = 0;
            for (other, &modulator) in indices.iter().enumerate() {
                if algorithm.mods[position] & (1 << other) != 0 {
                    slot.modulators[usize::from(slot.mod_count)] = modulator as u8;
                    slot.mod_count += 1;
                }
            }
            slot.carrier = algorithm.carriers & (1 << position) != 0;
            slot.feedback_head = position == 0;
            slot.feedback_target =
                (position == usize::from(algorithm.feedback)).then_some(indices[0] as u8);
        }
    }

    fn rebuild(&mut self, group: usize) {
        self.groups[group].dirty = false;
        match self.groups[group].sync {
            0 => self.connect(
                ALG4[usize::from(self.slots[group].algorithm)],
                &[group, group + 12, group + 24, group + 36],
            ),
            1 => {
                self.connect(
                    ALG2[usize::from(self.slots[group].algorithm & 3)],
                    &[group, group + 24],
                );
                self.connect(
                    ALG2[usize::from(self.slots[group + 12].algorithm & 3)],
                    &[group + 12, group + 36],
                );
            }
            2 => {
                self.connect(
                    ALG3[usize::from(self.slots[group].algorithm & 7)],
                    &[group, group + 12, group + 24],
                );
                self.connect(SINGLE, &[group + 36]);
            }
            _ => {
                for bank in 0..4 {
                    self.connect(SINGLE, &[group + bank * 12]);
                }
            }
        }
    }

    fn pcm_word(&self, slot: &Slot, position: u32) -> i32 {
        if !slot.pcm_12bit {
            i32::from(self.read_rom(slot.pcm_start.wrapping_add(position)) as i8) << 4
        } else {
            let address = slot.pcm_start.wrapping_add((position >> 1).wrapping_mul(3));
            let low = self.read_rom(address.wrapping_add(1));
            let value = if position & 1 != 0 {
                (u16::from(self.read_rom(address.wrapping_add(2))) << 4) | u16::from(low >> 4)
            } else {
                (u16::from(self.read_rom(address)) << 4) | u16::from(low & 15)
            };
            i32::from(value ^ 0x800) - 0x800
        }
    }

    fn pcm_sample(&mut self, slot: &mut Slot, index: usize, pm: i32) -> i32 {
        let a = self.pcm_word(slot, slot.pcm_position);
        let b = self.pcm_word(slot, slot.pcm_position.wrapping_add(1));
        let fraction = (slot.pcm_fraction >> 8) as i32;
        let output = (a * (256 - fraction) + b * fraction) >> 6;
        slot.pcm_fraction = slot.pcm_fraction.wrapping_add(slot.step(true, pm));
        slot.pcm_position = slot.pcm_position.wrapping_add(slot.pcm_fraction >> 16);
        slot.pcm_fraction &= 0xffff;
        if slot.pcm_position >= slot.pcm_end {
            slot.pcm_position = if slot.pcm_end > slot.pcm_loop {
                slot.pcm_loop + (slot.pcm_position - slot.pcm_end) % (slot.pcm_end - slot.pcm_loop)
            } else {
                slot.pcm_loop
            };
            if !slot.pcm_ended {
                slot.pcm_ended = true;
                self.end_status |= 1 << (index >> 2);
            }
        }
        output
    }

    fn env_mul(&self, value: i32, envelope: u32) -> i32 {
        (value * i32::from(self.exponential[((envelope & 63) << 2) as usize]))
            >> (11 + (envelope >> 6))
    }

    fn operator(&self, phase: u32, wave: u8, envelope: u32) -> i32 {
        let phase = phase & 1023;
        let mut index = phase & 255;
        if phase & 256 != 0 {
            index ^= 255;
        }
        let (attenuation, negative) = match wave {
            0 => (u32::from(self.logsin[index as usize]), phase & 512 != 0),
            1 => (
                u32::from(self.logsin[index as usize]) << 1,
                phase & 512 != 0,
            ),
            2 => (u32::from(self.logsin[index as usize]), false),
            3 if phase & 512 == 0 => (u32::from(self.logsin[index as usize]), false),
            4 | 5 if phase & 512 == 0 => {
                index = (phase << 1) & 255;
                if phase & 128 != 0 {
                    index ^= 255;
                }
                (
                    u32::from(self.logsin[index as usize]),
                    wave == 4 && phase & 256 != 0,
                )
            }
            _ => return 0,
        };
        let attenuation = attenuation + (envelope << 2);
        if attenuation >= 4096 {
            return 0;
        }
        let result =
            (i32::from(self.exponential[(attenuation & 255) as usize]) << 2) >> (attenuation >> 8);
        if negative { -result } else { result }
    }

    fn tick(&mut self) -> [i32; 8] {
        for group in 0..12 {
            if self.groups[group].dirty {
                self.rebuild(group);
            }
        }
        let eg_clock = self.eg_phase;
        self.eg_phase = !self.eg_phase;
        if eg_clock {
            self.eg_counter = self.eg_counter.wrapping_add(1);
        }
        let mut output = [0; 8];
        for index in 0..48 {
            let mut slot = self.slots[index];
            if eg_clock {
                slot.envelope_tick(self.eg_counter);
            }
            slot.lfo_tick();
            if slot.envelope == Envelope::Off {
                slot.output = 0;
                slot.sum = 0;
                self.slots[index] = slot;
                continue;
            }
            let modulation = if slot.feedback_head {
                if slot.feedback == 0 {
                    0
                } else {
                    (slot.history[0] + slot.history[1]) >> (10 - slot.feedback)
                }
            } else {
                let sum: i32 = slot.modulators[..usize::from(slot.mod_count)]
                    .iter()
                    .map(|&i| self.slots[usize::from(i)].output)
                    .sum();
                (sum * MOD_LEVEL[usize::from(slot.feedback)]) >> 8
            };
            let mut envelope = slot.attenuation as u32 + (u32::from(slot.level) << 3);
            if slot.ams != 0 && slot.lfo_wave != 0 {
                let am = slot.lfo_am();
                envelope += match slot.ams {
                    1 => am >> 1,
                    2 => am,
                    _ => am << 1,
                };
            }
            envelope = envelope.min(1023);
            let pm = if slot.pms != 0 && slot.lfo_wave != 0 {
                slot.lfo_pm()
            } else {
                0
            };
            let mut value = if slot.wave == 7 {
                if index.is_multiple_of(4) {
                    let sample = self.pcm_sample(&mut slot, index, pm);
                    self.env_mul(sample, envelope)
                } else {
                    0
                }
            } else if slot.wave == 6 {
                let m = if slot.multiple == 0 {
                    modulation >> 1
                } else {
                    modulation * i32::from(slot.multiple)
                };
                let linear = 8192 + (m.wrapping_shl(6) & 32767);
                slot.phase = slot.phase.wrapping_add(slot.step(false, pm));
                self.env_mul(linear, envelope)
            } else {
                let phase = (slot.phase >> 22).wrapping_add(modulation as u32);
                let value = self.operator(phase, slot.wave, envelope);
                slot.phase = slot.phase.wrapping_add(slot.step(false, pm));
                value
            };
            if slot.accumulate {
                slot.sum = (slot.sum + value).clamp(-8192, 8191);
                value = slot.sum;
            }
            slot.output = value;
            if slot.feedback_target == Some(index as u8) {
                slot.history = [value, slot.history[0]];
            } else if let Some(target) = slot.feedback_target {
                let history = &mut self.slots[usize::from(target)].history;
                *history = [value, history[0]];
            }
            if slot.carrier {
                for (channel, &level) in slot.pan.iter().enumerate() {
                    if level < 13 {
                        let v = if level & 1 != 0 {
                            (value * 3) >> 2
                        } else {
                            value
                        };
                        output[channel] += v >> (level >> 1);
                    }
                }
                if slot.external & 0x80 != 0 {
                    for channel in 0..4 {
                        if slot.external & (8 << channel) != 0 {
                            output[channel + 4] += value;
                        }
                    }
                }
            }
            self.slots[index] = slot;
        }
        for (counter, period, flag) in [
            (
                &mut self.timer_a_remaining,
                1024 - u32::from(self.timer_a),
                1,
            ),
            (
                &mut self.timer_b_remaining,
                16 * (256 - u32::from(self.timer_b)),
                2,
            ),
        ] {
            if let Some(remaining) = counter {
                *remaining -= 1;
                if *remaining == 0 {
                    *remaining = period;
                    self.status |= flag;
                }
            }
        }
        output
    }
}

impl SoundChip for Ymf271 {
    fn chip_type(&self) -> &Chip {
        &Chip::Ymf271
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
        8
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        if bank == 4 && register >> 4 == 2 && value & 0x80 != 0 {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Ymf271,
                reason: "alternate PCM looping is not implemented",
            });
        }
        match bank {
            0..=3 => self.write_fm(usize::from(bank), register, value),
            4 => self.write_pcm(register, value),
            6 => self.write_util(register, value),
            _ => {
                return Err(ChipError::InvalidRegisterBank {
                    chip: Chip::Ymf271,
                    bank,
                });
            }
        }
        Ok(())
    }
    fn write_rom(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        let end = start
            .checked_add(data.len())
            .filter(|&end| end <= 0x800000)
            .ok_or(ChipError::RomTooLarge)?;
        if self.rom.len() < end {
            self.rom.resize(end, 0xff);
        }
        self.rom[start..end].copy_from_slice(data);
        Ok(())
    }
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if !buffer.len().is_multiple_of(8) {
            return Err(ChipError::InvalidBufferLength {
                length: buffer.len(),
                channels: 8,
            });
        }
        for frame in buffer.as_chunks_mut::<8>().0 {
            *frame = self.tick();
        }
        Ok(())
    }
    fn reset(&mut self) {
        let rom = std::mem::take(&mut self.rom);
        *self = Self::new(self.clock).expect("a constructed YMF271 has a valid clock");
        self.rom = rom;
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Ymf271)
    }
}

impl Slot {
    fn key_scale_value(&self) -> u8 {
        let k = self.keycode;
        match self.key_scale {
            0 => 0,
            1 => k >> 3,
            2 => k >> 2,
            3 => k >> 1,
            4 => k,
            5 => k.saturating_add(2).min(31),
            6 => k.saturating_add(4).min(31),
            _ => k.saturating_add(8).min(31),
        }
    }

    fn update_keycode(&mut self, slot: usize) {
        let block = (self.block as u8) & 7;
        let value = if self.wave == 7 && slot.is_multiple_of(4) {
            let f = self.fnum & 0x7ff;
            let note = if f < 0x100 {
                0
            } else if f < 0x300 {
                1
            } else if f < 0x500 {
                2
            } else {
                3
            };
            self.pcm_srcblock * 4 + self.pcm_srcnote + block * 4 + note
        } else {
            let note = if self.fnum < 0x780 {
                0
            } else if self.fnum < 0x900 {
                1
            } else if self.fnum < 0xa80 {
                2
            } else {
                3
            };
            if self.block < 0 {
                note
            } else {
                block * 4 + note
            }
        };
        self.keycode = value & 31;
    }

    fn key_on(&mut self) {
        self.envelope = Envelope::Attack;
        if self.attack != 0 && u16::from(self.attack) * 2 + u16::from(self.key_scale_value()) >= 63
        {
            self.attenuation = 0;
        }
        self.phase = 0;
        self.lfo_counter = 0;
        self.lfo_position = 0;
        self.pcm_position = 0;
        self.pcm_fraction = 0;
        self.pcm_ended = false;
        self.sum = 0;
    }

    fn envelope_tick(&mut self, counter: u32) {
        if self.envelope == Envelope::Off {
            return;
        }
        if self.envelope == Envelope::Attack && self.attenuation <= 0 {
            self.attenuation = 0;
            self.envelope = Envelope::Decay1;
        }
        if self.envelope == Envelope::Decay1 {
            let target = i32::from(if self.sustain == 15 { 31 } else { self.sustain }) << 5;
            if self.attenuation >= target {
                self.envelope = Envelope::Decay2;
            }
        }
        let base = match self.envelope {
            Envelope::Attack => self.attack * 2,
            Envelope::Decay1 => self.decay * 2,
            Envelope::Decay2 => self.sustain_rate * 2,
            Envelope::Release => self.release * 4,
            Envelope::Off => return,
        };
        let rate = if base == 0 {
            0
        } else {
            base.saturating_add(self.key_scale_value()).min(63)
        };
        let index = if rate < 48 {
            let shift = 11 - (rate >> 2);
            if counter & ((1 << shift) - 1) != 0 {
                return;
            }
            (counter >> shift) & 7
        } else {
            counter & 7
        };
        let increment = ((EG_INCREMENT[usize::from(rate)] >> (index * 4)) & 15) as i32;
        if self.envelope == Envelope::Attack {
            self.attenuation += ((!self.attenuation) * increment) >> 4;
            if self.attenuation <= 0 {
                self.attenuation = 0;
                self.envelope = Envelope::Decay1;
            }
        } else {
            self.attenuation += increment;
            if self.attenuation >= 1023 {
                self.attenuation = 1023;
                if self.envelope == Envelope::Release {
                    self.envelope = Envelope::Off;
                }
            }
        }
    }

    fn lfo_tick(&mut self) {
        if self.lfo_wave == 0 {
            return;
        }
        let n = u32::from(self.lfo_freq);
        let period = if n >= 240 {
            16 - (n & 15)
        } else {
            (32 - (n & 15)) << (14 - (n >> 4))
        };
        self.lfo_counter += 1;
        if self.lfo_counter >= period {
            self.lfo_counter = 0;
            self.lfo_position = (self.lfo_position + 1) & 127;
        }
    }

    fn lfo_pm(&self) -> i32 {
        let p = i32::from(self.lfo_position);
        match self.lfo_wave {
            1 => ((p + 64) & 127) * 2 - 128,
            2 => {
                if p < 64 {
                    127
                } else {
                    -128
                }
            }
            3 => {
                if p < 32 {
                    p * 4
                } else if p < 96 {
                    128 - (p - 32) * 4
                } else {
                    (p - 96) * 4 - 128
                }
            }
            _ => 0,
        }
    }

    fn lfo_am(&self) -> u32 {
        let p = u32::from(self.lfo_position);
        match self.lfo_wave {
            1 => 127 - p,
            2 => {
                if p < 64 {
                    127
                } else {
                    0
                }
            }
            3 => {
                if p < 64 {
                    127 - p * 2
                } else {
                    (p - 64) * 2 + 1
                }
            }
            _ => 0,
        }
    }

    fn step(&self, pcm: bool, pm: i32) -> u32 {
        let f = if pcm {
            (self.fnum & 0x7ff) | 0x800
        } else {
            self.fnum
        };
        let mut fnum = i64::from(f) << 7;
        fnum += (i64::from(f) * PMS[usize::from(self.pms)] * i64::from(pm)) >> 10;
        let mut increment = if pcm {
            (fnum << 16) >> (18 - i32::from(self.block))
        } else {
            (fnum << (i32::from(self.block) + 11)) >> 7
        };
        let detune = i64::from(DETUNE[usize::from(self.keycode)][usize::from(self.detune & 3)])
            << if pcm { 6 } else { 12 };
        increment += if self.detune & 4 != 0 {
            -detune
        } else {
            detune
        };
        increment = increment.max(0);
        increment = if self.multiple == 0 {
            increment >> 1
        } else {
            increment * i64::from(self.multiple)
        };
        if pcm {
            increment >>= self.pcm_rate;
        }
        increment as u32
    }
}

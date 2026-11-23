//! Sega/Yamaha SCSP synthesis, based on MAME revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) ElSemi and R. Belmont. See `THIRD_PARTY_NOTICES.md`.

use super::scsp_dsp::Dsp;
use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;
use std::num::NonZeroU32;
use std::sync::{Arc, OnceLock};

const RAM_SIZE: usize = 1 << 20;
const SHIFT: u32 = 12;
const AR_TIMES: [f64; 64] = [
    100000., 100000., 8100., 6900., 6000., 4800., 4000., 3400., 3000., 2400., 2000., 1700., 1500.,
    1200., 1000., 860., 760., 600., 500., 430., 380., 300., 250., 220., 190., 150., 130., 110.,
    95., 76., 63., 55., 47., 38., 31., 27., 24., 19., 15., 13., 12., 9.4, 7.9, 6.8, 6., 4.7, 3.8,
    3.4, 3., 2.4, 2., 1.8, 1.6, 1.3, 1.1, 0.93, 0.85, 0.65, 0.53, 0.44, 0.40, 0.35, 0., 0.,
];
const DR_TIMES: [f64; 64] = [
    100000., 100000., 118200., 101300., 88600., 70900., 59100., 50700., 44300., 35500., 29600.,
    25300., 22200., 17700., 14800., 12700., 11100., 8900., 7400., 6300., 5500., 4400., 3700.,
    3200., 2800., 2200., 1800., 1600., 1400., 1100., 920., 790., 690., 550., 460., 390., 340.,
    270., 230., 200., 170., 140., 110., 98., 85., 68., 57., 49., 43., 34., 28., 25., 22., 18., 14.,
    12., 11., 8.5, 7.1, 6.1, 5.4, 4.3, 3.6, 3.1,
];
const LFO_FREQ: [f32; 32] = [
    0.17, 0.19, 0.23, 0.27, 0.34, 0.39, 0.45, 0.55, 0.68, 0.78, 0.92, 1.10, 1.39, 1.60, 1.87, 2.27,
    2.87, 3.31, 3.92, 4.79, 6.15, 7.18, 8.60, 10.8, 14.4, 17.2, 21.5, 28.7, 43.1, 57.4, 86.1,
    172.3,
];

struct Tables {
    pan: Vec<[i32; 2]>,
    envelope: [i32; 1024],
    attack: [i32; 64],
    decay: [i32; 64],
    pitch_lfo: [[i32; 256]; 8],
    amp_lfo: [[i32; 256]; 8],
}

fn tables() -> &'static Tables {
    static TABLES: OnceLock<Tables> = OnceLock::new();
    TABLES.get_or_init(|| {
        let mut pan = Vec::with_capacity(65536);
        for encoded in 0..65536 {
            let tl = encoded & 255;
            let p = (encoded >> 8) & 31;
            let send = (encoded >> 13) & 7;
            let tl_db: f32 = [0.4, 0.8, 1.5, 3., 6., 12., 24., 48.]
                .iter()
                .enumerate()
                .filter(|(i, _)| tl & (1 << i) != 0)
                .map(|(_, v)| *v)
                .sum();
            let pan_db: f32 = [3., 6., 12., 24.]
                .iter()
                .enumerate()
                .filter(|(i, _)| p & (1 << i) != 0)
                .map(|(_, v)| *v)
                .sum();
            let attenuation = if p & 15 == 15 {
                0.0
            } else {
                10_f32.powf(-pan_db / 20.)
            };
            let level = if send == 0 {
                0.
            } else {
                10_f32.powf((send as f32 - 7.) * 6. / 20.)
            };
            let base = 4. * 10_f32.powf(-tl_db / 20.) * level;
            let (left, right) = if p < 16 {
                (attenuation, 1.)
            } else {
                (1., attenuation)
            };
            pan.push([(4096. * base * left) as i32, (4096. * base * right) as i32]);
        }
        let mut envelope = [0; 1024];
        for (i, value) in envelope.iter_mut().enumerate() {
            *value = (10_f32.powf((3. * (i as f32 - 1023.) / 32.) / 20.) * 4096.) as i32;
        }
        let mut attack = [0; 64];
        let mut decay = [0; 64];
        for i in 2..64 {
            attack[i] = if AR_TIMES[i] == 0. {
                1024 << 16
            } else {
                (1023. * 1000. / (44100. * AR_TIMES[i]) * 65536.) as i32
            };
            decay[i] = (1023. * 1000. / (44100. * DR_TIMES[i]) * 65536.) as i32;
        }
        let mut pitch_lfo = [[0; 256]; 8];
        let mut amp_lfo = [[0; 256]; 8];
        let pitch_scale = [0., 7., 13.5, 27., 55., 112., 230., 494.];
        let amp_scale = [0., 0.4, 0.8, 1.5, 3., 6., 12., 24.];
        for s in 0..8 {
            for i in 0..256 {
                pitch_lfo[s][i] =
                    (256. * 2_f32.powf(pitch_scale[s] * (i as f32 - 128.) / 128. / 1200.)) as i32
                        * 16;
                amp_lfo[s][i] =
                    (256. * 10_f32.powf(-amp_scale[s] * i as f32 / 256. / 20.)) as i32 * 16;
            }
        }
        Tables {
            pan,
            envelope,
            attack,
            decay,
            pitch_lfo,
            amp_lfo,
        }
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EnvelopeState {
    Attack,
    Decay1,
    Decay2,
    Release,
}

#[derive(Clone, Copy)]
struct Slot {
    regs: [u16; 16],
    active: bool,
    position: u32,
    next: u32,
    step: u32,
    backwards: bool,
    envelope_state: EnvelopeState,
    envelope: i32,
    rates: [i32; 4],
    decay_level: i32,
    hold: bool,
    pitch_phase: u16,
    amp_phase: u16,
    lfo_step: u16,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            regs: [0; 16],
            active: false,
            position: 0,
            next: 0,
            step: 0,
            backwards: false,
            envelope_state: EnvelopeState::Release,
            envelope: 0,
            rates: [0; 4],
            decay_level: 0,
            hold: false,
            pitch_phase: 0,
            amp_phase: 0,
            lfo_step: 0,
        }
    }
}

impl Slot {
    fn update_step(&mut self) {
        let oct = i32::from((self.regs[8] >> 11) & 15);
        let shift = (oct ^ 8) - 8 + 2;
        let freq = u32::from((self.regs[8] & 1023) + 1024);
        self.step = if shift >= 0 {
            freq << shift
        } else {
            freq >> -shift
        };
    }

    fn update_lfo(&mut self) {
        let index = usize::from((self.regs[9] >> 10) & 31);
        self.lfo_step = (LFO_FREQ[index] * 65536. / 44100.) as u16;
    }

    fn start(&mut self) {
        self.active = true;
        self.position = 0;
        self.next = 1 << SHIFT;
        self.backwards = false;
        self.update_step();
        self.update_lfo();
        let oct = i32::from((self.regs[8] >> 11) & 15);
        let krs = i32::from((self.regs[5] >> 10) & 15);
        let base = if krs == 15 {
            0
        } else {
            (oct ^ 8) - 8 + 2 * krs + i32::from((self.regs[8] >> 9) & 1)
        };
        let rate = |r: u16| (base + i32::from(r) * 2).clamp(0, 63) as usize;
        let t = tables();
        self.rates = [
            t.attack[rate(self.regs[4] & 31)],
            t.decay[rate((self.regs[4] >> 6) & 31)],
            t.decay[rate((self.regs[4] >> 11) & 31)],
            t.decay[rate(self.regs[5] & 31)],
        ];
        self.decay_level = 31 - i32::from((self.regs[5] >> 5) & 31);
        self.hold = self.regs[4] & 0x20 != 0;
        self.envelope_state = EnvelopeState::Attack;
        self.envelope = 0x17f << 16;
    }

    fn stop(&mut self, release: bool) {
        if release {
            self.envelope_state = EnvelopeState::Release;
        } else {
            self.active = false;
        }
        self.regs[0] &= !0x800;
    }

    fn envelope_gain(&mut self) -> i32 {
        match self.envelope_state {
            EnvelopeState::Attack => {
                self.envelope = self.envelope.saturating_add(self.rates[0]);
                if self.envelope >= 0x3ff << 16 {
                    if self.regs[5] & 0x4000 == 0 {
                        self.envelope_state = if self.rates[1] >= 1024 << 16 {
                            EnvelopeState::Decay2
                        } else {
                            EnvelopeState::Decay1
                        };
                    }
                    self.envelope = 0x3ff << 16;
                }
                if self.hold {
                    return 0x3ff << 2;
                }
            }
            EnvelopeState::Decay1 => {
                self.envelope = (self.envelope - self.rates[1]).max(0);
                if self.envelope >> 21 <= self.decay_level {
                    self.envelope_state = EnvelopeState::Decay2;
                }
            }
            EnvelopeState::Decay2 => {
                if self.regs[4] >> 11 != 0 {
                    self.envelope = (self.envelope - self.rates[2]).max(0);
                }
            }
            EnvelopeState::Release => {
                self.envelope = (self.envelope - self.rates[3]).max(0);
                if self.envelope == 0 {
                    self.stop(false);
                }
            }
        }
        (self.envelope >> 16) << 2
    }
}

/// Sega/Yamaha SCSP (YMF292) with 32 slots, PCM, FM modulation and programmable DSP.
///
/// The sample and DSP memory share a 1 MiB big-endian address space. The
/// external audio inputs and host CPU interrupts are not connected.
/// Envelope and DSP behavior follows MAME's approximations; noise uses a
/// deterministic generator. Playback has not been verified against a
/// complete SCSP VGM recording.
#[derive(Clone)]
pub struct Scsp {
    clock: u32,
    slots: [Slot; 32],
    common: [u16; 24],
    ring: [i16; 128],
    ring_pos: usize,
    ram: Vec<u8>,
    dsp: Dsp,
    noise: u32,
    noise_table: Arc<[u8; 256]>,
}

impl Scsp {
    /// Create a chip using its input clock.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 512 {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: 512,
            });
        }
        let mut seed = 0x1234_5678_u32;
        let noise_table = Arc::new(std::array::from_fn(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as u8
        }));
        Ok(Self {
            clock,
            slots: [Slot::default(); 32],
            common: [0; 24],
            ring: [0; 128],
            ring_pos: 0,
            ram: vec![0; RAM_SIZE],
            dsp: Dsp::default(),
            noise: seed,
            noise_table,
        })
    }

    fn read_word(&self, address: usize) -> u16 {
        let at = address & (RAM_SIZE - 1);
        u16::from_be_bytes([self.ram[at], self.ram[(at + 1) & (RAM_SIZE - 1)]])
    }

    fn reg_word(&self, address: usize) -> u16 {
        match address {
            0x000..=0x3ff => self.slots[address / 32].regs[(address & 31) / 2],
            0x400..=0x42f => self.common[(address - 0x400) / 2],
            0x600..=0x6ff => self.ring[(address - 0x600) / 2] as u16,
            0x700..=0x77f => self.dsp.coef[(address - 0x700) / 2] as u16,
            0x780..=0x7ff => self.dsp.addresses[(address & 0x3f) / 2],
            0x800..=0xbff => self.dsp.program[(address - 0x800) / 2],
            0xc00..=0xeff => self.dsp.read_register(address),
            _ => 0,
        }
    }

    fn write_word(&mut self, address: usize, value: u16, allow_dma: bool) {
        match address {
            0x000..=0x3ff => {
                let i = address / 32;
                let reg = (address & 31) / 2;
                self.slots[i].regs[reg] = value;
                match reg {
                    0 if value & 0x1000 != 0 => {
                        for slot in &mut self.slots {
                            if slot.regs[0] & 0x800 != 0
                                && slot.envelope_state == EnvelopeState::Release
                            {
                                slot.start();
                            } else if slot.regs[0] & 0x800 == 0 {
                                slot.stop(true);
                            }
                        }
                        self.slots[i].regs[0] &= !0x1000;
                    }
                    5 => {
                        self.slots[i].rates[3] = tables().decay[usize::from(value & 31) * 2];
                        self.slots[i].decay_level = 31 - i32::from((value >> 5) & 31);
                    }
                    8 => self.slots[i].update_step(),
                    9 => self.slots[i].update_lfo(),
                    _ => {}
                }
            }
            0x400..=0x42f => {
                self.common[(address - 0x400) / 2] = value;
                match address {
                    0x402 => {
                        self.dsp.ring_length = 8192 << ((value >> 7) & 3);
                        self.dsp.ring_base = u32::from(value & 63);
                    }
                    0x416 if value & 0x1000 != 0 && allow_dma => self.dma(),
                    _ => {}
                }
            }
            0x600..=0x6ff => self.ring[(address - 0x600) / 2] = value as i16,
            0x700..=0x77f => self.dsp.coef[(address - 0x700) / 2] = value as i16,
            0x780..=0x7bf => self.dsp.addresses[(address - 0x780) / 2] = value,
            0x800..=0xbff => {
                self.dsp.program[(address - 0x800) / 2] = value;
                if address == 0xbf0 {
                    self.dsp.start();
                }
            }
            _ => {}
        }
    }

    fn dma(&mut self) {
        let params = [self.common[9], self.common[10], self.common[11]];
        let mut mem = usize::from(params[0] & 0xfffe) | (usize::from(params[1] & 0xf000) << 4);
        let mut reg = usize::from(params[1] & 0x0ffe);
        let length = usize::from(params[2] & 0x0ffe);
        let to_mem = params[2] & 0x2000 != 0;
        let clear = params[2] & 0x4000 != 0;
        for _ in (0..length).step_by(2) {
            if to_mem {
                let value = if clear {
                    0
                } else {
                    self.reg_word(reg & 0xfffe)
                };
                let [high, low] = value.to_be_bytes();
                self.ram[mem & (RAM_SIZE - 1)] = high;
                self.ram[(mem + 1) & (RAM_SIZE - 1)] = low;
                mem = mem.wrapping_add(2);
                if !clear {
                    reg = reg.wrapping_add(2);
                }
            } else {
                let value = if clear { 0 } else { self.read_word(mem) };
                self.write_word(reg & 0xfffe, value, false);
                reg = reg.wrapping_add(2);
                if !clear {
                    mem = mem.wrapping_add(2);
                }
            }
        }
        if !to_mem {
            self.common[9..12].copy_from_slice(&params);
        }
        self.common[11] &= !0x1000;
    }

    fn lfo_wave(&self, phase: u16, waveform: u16, amplitude: bool) -> i32 {
        let i = i32::from(phase >> 8);
        if amplitude {
            match waveform {
                0 => 255 - i,
                1 => {
                    if i < 128 {
                        255
                    } else {
                        0
                    }
                }
                2 => {
                    if i < 128 {
                        255 - 2 * i
                    } else {
                        2 * i - 256
                    }
                }
                _ => i32::from(self.noise_table[i as usize]),
            }
        } else {
            match waveform {
                0 => {
                    if i < 128 {
                        i
                    } else {
                        i - 256
                    }
                }
                1 => {
                    if i < 128 {
                        127
                    } else {
                        -128
                    }
                }
                2 => {
                    if i < 64 {
                        i * 2
                    } else if i < 128 {
                        255 - i * 2
                    } else if i < 192 {
                        256 - i * 2
                    } else {
                        i * 2 - 511
                    }
                }
                _ => 128 - i32::from(self.noise_table[i as usize]),
            }
        }
    }

    fn slot_sample(&mut self, index: usize) -> i32 {
        let mut slot = self.slots[index];
        let r = slot.regs;
        let mode = (r[0] >> 7) & 3;
        if mode == 3 {
            return 0;
        }
        let mut step = slot.step;
        if (r[9] >> 5) & 7 != 0 {
            slot.pitch_phase = slot.pitch_phase.wrapping_add(slot.lfo_step);
            let wave = self.lfo_wave(slot.pitch_phase, (r[9] >> 8) & 3, false);
            let gain = tables().pitch_lfo[usize::from((r[9] >> 5) & 7)][(wave + 128) as usize];
            step = ((u64::from(step) * gain as u64) >> SHIFT) as u32;
        }
        let pcm8 = r[0] & 0x10 != 0;
        let mut a = if pcm8 {
            slot.position >> SHIFT
        } else {
            (slot.position >> (SHIFT - 1)) & !1
        };
        let mut b = if pcm8 {
            slot.next >> SHIFT
        } else {
            (slot.next >> (SHIFT - 1)) & !1
        };
        if r[7] != 0 {
            let x = usize::from((r[7] >> 6) & 63);
            let y = usize::from(r[7] & 63);
            let level = u32::from((r[7] >> 12) & 15);
            let modulation = (((i32::from(self.ring[(self.ring_pos + x) & 63])
                + i32::from(self.ring[(self.ring_pos + y) & 63]))
                / 2)
                << 10)
                >> (26 - level);
            let modulation = if pcm8 { modulation } else { modulation << 1 };
            a = a.wrapping_add_signed(modulation);
            b = b.wrapping_add_signed(modulation);
        }
        let base = (u32::from(r[0] & 15) << 16) | u32::from(r[1]);
        let sample = |addr: u32| -> i32 {
            let addr = base.wrapping_add(addr) as usize & (RAM_SIZE - 1);
            if pcm8 {
                i32::from(self.ram[addr] as i8) << 8
            } else {
                i32::from(self.read_word(addr) as i16)
            }
        };
        let mut sample = match mode {
            0 => {
                let fraction = (slot.position & ((1 << SHIFT) - 1)) as i32;
                (sample(a) * ((1 << SHIFT) - fraction) + sample(b) * fraction) >> SHIFT
            }
            1 => {
                self.noise ^= self.noise << 13;
                self.noise ^= self.noise >> 17;
                self.noise ^= self.noise << 5;
                i32::from(self.noise as i16)
            }
            _ => 0,
        };
        if r[0] & 0x200 != 0 {
            sample ^= 0x7fff;
        }
        if r[0] & 0x400 != 0 {
            sample = i32::from((sample ^ 0x8000) as i16);
        }
        slot.position = if slot.backwards {
            slot.position.wrapping_sub(step)
        } else {
            slot.position.wrapping_add(step)
        };
        slot.next = slot.position.wrapping_add(1 << SHIFT);
        if slot.position >> SHIFT >= u32::from(r[2])
            && !slot.backwards
            && r[5] & 0x4000 != 0
            && slot.envelope_state == EnvelopeState::Attack
        {
            slot.envelope_state = EnvelopeState::Decay1;
        }
        let original = [slot.position >> SHIFT, slot.next >> SHIFT];
        let mut positions = [slot.position, slot.next];
        for j in 0..2 {
            let start = u32::from(r[2]) << SHIFT;
            let end = u32::from(r[3]) << SHIFT;
            match (r[0] >> 5) & 3 {
                0 if original[j] >= u32::from(r[2]) && original[j] >= u32::from(r[3]) => {
                    slot.stop(false)
                }
                1 if original[j] >= u32::from(r[3]) => {
                    positions[j] = start.wrapping_add(positions[j].wrapping_sub(end))
                }
                2 if original[j] >= u32::from(r[2]) && !slot.backwards => {
                    positions[j] = end.wrapping_sub(positions[j].wrapping_sub(start));
                    slot.backwards = true;
                }
                2 if (original[j] < u32::from(r[2]) || positions[j] & 0x8000_0000 != 0)
                    && slot.backwards =>
                {
                    positions[j] = end.wrapping_sub(start.wrapping_sub(positions[j]));
                }
                3 if original[j] >= u32::from(r[3]) => {
                    positions[j] = end.wrapping_sub(positions[j].wrapping_sub(end));
                    slot.backwards = true;
                }
                3 if (original[j] < u32::from(r[2]) || positions[j] & 0x8000_0000 != 0)
                    && slot.backwards =>
                {
                    positions[j] = start.wrapping_add(start.wrapping_sub(positions[j]));
                    slot.backwards = false;
                }
                _ => {}
            }
        }
        slot.position = positions[0];
        slot.next = positions[1];
        if r[6] & 0x100 == 0 {
            if r[9] & 7 != 0 {
                slot.amp_phase = slot.amp_phase.wrapping_add(slot.lfo_step);
                let wave = self.lfo_wave(slot.amp_phase, (r[9] >> 3) & 3, true);
                sample = (sample * tables().amp_lfo[usize::from(r[9] & 7)][wave as usize]) >> SHIFT;
            }
            let attack = slot.envelope_state == EnvelopeState::Attack;
            let gain = slot.envelope_gain();
            let gain = if attack {
                gain
            } else {
                tables().envelope[(gain >> 2) as usize]
            };
            sample = (sample * gain) >> SHIFT;
        }
        if r[6] & 0x200 == 0 {
            let tl = if r[6] & 0x100 != 0 { 0 } else { r[6] & 255 };
            let gain = tables().pan[usize::from(tl | (7 << 13))][0];
            self.ring[self.ring_pos] = ((sample * gain) >> (SHIFT + 1)) as i16;
        }
        self.slots[index] = slot;
        sample
    }

    fn tick(&mut self) -> [i32; 2] {
        let mut output = [0_i32; 2];
        for i in 0..32 {
            if self.slots[i].active {
                let sample = self.slot_sample(i);
                let r = self.slots[i].regs;
                let tl = if r[6] & 0x100 != 0 { 0 } else { r[6] & 255 };
                let input_gain = tables().pan[usize::from(tl | ((r[10] & 7) << 13))][0];
                let input = (sample * input_gain) >> (SHIFT - 2);
                let target = &mut self.dsp.mix[usize::from((r[10] >> 3) & 15)];
                *target = target.wrapping_add(input);
                let gains = tables().pan[usize::from(tl | (r[11] & 0xff00))];
                for channel in 0..2 {
                    output[channel] += (sample * gains[channel]) >> SHIFT;
                }
            }
            self.ring_pos = (self.ring_pos + 1) & 63;
        }
        self.dsp.step(&mut self.ram);
        for i in 0..16 {
            let r = self.slots[i].regs[11];
            let encoded = ((r & 0xe0) << 8) | ((r & 31) << 8);
            let gains = tables().pan[usize::from(encoded)];
            for channel in 0..2 {
                output[channel] += (i32::from(self.dsp.effects[i]) * gains[channel]) >> SHIFT;
            }
        }
        let volume = i32::from(self.common[0] & 15);
        output.map(|value| (value.clamp(-131072, 131071) >> 2) * volume / 15)
    }
}

impl SoundChip for Scsp {
    fn chip_type(&self) -> &Chip {
        &Chip::Scsp
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 512
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(512).unwrap())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        write.parts_for(&Chip::Scsp)?;
        let ChipRegisterWrite::Scsp(spec) = write else {
            unreachable!()
        };
        let address = usize::from(spec.offset) & !1;
        let old = self.reg_word(address);
        let value = if spec.offset & 1 == 0 {
            (old & 0x00ff) | (u16::from(spec.value) << 8)
        } else {
            (old & 0xff00) | u16::from(spec.value)
        };
        self.write_word(address, value, true);
        Ok(())
    }
    fn write_ram(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= RAM_SIZE)
            .ok_or(ChipError::RamOutOfRange {
                chip: Chip::Scsp,
                start,
                length: data.len(),
            })?;
        self.ram[start..end].copy_from_slice(data);
        Ok(())
    }
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if !buffer.len().is_multiple_of(2) {
            return Err(ChipError::InvalidBufferLength {
                length: buffer.len(),
                channels: 2,
            });
        }
        if !self.dsp.valid() {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Scsp,
                reason: "DSP program uses a reserved input register",
            });
        }
        if self
            .slots
            .iter()
            .any(|slot| slot.active && (slot.regs[0] >> 7) & 3 == 3)
        {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Scsp,
                reason: "active slot uses a reserved sample source",
            });
        }
        for frame in buffer.as_chunks_mut::<2>().0 {
            *frame = self.tick();
        }
        Ok(())
    }
    fn reset(&mut self) {
        let ram = std::mem::take(&mut self.ram);
        *self = Self::new(self.clock).expect("existing SCSP clock is valid");
        self.ram = ram;
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Scsp)
    }
}

//! NES APU based on the JSNES-derived Tetorica native port.
//!
//! JSNES revision `b8a45d088e922b6abcd87c4eb1b7237919e9d3d2`, Apache-2.0.
//! JSNES: Copyright JSNES contributors.
//! Tetorica: Copyright (c) 2026, kyorohiro.
//! See `THIRD_PARTY_NOTICES.md`.

use super::{ChipError, ChipRegisterWrite, SoundChip};
use soundlog::chip::Chip;

const RATE: u32 = 44_100;
const LENGTH: [i32; 32] = [
    10, 254, 20, 2, 40, 4, 80, 6, 160, 8, 60, 10, 14, 12, 26, 14, 12, 16, 24, 18, 48, 20, 96, 22,
    192, 24, 72, 26, 16, 28, 32, 30,
];
const DUTY: [i32; 32] = [
    0, 1, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 0, 1, 1, 1, 1, 1,
];
const NOISE_NTSC: [i32; 16] = [
    4, 8, 16, 32, 64, 96, 128, 160, 202, 254, 380, 508, 762, 1016, 2034, 4068,
];
const NOISE_PAL: [i32; 16] = [
    4, 8, 14, 30, 60, 88, 118, 148, 188, 236, 354, 472, 708, 944, 1890, 3778,
];
const DMC_NTSC: [i32; 16] = [
    0xd60, 0xbe0, 0xaa0, 0xa00, 0x8f0, 0x7f0, 0x710, 0x6b0, 0x5f0, 0x500, 0x470, 0x400, 0x350,
    0x2a0, 0x240, 0x1b0,
];
const DMC_PAL: [i32; 16] = [
    398 * 8,
    354 * 8,
    316 * 8,
    298 * 8,
    276 * 8,
    236 * 8,
    210 * 8,
    198 * 8,
    176 * 8,
    148 * 8,
    132 * 8,
    118 * 8,
    98 * 8,
    78 * 8,
    66 * 8,
    50 * 8,
];

#[derive(Clone, Default)]
struct Envelope {
    disable: bool,
    looping: bool,
    reset: bool,
    rate: i32,
    counter: i32,
    decay: i32,
    volume: i32,
}

impl Envelope {
    fn write(&mut self, value: u8) {
        self.disable = value & 0x10 != 0;
        self.looping = value & 0x20 != 0;
        self.rate = i32::from(value & 15);
        self.volume = if self.disable { self.rate } else { self.decay };
    }

    fn clock(&mut self) {
        if self.reset {
            self.reset = false;
            self.counter = self.rate + 1;
            self.decay = 15;
        } else {
            self.counter -= 1;
            if self.counter <= 0 {
                self.counter = self.rate + 1;
                if self.decay > 0 {
                    self.decay -= 1;
                } else if self.looping {
                    self.decay = 15;
                }
            }
        }
        self.volume = if self.disable { self.rate } else { self.decay };
    }
}

#[derive(Clone, Default)]
struct Pulse {
    first: bool,
    enabled: bool,
    length_enable: bool,
    sweep_active: bool,
    sweep_update: bool,
    timer: i32,
    period: i32,
    length: i32,
    position: i32,
    sweep_counter: i32,
    sweep_period: i32,
    sweep_mode: i32,
    sweep_shift: i32,
    duty: i32,
    sample: i32,
    envelope: Envelope,
}

impl Pulse {
    fn update(&mut self) {
        self.sample = if self.enabled
            && self.length > 0
            && self.period > 7
            && !(self.sweep_mode == 0 && self.period + (self.period >> self.sweep_shift) > 0x7ff)
        {
            self.envelope.volume * DUTY[((self.duty << 3) + self.position) as usize]
        } else {
            0
        };
    }

    fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.length = 0;
        }
        self.update();
    }

    fn write(&mut self, reg: u8, value: u8) {
        let value = i32::from(value);
        match reg {
            0 => {
                self.envelope.write(value as u8);
                self.duty = (value >> 6) & 3;
                self.length_enable = value & 32 == 0;
                self.update();
            }
            1 => {
                self.sweep_active = value & 128 != 0;
                self.sweep_period = (value >> 4) & 7;
                self.sweep_mode = (value >> 3) & 1;
                self.sweep_shift = value & 7;
                self.sweep_update = true;
            }
            2 => self.period = (self.period & 0x700) | value,
            3 => {
                self.period = (self.period & 255) | ((value & 7) << 8);
                if self.enabled {
                    self.length = LENGTH[((value & 248) >> 3) as usize];
                }
                self.envelope.reset = true;
            }
            _ => unreachable!(),
        }
    }

    fn half(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.update();
            }
        }
        self.sweep_counter -= 1;
        if self.sweep_counter <= 0 {
            self.sweep_counter = self.sweep_period + 1;
            if self.sweep_active && self.sweep_shift > 0 && self.period > 7 {
                if self.sweep_mode == 0 {
                    self.period += self.period >> self.sweep_shift;
                    if self.period > 0x7ff {
                        self.period = 4095;
                    }
                } else {
                    self.period -= (self.period >> self.sweep_shift) + i32::from(self.first);
                }
            }
        }
        if self.sweep_update {
            self.sweep_update = false;
            self.sweep_counter = self.sweep_period + 1;
        }
    }

    fn tick(&mut self) {
        self.timer -= 1;
        if self.timer <= 0 {
            self.timer += (self.period + 1) << 1;
            self.position = (self.position + 1) & 7;
            self.update();
        }
    }
}

#[derive(Clone)]
struct Triangle {
    timer: i32,
    period: i32,
    position: i32,
    length: i32,
    linear: i32,
    reload: i32,
    sample: i32,
    enabled: bool,
    condition: bool,
    length_enable: bool,
    halt: bool,
    control: bool,
}

impl Default for Triangle {
    fn default() -> Self {
        Self {
            timer: 0,
            period: 0,
            position: 0,
            length: 0,
            linear: 0,
            reload: 0,
            sample: 15,
            enabled: false,
            condition: false,
            length_enable: false,
            halt: true,
            control: false,
        }
    }
}

impl Triangle {
    fn update(&mut self) {
        self.condition = self.enabled && self.period > 7 && self.linear > 0 && self.length > 0;
    }

    fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.length = 0;
        }
        self.update();
    }

    fn write(&mut self, reg: u8, value: u8) {
        let value = i32::from(value);
        match reg {
            8 => {
                self.control = value & 128 != 0;
                self.reload = value & 127;
                self.length_enable = !self.control;
            }
            10 => self.period = (self.period & 0x700) | value,
            11 => {
                self.period = (self.period & 255) | ((value & 7) << 8);
                if self.enabled {
                    self.length = LENGTH[((value & 248) >> 3) as usize];
                }
                self.halt = true;
            }
            _ => return,
        }
        self.update();
    }

    fn quarter(&mut self) {
        if self.halt {
            self.linear = self.reload;
        } else if self.linear > 0 {
            self.linear -= 1;
        }
        self.update();
        if !self.control {
            self.halt = false;
        }
    }

    fn half(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.update();
            }
        }
    }

    fn tick(&mut self) {
        if self.period > 0 {
            self.timer -= 1;
            while self.timer <= 0 {
                self.timer += self.period + 1;
                if self.linear > 0 && self.length > 0 {
                    self.position = (self.position + 1) & 31;
                    if self.enabled {
                        self.sample = if self.position >= 16 {
                            self.position & 15
                        } else {
                            15 - (self.position & 15)
                        } << 4;
                    }
                }
            }
        }
    }
}

#[derive(Clone)]
struct Noise {
    timer: i32,
    period: i32,
    length: i32,
    shift: u32,
    random: i32,
    mode: bool,
    sample: i32,
    accumulated: i32,
    count: i32,
    enabled: bool,
    length_enable: bool,
    envelope: Envelope,
}

impl Default for Noise {
    fn default() -> Self {
        Self {
            timer: 0,
            period: 0,
            length: 0,
            shift: 1,
            random: 0,
            mode: false,
            sample: 0,
            accumulated: 0,
            count: 1,
            enabled: false,
            length_enable: false,
            envelope: Envelope::default(),
        }
    }
}

impl Noise {
    fn update(&mut self) {
        if self.enabled && self.length > 0 {
            self.sample = self.random * self.envelope.volume;
        }
    }

    fn enable(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            self.length = 0;
        }
        self.update();
    }

    fn write(&mut self, reg: u8, value: u8, pal: bool) {
        match reg {
            12 => {
                self.envelope.write(value);
                self.length_enable = value & 32 == 0;
            }
            14 => {
                self.period = if pal { NOISE_PAL } else { NOISE_NTSC }[usize::from(value & 15)];
                self.mode = value & 128 != 0;
            }
            15 => {
                if self.enabled {
                    self.length = LENGTH[usize::from(value >> 3)];
                }
                self.envelope.reset = true;
            }
            _ => {}
        }
    }

    fn half(&mut self) {
        if self.length_enable && self.length > 0 {
            self.length -= 1;
            if self.length == 0 {
                self.update();
            }
        }
    }

    fn tick(&mut self) {
        if self.timer - 1 > 0 {
            self.timer -= 1;
        } else {
            self.timer -= 1;
            if self.timer <= 0 && self.period > 0 {
                self.shift = self.shift.wrapping_shl(1);
                let bit = ((self.shift << if self.mode { 6 } else { 1 }) ^ self.shift) & 0x8000;
                if bit != 0 {
                    self.shift |= 1;
                    self.random = 0;
                    self.sample = 0;
                } else {
                    self.random = 1;
                    self.sample = if self.enabled && self.length > 0 {
                        self.envelope.volume
                    } else {
                        0
                    };
                }
                self.timer += self.period;
            }
        }
        self.accumulated += self.sample;
        self.count += 1;
    }
}

#[derive(Clone, Default)]
struct Dmc {
    enabled: bool,
    has_sample: bool,
    irq: bool,
    mode: i32,
    frequency: i32,
    dma_counter: i32,
    delta: i32,
    start: usize,
    address: usize,
    length: i32,
    remaining: i32,
    sample: i32,
    lsb: i32,
    shift_counter: i32,
    data: u8,
}

impl Dmc {
    fn next(&mut self, memory: &[u8]) {
        self.data = memory[self.address & 0xffff];
        self.remaining -= 1;
        self.address += 1;
        if self.address > 0xffff {
            self.address = 0x8000;
        }
        self.has_sample = true;
    }
}

/// Nintendo NES APU with two pulse channels, triangle, noise and DMC.
///
/// Registers are offsets from CPU address `$4000`. Sample RAM uses CPU
/// addresses, and is snapshotted along with the channel state. VGM playback
/// uses the header CPU clock and loads DMC RAM upload blocks into sample RAM.
/// With the `nes_fds` feature, `NesApu::with_fds` enables FDS expansion audio;
/// its analog mixing and filters are approximations.
#[derive(Clone)]
pub struct NesApu {
    clock: u32,
    #[cfg(feature = "nes_fds")]
    fds: Option<super::nes_fds::Fds>,
    pal: bool,
    pulse: [Pulse; 2],
    triangle: Triangle,
    noise: Noise,
    dmc: Dmc,
    memory: Box<[u8; 65536]>,
    frame_counter: i32,
    frame_step: usize,
    sequence: bool,
    parity: i32,
    sample_timer: i32,
    sample_timer_max: i32,
    count: i32,
    pulse_sum: [i32; 2],
    triangle_sum: i32,
    dmc_sum: i32,
    triangle_value: i32,
    square_table: Box<[i32; 512]>,
    tnd_table: Box<[i32; 3264]>,
    dc_value: f64,
    previous_mix: f64,
    filter_acc: f64,
    previous_source: f64,
    filtered: f64,
    dc_coefficient: f64,
}

impl NesApu {
    /// Create an APU using its CPU clock in hertz.
    ///
    /// Clocks below 1.7 MHz use PAL frame-counter and noise/DMC timings;
    /// higher clocks use NTSC timings.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < RATE {
            return Err(ChipError::InvalidClock {
                clock,
                minimum: RATE,
            });
        }
        let mut square_table = Box::new([0; 512]);
        let mut tnd_table = Box::new([0; 3264]);
        for (i, value) in square_table.iter_mut().enumerate().skip(1) {
            *value =
                (95.52 / (8128.0 / (i as f64 / 16.0) + 100.0) * 0.98411 * 50000.0).floor() as i32;
        }
        for (i, value) in tnd_table.iter_mut().enumerate().skip(1) {
            *value =
                (163.67 / (24329.0 / (i as f64 / 16.0) + 100.0) * 0.98411 * 50000.0).floor() as i32;
        }
        let dc_value = f64::from(square_table[511] + tnd_table[3263]) / 2.0;
        let mut apu = Self {
            clock,
            #[cfg(feature = "nes_fds")]
            fds: None,
            pal: clock < 1_700_000,
            pulse: [
                Pulse {
                    first: true,
                    ..Pulse::default()
                },
                Pulse::default(),
            ],
            triangle: Triangle::default(),
            noise: Noise::default(),
            dmc: Dmc::default(),
            memory: Box::new([0; 65536]),
            frame_counter: 0,
            frame_step: 0,
            sequence: false,
            parity: 0,
            sample_timer: 0,
            sample_timer_max: (u64::from(clock) * 1024 / u64::from(RATE)) as i32,
            count: 0,
            pulse_sum: [0; 2],
            triangle_sum: 0,
            dmc_sum: 0,
            triangle_value: 0,
            square_table,
            tnd_table,
            dc_value,
            previous_mix: -dc_value,
            filter_acc: 0.0,
            previous_source: 0.0,
            filtered: 0.0,
            dc_coefficient: (-2.0 * std::f64::consts::PI * 10.0 / f64::from(RATE)).exp(),
        };
        for register in 0..0x14 {
            apu.write_register(register, if register == 0x10 { 0x10 } else { 0 })?;
        }
        apu.write_register(0x15, 0)?;
        Ok(apu)
    }

    /// Create an APU with FDS expansion audio enabled.
    #[cfg(feature = "nes_fds")]
    pub fn with_fds(clock: u32) -> Result<Self, ChipError> {
        let mut apu = Self::new(clock)?;
        apu.fds = Some(super::nes_fds::Fds::new(RATE));
        Ok(apu)
    }

    fn write_register(&mut self, register: u8, value: u8) -> Result<(), ChipError> {
        match register {
            0x00..=0x03 => self.pulse[0].write(register, value),
            0x04..=0x07 => self.pulse[1].write(register - 4, value),
            0x08..=0x0b => self.triangle.write(register, value),
            0x0c..=0x0f => self.noise.write(register, value, self.pal),
            0x10..=0x13 => self.dmc.write(register, value, self.pal, &self.memory[..]),
            0x15 => {
                self.pulse[0].enable(value & 1 != 0);
                self.pulse[1].enable(value & 2 != 0);
                self.triangle.enable(value & 4 != 0);
                self.noise.enable(value & 8 != 0);
                self.dmc.enabled = value & 16 != 0;
                self.dmc.write(register, value, self.pal, &self.memory[..]);
            }
            0x17 => {
                self.sequence = value & 128 != 0;
                self.frame_counter = -7 + ((self.parity + 1) & 1);
                self.frame_step = 0;
                if self.sequence {
                    self.quarter();
                    self.half();
                }
            }
            0x14 | 0x16 => {}
            #[cfg(feature = "nes_fds")]
            0x23 | 0x40..=0x7f | 0x80..=0x8a if self.fds.is_some() => {
                self.fds
                    .as_mut()
                    .expect("FDS is enabled")
                    .write(register, value);
            }
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: Chip::NesApu,
                    register,
                });
            }
        }
        Ok(())
    }

    fn quarter(&mut self) {
        for pulse in &mut self.pulse {
            pulse.envelope.clock();
            pulse.update();
        }
        self.noise.envelope.clock();
        self.noise.update();
        self.triangle.quarter();
    }

    fn half(&mut self) {
        self.triangle.half();
        self.pulse[0].half();
        self.pulse[1].half();
        self.noise.half();
    }

    fn advance_frame(&mut self) {
        self.frame_counter += 1;
        let (steps, period) = match (self.pal, self.sequence) {
            (false, false) => ([7457, 14913, 22371, 29828, 29829], 29830),
            (false, true) => ([7457, 14913, 22371, 29829, 37281], 37282),
            (true, false) => ([8313, 16627, 24939, 33252, 33253], 33254),
            (true, true) => ([8313, 16627, 24939, 33253, 41565], 41566),
        };
        while self.frame_step < 5 && self.frame_counter >= steps[self.frame_step] {
            let step = self.frame_step;
            if matches!(step, 0..=2) || step == 4 {
                self.quarter();
                if step == 1 || step == 4 {
                    self.half();
                }
            }
            self.frame_step += 1;
        }
        if self.frame_step >= 5 && self.frame_counter >= period {
            self.frame_step = 0;
            self.frame_counter -= period;
        }
    }

    fn tick(&mut self) {
        #[cfg(feature = "nes_fds")]
        if let Some(fds) = &mut self.fds {
            fds.tick();
        }
        self.parity = (self.parity + 1) & 1;
        if self.dmc.enabled {
            self.dmc.shift_counter -= 8;
            while self.dmc.shift_counter <= 0 && self.dmc.frequency > 0 {
                self.dmc.shift_counter += self.dmc.frequency;
                self.dmc.clock(&self.memory[..]);
            }
        }
        self.triangle.tick();
        self.pulse[0].tick();
        self.pulse[1].tick();
        self.noise.tick();
        self.advance_frame();
        if self.triangle.condition {
            self.triangle_value = (self.triangle.timer << 4) / (self.triangle.period + 1);
            self.triangle_value = self.triangle_value.min(16);
            if self.triangle.position >= 16 {
                self.triangle_value = 16 - self.triangle_value;
            }
            self.triangle_value += self.triangle.sample;
        }
        self.triangle_sum += self.triangle_value;
        self.dmc_sum += self.dmc.sample;
        self.pulse_sum[0] += self.pulse[0].sample;
        self.pulse_sum[1] += self.pulse[1].sample;
        self.count += 1;
        self.sample_timer += 1024;
    }

    fn sample(&mut self) -> i32 {
        while self.sample_timer < self.sample_timer_max {
            self.tick();
        }
        self.sample_timer -= self.sample_timer_max;
        let pulse = if self.count > 0 {
            [
                (self.pulse_sum[0] << 4) / self.count,
                (self.pulse_sum[1] << 4) / self.count,
            ]
        } else {
            [self.pulse[0].sample << 4, self.pulse[1].sample << 4]
        };
        let triangle = if self.count > 0 {
            self.triangle_sum / self.count
        } else {
            self.triangle.sample
        };
        let dmc = if self.count > 0 {
            (self.dmc_sum << 4) / self.count
        } else {
            self.dmc.sample << 4
        };
        let noise = (self.noise.accumulated << 4) / self.noise.count;
        self.noise.accumulated = noise >> 4;
        self.noise.count = 1;
        self.count = 0;
        self.pulse_sum = [0; 2];
        self.triangle_sum = 0;
        self.dmc_sum = 0;
        let square = ((pulse[0] + pulse[1]) >> 1).clamp(0, 511) as usize;
        let tnd = ((3 * triangle + 2 * noise + dmc) >> 1).clamp(0, 3263) as usize;
        let mixed = f64::from(self.square_table[square] + self.tnd_table[tnd]) - self.dc_value;
        let difference = mixed - self.previous_mix;
        self.previous_mix += difference;
        self.filter_acc += difference - f64::from((self.filter_acc as i32) >> 10);
        let source = self.filter_acc / 32768.0;
        #[cfg(feature = "nes_fds")]
        let source = source + self.fds.as_mut().map_or(0.0, |fds| fds.sample());
        let source = source as f32 as f64;
        self.filtered = self.dc_coefficient * (self.filtered + source - self.previous_source);
        self.previous_source = source;
        ((self.filtered as f32) * 32768.0) as i32
    }
}

impl SoundChip for NesApu {
    fn chip_type(&self) -> &Chip {
        &Chip::NesApu
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        RATE
    }
    fn channels(&self) -> usize {
        1
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        match write {
            ChipRegisterWrite::NesApu(spec) => self.write_register(spec.register, spec.value),
            other => Err(ChipError::ChipMismatch {
                expected: other.chip(),
                actual: Chip::NesApu,
            }),
        }
    }

    fn write_ram(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        let end = start
            .checked_add(data.len())
            .filter(|end| *end <= self.memory.len())
            .ok_or(ChipError::RamOutOfRange {
                chip: Chip::NesApu,
                start,
                length: data.len(),
            })?;
        self.memory[start..end].copy_from_slice(data);
        Ok(())
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        for sample in buffer {
            *sample = self.sample();
        }
        Ok(())
    }

    fn reset(&mut self) {
        #[cfg(feature = "nes_fds")]
        if self.fds.is_some() {
            *self = Self::with_fds(self.clock).expect("existing clock is valid");
            return;
        }
        *self = Self::new(self.clock).expect("existing clock is valid");
    }

    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::NesApu)
    }
}
impl Dmc {
    fn clock(&mut self, memory: &[u8]) {
        if self.has_sample {
            if self.data & 1 == 0 {
                if self.delta > 0 {
                    self.delta -= 1;
                }
            } else if self.delta < 63 {
                self.delta += 1;
            }
            self.sample = if self.enabled {
                (self.delta << 1) + self.lsb
            } else {
                0
            };
            self.data >>= 1;
        }
        self.dma_counter -= 1;
        if self.dma_counter <= 0 {
            self.has_sample = false;
            if self.remaining == 0 && self.mode == 1 {
                self.address = self.start;
                self.remaining = self.length;
            }
            if self.remaining > 0 {
                self.next(memory);
                if self.remaining == 0 && self.mode == 2 {
                    self.irq = true;
                }
            }
            self.dma_counter = 8;
        }
    }

    fn write(&mut self, reg: u8, value: u8, pal: bool, memory: &[u8]) {
        match reg {
            0x10 => {
                self.mode = match value >> 6 {
                    1 | 3 => 1,
                    2 => 2,
                    _ => 0,
                };
                if value & 128 == 0 {
                    self.irq = false;
                }
                self.frequency = if pal { DMC_PAL } else { DMC_NTSC }[usize::from(value & 15)];
            }
            0x11 => {
                self.delta = i32::from((value >> 1) & 63);
                self.lsb = i32::from(value & 1);
                self.sample = (self.delta << 1) + self.lsb;
            }
            0x12 => self.start = (usize::from(value) << 6) | 0xc000,
            0x13 => self.length = (i32::from(value) << 4) + 1,
            0x15 => {
                self.irq = false;
                if value & 16 == 0 {
                    self.remaining = 0;
                } else if self.remaining == 0 {
                    self.address = self.start;
                    self.remaining = self.length;
                    if !self.has_sample && self.remaining > 0 {
                        self.next(memory);
                        self.dma_counter = 8;
                        self.shift_counter = self.frequency;
                        if self.remaining == 0 && self.mode == 2 {
                            self.irq = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

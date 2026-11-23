//! NES FDS audio based on the fixNES-derived Tetorica native port.
//!
//! fixNES revision `156fcaca9f4cd9c23d423737994c45cfb05d16ca`.
//! fixNES: Copyright (c) 2017 FIX94.
//! Tetorica: Copyright (c) 2026, kyorohiro.
//! Apache-2.0 AND MIT; see `THIRD_PARTY_NOTICES.md`.

#[derive(Clone)]
pub(super) struct Fds {
    wave: [u8; 64],
    modulation: [u8; 32],
    mod_index: usize,
    current_wave: u8,
    env_speed: i32,
    volume_speed: i32,
    sweep_speed: i32,
    volume_gain: i32,
    sweep_gain: i32,
    volume_up: bool,
    sweep_up: bool,
    enabled: bool,
    volume_enabled: bool,
    envelope_enabled: bool,
    sweep_enabled: bool,
    modulation_enabled: bool,
    modulation_force: bool,
    wave_write_enabled: bool,
    io_enabled: bool,
    master: usize,
    ticks: u8,
    mod_counter: i32,
    frequency: i32,
    mod_frequency: i32,
    accumulator: i32,
    mod_clock: i32,
    output: i32,
    volume_period: i32,
    sweep_period: i32,
    volume_timer: i32,
    sweep_timer: i32,
    low: f64,
    dc: f64,
    low_k: f64,
    dc_k: f64,
}

impl Fds {
    pub(super) fn new(rate: u32) -> Self {
        let period = 8 * (0xe8 + 1);
        Self {
            wave: [0; 64],
            modulation: [0; 32],
            mod_index: 0,
            current_wave: 0,
            env_speed: 0xe8,
            volume_speed: 0,
            sweep_speed: 0,
            volume_gain: 0,
            sweep_gain: 0,
            volume_up: false,
            sweep_up: false,
            enabled: false,
            volume_enabled: false,
            envelope_enabled: false,
            sweep_enabled: false,
            modulation_enabled: false,
            modulation_force: false,
            wave_write_enabled: false,
            io_enabled: true,
            master: 0,
            ticks: 0,
            mod_counter: 0,
            frequency: 0,
            mod_frequency: 0,
            accumulator: 0,
            mod_clock: 0,
            output: 0,
            volume_period: period,
            sweep_period: period,
            volume_timer: period,
            sweep_timer: period,
            low: 0.0,
            dc: 0.0,
            low_k: (-2.0 * std::f64::consts::PI * 2000.0 / f64::from(rate)).exp(),
            dc_k: (-2.0 * std::f64::consts::PI * 20.0 / f64::from(rate)).exp(),
        }
    }

    fn rates(&mut self) {
        self.volume_period = 8 * (self.env_speed + 1) * (self.volume_speed + 1);
        self.sweep_period = 8 * (self.env_speed + 1) * (self.sweep_speed + 1);
    }

    fn signed7(value: i32) -> i32 {
        ((value & 127) ^ 64) - 64
    }

    pub(super) fn write(&mut self, register: u8, value: u8) {
        if register == 0x23 {
            self.io_enabled = value & 2 != 0;
            return;
        }
        if !self.io_enabled {
            return;
        }
        if (0x40..=0x7f).contains(&register) {
            if self.wave_write_enabled {
                self.wave[usize::from(register & 63)] = value;
            }
            return;
        }
        let value = i32::from(value);
        match register {
            0x80 => {
                self.volume_enabled = value & 128 == 0;
                self.volume_up = value & 64 != 0;
                self.volume_speed = value & 63;
                if !self.volume_enabled {
                    self.volume_gain = value & 63;
                }
                self.rates();
                self.volume_timer = self.volume_period;
            }
            0x82 => self.frequency = (self.frequency & 0xf00) | value,
            0x83 => {
                self.enabled = value & 128 == 0;
                self.frequency = (self.frequency & 255) | ((value & 15) << 8);
                if !self.enabled {
                    self.accumulator = 0;
                    self.current_wave = self.wave[0];
                }
                self.envelope_enabled = value & 64 == 0;
                if !self.envelope_enabled {
                    self.volume_timer = self.volume_period;
                    self.sweep_timer = self.sweep_period;
                }
            }
            0x84 => {
                self.sweep_enabled = value & 128 == 0;
                self.sweep_up = value & 64 != 0;
                self.sweep_speed = value & 63;
                if !self.sweep_enabled {
                    self.sweep_gain = value & 63;
                }
                self.rates();
                self.sweep_timer = self.sweep_period;
            }
            0x85 => self.mod_counter = Self::signed7(value),
            0x86 => self.mod_frequency = (self.mod_frequency & 0xf00) | value,
            0x87 => {
                self.modulation_enabled = value & 128 == 0;
                self.modulation_force = value & 64 != 0;
                self.mod_frequency = (self.mod_frequency & 255) | ((value & 15) << 8);
                if !self.modulation_enabled {
                    self.mod_clock = 0;
                    self.mod_index &= 62;
                }
            }
            0x88 => {
                if !self.modulation_enabled {
                    self.modulation[self.mod_index >> 1] = (value & 7) as u8;
                    self.mod_index = (self.mod_index + 2) & 63;
                }
            }
            0x89 => {
                self.wave_write_enabled = value & 128 != 0;
                self.master = (value & 3) as usize;
            }
            0x8a => {
                self.env_speed = value;
                self.rates();
            }
            _ => {}
        }
    }

    fn envelope(&mut self) {
        if self.volume_enabled {
            if self.volume_timer != 0 {
                self.volume_timer -= 1;
            } else {
                self.volume_timer = self.volume_period;
                if self.volume_up {
                    if self.volume_gain < 32 {
                        self.volume_gain += 1;
                    }
                } else if self.volume_gain > 0 {
                    self.volume_gain -= 1;
                }
            }
        }
        if self.sweep_enabled {
            if self.sweep_timer != 0 {
                self.sweep_timer -= 1;
            } else {
                self.sweep_timer = self.sweep_period;
                if self.sweep_up {
                    if self.sweep_gain < 32 {
                        self.sweep_gain += 1;
                    }
                } else if self.sweep_gain > 0 {
                    self.sweep_gain -= 1;
                }
            }
        }
    }

    pub(super) fn tick(&mut self) {
        if self.envelope_enabled && self.env_speed != 0 {
            self.envelope();
            if !self.enabled {
                self.envelope();
                self.envelope();
                self.envelope();
            }
        }
        if !self.enabled {
            self.ticks = 0;
        } else if self.ticks & 15 == 0 {
            if self.modulation_enabled {
                self.mod_clock = (self.mod_clock + self.mod_frequency) & 65535;
                if self.mod_clock & 0xf000 != 0 || self.modulation_force {
                    self.mod_clock &= 4095;
                    let entry = self.modulation[self.mod_index >> 1];
                    let steps = [0, 1, 2, 4, 0, -4, -2, -1];
                    self.mod_counter = if entry == 4 {
                        0
                    } else {
                        Self::signed7(self.mod_counter + steps[usize::from(entry)])
                    };
                    self.mod_index = (self.mod_index + 1) & 63;
                }
            }
            let mut product = self.mod_counter * self.sweep_gain;
            if product & 15 != 0 && product & 0x800 == 0 {
                product += 32;
            }
            let multiplier = ((product + 1024) >> 4) & 255;
            self.accumulator = (self.accumulator + self.frequency * multiplier) & 0xffffff;
            self.current_wave = self.wave[(self.accumulator as usize) >> 18];
        }
        self.ticks = self.ticks.wrapping_add(1);
        if !self.wave_write_enabled {
            let mut volume = i32::from(self.current_wave & 63) * self.volume_gain.min(32);
            if self.master != 0 {
                let gains = [1.0, 20.0 / 30.0, 15.0 / 30.0, 12.0 / 30.0];
                volume = (f64::from(volume) * gains[self.master]).floor() as i32;
            }
            self.output = (volume >> 5) & 63;
        }
    }

    pub(super) fn sample(&mut self) -> f64 {
        let value = f64::from(self.output) / 63.0 * 0.3;
        self.low = self.low_k * self.low + (1.0 - self.low_k) * value;
        self.dc = self.dc_k * self.dc + (1.0 - self.dc_k) * self.low;
        self.low - self.dc
    }
}

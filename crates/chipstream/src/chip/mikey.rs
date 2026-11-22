//! Based on libvgm's MIT-licensed Mikey implementation, revision
//! `c8b998b606895990c409a512b86c5509070f9f0d`.
//! Copyright (c) laoo. C port by Valley Bell.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip};
use soundlog::chip::Chip;

const NEVER: u64 = u64::MAX;
const SLOTS: [u64; 8] = [1, 3, 5, 7, 8, 9, 10, 11];

#[derive(Clone, Copy)]
struct Timer {
    clear_at: u64,
    pending: u32,
    backup: u8,
    value: u8,
    control: u8,
    status: u8,
    done: bool,
}

impl Default for Timer {
    fn default() -> Self {
        Self {
            clear_at: NEVER,
            pending: 0,
            backup: 0,
            value: 0,
            control: 0,
            status: 0,
            done: false,
        }
    }
}

impl Timer {
    fn linked(self) -> bool {
        self.control & 7 == 7
    }
    fn enabled(self) -> bool {
        self.control & 8 != 0
    }
    fn reload(self) -> bool {
        self.control & 16 != 0
    }
    fn next(self, tick: u64) -> u64 {
        if self.enabled() && !self.linked() {
            let period = 1u64 << (u32::from(self.control & 7) + 4);
            (tick & !(period - 1)).saturating_add(period)
        } else {
            NEVER
        }
    }
    fn clear(&mut self, tick: u64) {
        if tick >= self.clear_at {
            self.status &= !3;
            self.clear_at = NEVER;
        }
    }
    fn schedule_clear(&mut self, tick: u64) {
        self.clear_at = if self.status & 3 != 0 { tick } else { NEVER };
    }
    fn borrow(&mut self) -> bool {
        if self.value > 0 {
            self.value -= 1;
            if !self.linked() {
                self.status &= !4;
            }
            false
        } else {
            self.status |= 9;
            if !self.linked() {
                self.status |= 4;
            }
            self.done = true;
            if self.reload() {
                self.value = self.backup;
            }
            true
        }
    }
    fn clock(&mut self) -> bool {
        self.status &= !3;
        if self.control & 0x40 != 0 {
            self.status &= !8;
            self.done = false;
        }
        if !self.reload() && self.done {
            return false;
        }
        self.status |= 2;
        self.borrow()
    }
    fn set_control(&mut self, value: u8) {
        let old = self.control;
        self.control = value;
        if old & 7 != value & 7 || old & 8 == 0 && value & 8 != 0 {
            self.pending = 0;
        }
        if value & 0x40 != 0 {
            self.done = false;
            self.status &= !8;
        }
    }
    fn set_status(&mut self, tick: u64, value: u8) -> bool {
        self.clear(tick);
        let borrow = self.status & 2 == 0 && value & 2 != 0 && self.borrow();
        self.status = value & 8;
        self.done = value & 8 != 0;
        self.clear_at = NEVER;
        borrow
    }
}

#[derive(Clone, Copy, Default)]
struct Channel {
    shift: u16,
    taps: u16,
    volume: i8,
    output: i8,
    feedback: u8,
    control: u8,
}

impl Channel {
    fn trigger(&mut self) {
        let parity = (self.taps & self.shift).count_ones() & 1;
        self.shift = ((self.shift << 1) & 0xffe) | (parity as u16 ^ 1);
        let delta = if self.shift & 1 != 0 {
            i32::from(self.volume)
        } else {
            -i32::from(self.volume)
        };
        let output = if self.control & 0x20 != 0 {
            i32::from(self.output) + delta
        } else {
            delta
        };
        self.output = output.clamp(-128, 127) as i8;
    }
}

/// Atari Lynx Mikey sound and linked-timer engine.
///
/// Registers use the chip's native addresses: audio channels occupy
/// `0x20..=0x3f`, and stereo control uses `0x40..=0x44` and `0x50`.
#[derive(Clone)]
pub struct Mikey {
    clock: u32,
    timers: [Timer; 8],
    channels: [Channel; 4],
    actions: [u64; 8],
    attenuation: [u8; 4],
    pan: u8,
    stereo: u8,
    tick: u64,
    next_tick: u64,
    remainder: u32,
}

impl Mikey {
    /// Create a Mikey using its input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock < 16 {
            return Err(ChipError::InvalidClock { clock, minimum: 16 });
        }
        let rate = clock / 16;
        Ok(Self {
            clock,
            timers: [Timer::default(); 8],
            channels: [Channel::default(); 4],
            actions: [NEVER; 8],
            attenuation: [0; 4],
            pan: 0,
            stereo: 0,
            tick: 0,
            next_tick: u64::from(16_000_000 / rate),
            remainder: 16_000_000 % rate,
        })
    }

    fn service_tick(stage: usize, tick: u64) -> u64 {
        let candidate = (tick & !15).saturating_add(SLOTS[stage]);
        if candidate <= tick {
            candidate.saturating_add(16)
        } else {
            candidate
        }
    }

    fn schedule_link(&mut self, stage: usize, tick: u64) {
        let next = (stage + 1) & 7;
        let timer = &mut self.timers[next];
        if timer.linked() {
            timer.clear(tick);
            timer.pending = timer.pending.saturating_add(1);
            timer.status |= 2;
            let at = Self::service_tick(next, tick);
            timer.schedule_clear(at);
            self.actions[next] = self.actions[next].min(at);
        }
    }

    fn fire(&mut self, stage: usize, tick: u64) {
        let timer = &mut self.timers[stage];
        if timer.linked() {
            timer.status &= !3;
            if timer.enabled() && timer.control & 0x40 != 0 {
                timer.status &= !8;
                timer.done = false;
            }
            let mut clocks = if timer.enabled() && (timer.reload() || !timer.done) {
                std::mem::take(&mut timer.pending)
            } else {
                0
            };
            self.actions[stage] = NEVER;
            while clocks > 0 {
                clocks -= 1;
                self.timers[stage].status |= 2;
                if self.timers[stage].borrow() {
                    if stage >= 4 {
                        self.channels[stage - 4].trigger();
                    }
                    self.schedule_link(stage, tick);
                    if !self.timers[stage].reload() {
                        break;
                    }
                }
            }
            self.timers[stage].schedule_clear(tick.saturating_add(16));
        } else {
            let borrowed = timer.clock();
            timer.schedule_clear(Self::service_tick(stage, tick));
            self.actions[stage] = timer.next(tick);
            if borrowed {
                if stage >= 4 {
                    self.channels[stage - 4].trigger();
                }
                self.schedule_link(stage, tick);
            }
        }
    }

    fn write_register(&mut self, address: u8, value: u8) -> Result<(), ChipError> {
        let (stage, reg) = if address < 0x20 {
            let timer = (address >> 2) & 7;
            if timer & 1 == 0 {
                return Ok(());
            }
            (usize::from(timer >> 1), address & 3)
        } else if address < 0x40 {
            let channel = usize::from((address - 0x20) >> 3);
            let c = &mut self.channels[channel];
            match address & 7 {
                0 => {
                    c.volume = value as i8;
                    return Ok(());
                }
                1 => {
                    c.feedback = value;
                    c.taps =
                        (c.taps & 0x3c0) | u16::from(value & 0x3f) | (u16::from(value & 0xc0) << 4);
                    return Ok(());
                }
                2 => {
                    c.output = value as i8;
                    return Ok(());
                }
                3 => {
                    c.shift = (c.shift & 0xf00) | u16::from(value);
                    return Ok(());
                }
                4 => (channel + 4, 0),
                5 => {
                    c.control = value;
                    c.taps = (c.taps & 0xf7f) | u16::from(value & 0x80);
                    (channel + 4, 1)
                }
                6 => (channel + 4, 2),
                _ => (channel + 4, 3),
            }
        } else {
            match address {
                0x40..=0x43 => self.attenuation[usize::from(address - 0x40)] = value,
                0x44 => self.pan = value,
                0x50 => self.stereo = value,
                _ => {
                    return Err(ChipError::InvalidRegister {
                        chip: Chip::Mikey,
                        register: address,
                    });
                }
            }
            return Ok(());
        };
        let timer = &mut self.timers[stage];
        let borrow = match reg {
            0 => {
                timer.backup = value;
                false
            }
            1 => {
                timer.set_control(if stage >= 4 { value & !0xa0 } else { value });
                false
            }
            2 => {
                timer.value = value;
                false
            }
            _ => timer.set_status(self.tick, value),
        };
        if borrow && stage >= 4 {
            self.channels[stage - 4].trigger();
        }
        if stage >= 4 && reg == 3 {
            let channel = &mut self.channels[stage - 4];
            channel.shift = (channel.shift & 0xff) | (u16::from(value & 0xf0) << 4);
        }
        self.actions[stage] = if timer.linked() && timer.pending > 0 {
            self.actions[stage].min(Self::service_tick(stage, self.tick))
        } else {
            timer.next(self.tick)
        };
        if borrow {
            self.schedule_link(stage, self.tick);
        }
        Ok(())
    }

    fn output(&self) -> [i32; 2] {
        let mut output = [0; 2];
        for i in 0..4 {
            let left = 0x10 << i;
            let right = 1 << i;
            let volume = i32::from(self.channels[i].output);
            if self.stereo & left == 0 {
                output[0] += volume
                    * if self.pan & left != 0 {
                        i32::from(self.attenuation[i] & 0xf0) >> 2
                    } else {
                        60
                    };
            }
            if self.stereo & right == 0 {
                output[1] += volume
                    * if self.pan & right != 0 {
                        i32::from(self.attenuation[i] & 15) << 2
                    } else {
                        60
                    };
            }
        }
        output
    }
}

impl SoundChip for Mikey {
    fn chip_type(&self) -> &Chip {
        &Chip::Mikey
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 16
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_hz(self.sample_rate())
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        match write {
            ChipRegisterWrite::Mikey(spec) => self.write_register(spec.register, spec.value),
            other => Err(ChipError::ChipMismatch {
                expected: other.chip(),
                actual: Chip::Mikey,
            }),
        }
    }
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if !buffer.len().is_multiple_of(2) {
            return Err(ChipError::InvalidBufferLength {
                length: buffer.len(),
                channels: 2,
            });
        }
        let rate = self.sample_rate();
        for frame in buffer.as_chunks_mut::<2>().0 {
            while let Some((stage, &at)) = self.actions.iter().enumerate().min_by_key(|(_, at)| *at)
            {
                if at > self.tick || at == NEVER {
                    break;
                }
                self.fire(stage, at);
            }
            *frame = self.output();
            self.tick = self.next_tick & !15;
            self.next_tick = self.next_tick.saturating_add(u64::from(16_000_000 / rate));
            let rem = u64::from(self.remainder) + u64::from(16_000_000 % rate);
            self.remainder = (rem % u64::from(rate)) as u32;
            self.next_tick = self.next_tick.saturating_add(rem / u64::from(rate));
        }
        Ok(())
    }
    fn reset(&mut self) {
        *self = Self::new(self.clock).expect("existing clock is valid");
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Mikey)
    }
}

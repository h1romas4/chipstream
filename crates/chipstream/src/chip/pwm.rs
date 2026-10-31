//! Based on MAME's 32X implementation, revision
//! `ee1e4f9683a4953cb9d88f9256017fcbc38e3144`.
//! Copyright (c) David Haywood.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SoundChip};
use soundlog::chip::Chip;

const RATE: u32 = 22_050;

#[derive(Clone, Default)]
struct Fifo {
    data: [u16; 3],
    len: usize,
}

impl Fifo {
    fn push(&mut self, value: u16) {
        if self.len == self.data.len() {
            self.pop();
        }
        self.data[self.len] = value;
        self.len += 1;
    }

    fn pop(&mut self) -> Option<u16> {
        if self.len == 0 {
            return None;
        }
        let value = self.data[0];
        self.data.copy_within(1..self.len, 0);
        self.len -= 1;
        Some(value)
    }
}

/// 32X PWM output, using the reference port's fixed 22,050 Hz native rate.
///
/// `PwmSpec` carries 12-bit writes. Undefined channel routing values are
/// rejected.
#[derive(Clone)]
pub struct Pwm {
    clock: u32,
    control: u16,
    cycle: u16,
    left: Fifo,
    right: Fifo,
    timer_rate: u32,
    phase: u32,
    pending: u32,
    output: [i32; 2],
}

impl Pwm {
    /// Create a PWM chip with empty FIFOs and disabled output.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock == 0 {
            return Err(ChipError::InvalidClock { clock, minimum: 1 });
        }
        Ok(Self {
            clock,
            control: 0,
            cycle: 0,
            left: Fifo::default(),
            right: Fifo::default(),
            timer_rate: 0,
            phase: 0,
            pending: 1,
            output: [0; 2],
        })
    }

    fn update_timer(&mut self) {
        let cycle = if self.cycle == 0 {
            4095
        } else {
            u32::from(self.cycle)
        };
        if cycle == 1 || self.control & 15 == 0 {
            self.timer_rate = 0;
        } else {
            self.left = Fifo::default();
            self.right = Fifo::default();
            self.timer_rate = self.clock / (cycle - 1);
        }
    }

    fn timer_tick(&mut self) {
        let mut output = [0; 2];
        if let Some(value) = self.left.pop() {
            match self.control & 3 {
                1 => output[0] = i32::from(value & 0xfff) * 32,
                2 => output[1] = i32::from(value & 0xfff) * 32,
                _ => {}
            }
        }
        if let Some(value) = self.right.pop() {
            match (self.control >> 2) & 3 {
                1 => output[1] = i32::from(value & 0xfff) * 32,
                2 => output[0] = i32::from(value & 0xfff) * 32,
                _ => {}
            }
        }
        self.output = output;
    }

    fn tick(&mut self) -> [i32; 2] {
        if self.timer_rate != 0 {
            // After four ticks both three-entry FIFOs are empty and output is zero.
            for _ in 0..self.pending.min(4) {
                self.timer_tick();
            }
            self.phase += self.timer_rate % RATE;
            self.pending = self.timer_rate / RATE + self.phase / RATE;
            self.phase %= RATE;
        }
        self.output
    }
}

impl SoundChip for Pwm {
    fn chip_type(&self) -> &Chip {
        &Chip::Pwm
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        RATE
    }
    fn channels(&self) -> usize {
        2
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.parts_for(self.chip_type())?;
        if bank != 0 {
            return Err(ChipError::InvalidRegisterBank {
                chip: Chip::Pwm,
                bank,
            });
        }
        if value > 0xfff || (register == 0 && (value & 3 == 3 || (value >> 2) & 3 == 3)) {
            return Err(ChipError::InvalidRegisterValue {
                chip: Chip::Pwm,
                register,
                value,
            });
        }
        let value = value as u16;
        match register {
            0 => {
                self.control = value;
                self.update_timer();
            }
            1 => {
                self.cycle = value;
                self.update_timer();
            }
            2 => self.left.push(value),
            3 => self.right.push(value),
            4 => {
                self.left.push(value);
                self.right.push(value);
            }
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: Chip::Pwm,
                    register,
                });
            }
        }
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
            frame.copy_from_slice(&self.tick());
        }
        Ok(())
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, super::ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), super::ChipError> {
        let chip = self.chip_type().clone();
        super::restore_cloned_state(self, state, chip)
    }

    fn reset(&mut self) {
        self.control = 0;
        self.cycle = 0;
        self.left = Fifo::default();
        self.right = Fifo::default();
        self.timer_rate = 0;
        self.phase = 0;
        self.pending = 1;
        self.output = [0; 2];
    }
}

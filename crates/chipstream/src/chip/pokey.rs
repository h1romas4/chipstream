//! Based on MAME's POKEY implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Brad Oliver, Eric Smith, and Juergen Buchmueller.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.
//!
//! [`Pokey`] emulates the audio portion of the Atari POKEY.

use super::{ChipError, ChipRegisterWrite, SoundChip};
use soundlog::chip::Chip;

#[derive(Clone, Copy)]
struct Channel {
    frequency: u8,
    control: u8,
    borrow: u8,
    counter: u8,
    output: bool,
    filter: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            frequency: 0,
            control: 0xb0,
            borrow: 0,
            counter: 0,
            output: false,
            filter: false,
        }
    }
}

impl Channel {
    fn reset_timer(&mut self) {
        self.counter = !self.frequency;
        self.borrow = 0;
    }

    fn increment(&mut self, delay: u8) {
        self.counter = self.counter.wrapping_add(1);
        if self.counter == 0 && self.borrow == 0 {
            self.borrow = delay;
        }
    }

    fn borrowed(&mut self) -> bool {
        if self.borrow == 0 {
            return false;
        }
        self.borrow -= 1;
        self.borrow == 0
    }
}

/// Atari POKEY audio generator with four channels and mono output.
///
/// The native sample rate equals the input clock. Audio uses MAME's legacy
/// linear output model; external keyboard, paddle, and serial input are not
/// exposed by this audio-only backend. POKEY has no audio reset pin; `reset`
/// preserves the audio state, as in MAME.
#[derive(Clone)]
pub struct Pokey {
    clock: u32,
    channels: [Channel; 4],
    audio_control: u8,
    serial_control: u8,
    irq_enable: u8,
    irq_status: u8,
    serial_output: u8,
    serial_shift: u16,
    serial_full: bool,
    serial_clock: bool,
    clock28: u8,
    clock114: u8,
    poly4: u32,
    poly5: u32,
    poly9: u32,
    poly17: u32,
}

impl Pokey {
    /// Create a POKEY using the input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock == 0 {
            return Err(ChipError::InvalidClock { clock, minimum: 1 });
        }
        let mut chip = Self {
            clock,
            channels: [Channel::default(); 4],
            audio_control: 0,
            serial_control: 0,
            irq_enable: 0,
            irq_status: 0x08,
            serial_output: 0,
            serial_shift: 1,
            serial_full: false,
            serial_clock: false,
            clock28: 0,
            clock114: 0,
            poly4: 0,
            poly5: 0,
            poly9: 0,
            poly17: 0,
        };
        chip.reset_polynomials();
        Ok(chip)
    }

    fn poly_short(value: u32, bits: u32) -> u32 {
        ((value << 1) | (!(value >> 2 ^ value >> (bits - 1)) & 1)) & ((1 << bits) - 1)
    }

    fn poly9(value: u32) -> u32 {
        (value >> 1) | (((value ^ (value >> 5)) & 1) << 8)
    }

    fn poly17(value: u32) -> u32 {
        ((value >> 1) & 0xff7f) | ((((value >> 8) ^ (value >> 13)) & 1) << 7) | ((value & 1) << 16)
    }

    fn reset_polynomials(&mut self) {
        self.poly4 = Self::poly_short(0, 4);
        self.poly5 = Self::poly_short(0, 5);
        self.poly9 = Self::poly9(0x1ff);
        self.poly17 = Self::poly17(0x1ffff);
        self.clock28 = 0;
        self.clock114 = 0;
    }

    fn write_register(&mut self, register: u8, value: u8) {
        match register & 15 {
            0..=7 => {
                let channel = &mut self.channels[usize::from((register & 7) >> 1)];
                if register & 1 == 0 {
                    channel.frequency = value;
                } else {
                    channel.control = value;
                }
            }
            8 => self.audio_control = value,
            9 => {
                for (index, channel) in self.channels.iter_mut().enumerate() {
                    channel.reset_timer();
                    channel.output = false;
                    channel.filter = index < 2;
                }
            }
            13 => {
                self.serial_output = value;
                if self.serial_control & 3 != 0 {
                    self.serial_full = true;
                }
            }
            14 => {
                self.irq_status &= 0x08 | value;
                self.irq_enable = value;
            }
            15 if value != self.serial_control => {
                self.serial_control = value;
                if value & 3 == 0 {
                    self.irq_enable = 0;
                    self.irq_status = 0x08;
                    self.serial_shift = 1;
                    self.serial_full = false;
                    self.reset_polynomials();
                }
                if value & 0x60 == 0 {
                    self.serial_clock = false;
                }
                if value & 0x10 != 0 {
                    self.channels[2].reset_timer();
                    self.channels[3].reset_timer();
                }
            }
            _ => {}
        }
    }

    fn process_channel(&mut self, index: usize) {
        let channel = &mut self.channels[index];
        if channel.control & 0x80 != 0 || self.poly5 & 1 != 0 {
            channel.output = if channel.control & 0x20 != 0 {
                !channel.output
            } else if channel.control & 0x40 != 0 {
                self.poly4 & 1 != 0
            } else if self.audio_control & 0x80 != 0 {
                self.poly9 & 1 != 0
            } else {
                self.poly17 & 1 != 0
            };
        }
    }

    fn tick(&mut self) -> i32 {
        let mut toggle_serial = false;
        if self.serial_control & 3 != 0 {
            self.poly4 = Self::poly_short(self.poly4, 4);
            self.poly5 = Self::poly_short(self.poly5, 5);
            self.poly9 = Self::poly9(self.poly9);
            self.poly17 = Self::poly17(self.poly17);
            self.clock28 += 1;
            let clock28 = self.clock28 == 28;
            if clock28 {
                self.clock28 = 0;
            }
            self.clock114 += 1;
            let clock114 = self.clock114 == 114;
            if clock114 {
                self.clock114 = 0;
            }
            let base_clock = if self.audio_control & 1 != 0 {
                clock114
            } else {
                clock28
            };
            if self.audio_control & 0x40 != 0 {
                self.channels[0].increment(if self.audio_control & 0x10 != 0 { 7 } else { 4 });
            } else if base_clock {
                self.channels[0].increment(1);
            }
            let async_reset = self.serial_control & 0x10 != 0;
            if !async_reset {
                if self.audio_control & 0x20 != 0 {
                    self.channels[2].increment(if self.audio_control & 8 != 0 { 7 } else { 4 });
                } else if base_clock {
                    self.channels[2].increment(1);
                }
            }
            if base_clock {
                if self.audio_control & 0x10 == 0 {
                    toggle_serial |=
                        self.channels[1].counter == 0xff && self.serial_control & 0x60 == 0x60;
                    self.channels[1].increment(1);
                }
                if self.audio_control & 8 == 0 && !async_reset {
                    toggle_serial |= self.channels[3].counter == 0xff
                        && self.serial_control & 0x60 != 0
                        && self.serial_control & 0x60 != 0x60;
                    self.channels[3].increment(1);
                }
            }
        }
        if self.channels[2].borrowed() {
            if self.audio_control & 8 != 0 {
                toggle_serial |= self.channels[3].counter == 0xff
                    && self.serial_control & 0x60 != 0
                    && self.serial_control & 0x60 != 0x60;
                self.channels[3].increment(1);
            } else {
                self.channels[2].reset_timer();
            }
            self.process_channel(2);
            self.channels[0].filter = if self.audio_control & 4 != 0 {
                self.channels[0].output
            } else {
                true
            };
        }
        if self.channels[3].borrowed() {
            if self.audio_control & 8 != 0 {
                self.channels[2].reset_timer();
            }
            self.channels[3].reset_timer();
            self.process_channel(3);
            self.channels[1].filter = if self.audio_control & 2 != 0 {
                self.channels[1].output
            } else {
                true
            };
        }
        if self.serial_control & 8 != 0 && self.channels[1].borrow == 1 {
            self.channels[0].reset_timer();
        }
        if self.channels[0].borrowed() {
            if self.audio_control & 0x10 != 0 {
                toggle_serial |=
                    self.channels[1].counter == 0xff && self.serial_control & 0x60 == 0x60;
                self.channels[1].increment(1);
            } else {
                self.channels[0].reset_timer();
                let serial_high = if self.irq_status & 8 != 0 {
                    self.serial_control & 0x80 == 0
                } else {
                    self.serial_shift & 1 != 0
                };
                if self.serial_control & 8 != 0 && serial_high {
                    self.channels[1].reset_timer();
                }
            }
            self.process_channel(0);
        }
        if self.channels[1].borrowed() {
            if self.audio_control & 0x10 != 0 {
                self.channels[0].reset_timer();
            }
            self.channels[1].reset_timer();
            self.process_channel(1);
        }
        if toggle_serial && (self.serial_full || self.irq_status & 8 == 0) {
            self.serial_clock = !self.serial_clock;
            if self.serial_clock {
                if self.serial_shift == 1 {
                    if self.serial_full {
                        self.serial_shift = (u16::from(self.serial_output) << 1) | (1 << 9);
                        self.serial_full = false;
                        self.irq_status &= !8;
                        if self.irq_enable & 0x10 != 0 {
                            self.irq_status |= 0x10;
                        }
                    } else {
                        self.irq_status |= 8;
                    }
                } else {
                    self.serial_shift >>= 1;
                }
            }
        }
        let volume: i32 = self
            .channels
            .iter()
            .map(|channel| {
                if channel.output != channel.filter || channel.control & 0x10 != 0 {
                    i32::from(channel.control & 15)
                } else {
                    0
                }
            })
            .sum();
        (volume * (32767 / 11 / 4)).min(32767)
    }
}

impl SoundChip for Pokey {
    fn chip_type(&self) -> &Chip {
        &Chip::Pokey
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.clock
    }

    fn channels(&self) -> usize {
        1
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (_, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(register, value);
        Ok(())
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        for sample in buffer {
            *sample = self.tick();
        }
        Ok(())
    }

    fn reset(&mut self) {}

    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Pokey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polynomial_sequences_match_mame_tables() {
        let expected = [
            "1101100101000011101100101000011101100101000011101100101000011101",
            "1100100010101111011010011000001110010001010111101101001100000111",
            "1111111000011110111000010110011011011110100001110011000010010001",
            "1111110000000000001111100000001111111111001111100000110001111111",
        ];
        let mut chip = Pokey::new(1_789_790).unwrap();
        chip.write_register(15, 3);
        for i in 0..64 {
            chip.tick();
            for (index, value) in [chip.poly4, chip.poly5, chip.poly9, chip.poly17]
                .into_iter()
                .enumerate()
            {
                assert_eq!(b'0' + (value & 1) as u8, expected[index].as_bytes()[i]);
            }
        }
        chip.write_register(15, 0);
        chip.write_register(15, 3);
        chip.tick();
        assert_eq!(
            [
                chip.poly4 & 1,
                chip.poly5 & 1,
                chip.poly9 & 1,
                chip.poly17 & 1
            ],
            [1; 4]
        );
    }
}

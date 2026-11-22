//! Based on MAME's K051649 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Bryan McPhail.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.
//!
//! [`K051649`] emulates the five-channel SCC and SCC+ wavetable generator.

use super::{ChipError, ChipRegisterWrite, SoundChip};
use soundlog::chip::Chip;

#[derive(Clone, Copy)]
struct Channel {
    wave: [i8; 32],
    counter: u8,
    clock: u16,
    frequency: u16,
    volume: u8,
    sample: i16,
    key: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            wave: [0; 32],
            counter: 0,
            clock: 0,
            frequency: 0,
            volume: 15,
            sample: 0,
            key: false,
        }
    }
}

/// Konami K051649 with SCC and SCC+ waveform write ports.
///
/// The native mono sample rate equals the input clock. Port 0 writes the
/// shared SCC waveform RAM; port 4 writes the independent SCC+ waveform RAM.
#[derive(Clone)]
pub struct K051649 {
    clock: u32,
    channels: [Channel; 5],
    test: u8,
}

impl K051649 {
    /// Create a K051649 using the input clock in hertz.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        if clock == 0 {
            return Err(ChipError::InvalidClock { clock, minimum: 1 });
        }
        Ok(Self {
            clock,
            channels: [Channel::default(); 5],
            test: 0,
        })
    }

    fn write_register(&mut self, port: u8, register: u8, value: u8) -> Result<(), ChipError> {
        let invalid_register = || ChipError::InvalidRegister {
            chip: Chip::K051649,
            register,
        };
        match port {
            0 | 4 => {
                if register >= if port == 0 { 0x80 } else { 0xa0 } {
                    return Err(invalid_register());
                }
                if self.test & 0x40 != 0 || (port == 0 && self.test & 0x80 != 0 && register >= 0x60)
                {
                    return Ok(());
                }
                let channel = usize::from(register >> 5);
                let index = usize::from(register & 31);
                self.channels[channel].wave[index] = value as i8;
                if port == 0 && channel == 3 {
                    self.channels[4].wave[index] = value as i8;
                }
            }
            1 => {
                if register >= 10 {
                    return Err(invalid_register());
                }
                let channel = &mut self.channels[usize::from(register / 2)];
                if register & 1 == 0 {
                    channel.frequency = (channel.frequency & 0xf00) | u16::from(value);
                } else {
                    channel.frequency = (channel.frequency & 0xff) | (u16::from(value & 15) << 8);
                }
                if self.test & 0x20 != 0 {
                    channel.counter = 0;
                }
                channel.clock = u16::MAX;
            }
            2 => {
                if register >= 5 {
                    return Err(invalid_register());
                }
                self.channels[usize::from(register)].volume = value & 15;
            }
            3 => {
                for (index, channel) in self.channels.iter_mut().enumerate() {
                    channel.key = value & (1 << index) != 0;
                }
            }
            5 => self.test = value,
            _ => {
                return Err(ChipError::InvalidRegisterBank {
                    chip: Chip::K051649,
                    bank: port,
                });
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> i32 {
        let mut output = 0;
        for channel in &mut self.channels {
            if channel.frequency > 8 {
                channel.clock = channel.clock.wrapping_add(1);
                if channel.clock > channel.frequency {
                    channel.counter = (channel.counter + 1) & 31;
                    channel.clock = 0;
                }
                if channel.clock == 0 {
                    channel.sample = if channel.key {
                        i16::from(channel.wave[usize::from(channel.counter)])
                            * i16::from(channel.volume)
                    } else {
                        0
                    };
                }
            }
            output += i32::from(channel.sample >> 4) * 32;
        }
        output
    }
}

impl SoundChip for K051649 {
    fn chip_type(&self) -> &Chip {
        &Chip::K051649
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
        let (port, register, value) = write.byte_parts_for(self.chip_type())?;
        self.write_register(port, register, value)
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        for sample in buffer {
            *sample = self.tick();
        }
        Ok(())
    }

    fn reset(&mut self) {
        for channel in &mut self.channels {
            channel.frequency = 0;
            channel.volume = 15;
            channel.counter = 0;
            channel.clock = 0;
            channel.key = false;
        }
        self.test = 0;
    }

    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::K051649)
    }
}

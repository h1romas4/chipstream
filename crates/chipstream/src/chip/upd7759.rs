//! Based on MAME's uPD7759 implementation, revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) Juergen Buchmueller, Mike Balfour, Howie Cohen,
//! Olivier Galibert, and Aaron Giles.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, write_rom_data};
use soundlog::chip::Chip;
use std::collections::VecDeque;
use std::num::NonZeroU32;

const STEPS: [[i16; 8]; 16] = [
    [0, 0, 1, 2, 3, 5, 7, 10],
    [0, 1, 2, 3, 4, 6, 8, 13],
    [0, 1, 2, 4, 5, 7, 10, 15],
    [0, 1, 3, 4, 6, 9, 13, 19],
    [0, 2, 3, 5, 8, 11, 15, 23],
    [0, 2, 4, 7, 10, 14, 19, 29],
    [0, 3, 5, 8, 12, 16, 22, 33],
    [1, 4, 7, 10, 15, 20, 29, 43],
    [1, 4, 8, 13, 18, 25, 35, 53],
    [1, 6, 10, 16, 22, 31, 43, 64],
    [2, 7, 12, 19, 27, 37, 51, 76],
    [2, 9, 16, 24, 34, 46, 64, 96],
    [3, 11, 19, 29, 41, 57, 79, 117],
    [4, 13, 24, 36, 50, 69, 96, 143],
    [4, 16, 29, 44, 62, 85, 118, 175],
    [6, 20, 36, 54, 76, 104, 144, 214],
];
const STATE_STEP: [i8; 8] = [-1, -1, 0, 0, 1, 2, 2, 3];
const MAX_PENDING_BYTES: usize = 1_048_576;

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Idle,
    DropDrq,
    Start,
    FirstRequest,
    LastSample,
    Dummy1,
    AddressHigh,
    AddressLow,
    Dummy2,
    BlockHeader,
    NibbleCount,
    NibbleHigh,
    NibbleLow,
}

/// NEC uPD7759 ADPCM speech processor and its VGM board interface.
///
/// The clock is in hertz; output is mono at clock / 4. [`Self::new`] selects
/// standalone ROM playback; [`Self::with_slave_mode`] selects VGM slave input.
/// `Upd7759Spec` registers 0, 1 and 2 represent reset, start and data pins;
/// register 3 selects the external 128 KiB ROM bank.
#[derive(Clone)]
pub struct Upd7759 {
    clock: u32,
    slave: bool,
    rom: Vec<u8>,
    bank: u8,
    fifo: VecDeque<u8>,
    data: u8,
    reset_line: bool,
    start_line: bool,
    mode_line: bool,
    slave_active: bool,
    drq_pending: bool,
    state: State,
    next_state: State,
    clocks_left: i32,
    next_clocks: i32,
    elapsed: i32,
    requested: u8,
    offset: u32,
    repeat_offset: u32,
    repeat_count: u8,
    first_header: bool,
    nibbles_left: u16,
    divider: u8,
    adpcm_data: u8,
    adpcm_state: i8,
    sample: i16,
}

impl Upd7759 {
    /// Create a standalone ROM-mode chip.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        Self::with_slave_mode(clock, false)
    }

    /// Create a chip using the VGM slave-data interface when `slave` is true.
    pub fn with_slave_mode(clock: u32, slave: bool) -> Result<Self, ChipError> {
        if clock < 4 {
            return Err(ChipError::InvalidClock { clock, minimum: 4 });
        }
        Ok(Self {
            clock,
            slave,
            rom: Vec::new(),
            bank: 0,
            fifo: VecDeque::new(),
            data: 0,
            reset_line: true,
            start_line: true,
            mode_line: true,
            slave_active: false,
            drq_pending: false,
            state: State::Idle,
            next_state: State::Idle,
            clocks_left: 0,
            next_clocks: 0,
            elapsed: 0,
            requested: 0,
            offset: 0,
            repeat_offset: 0,
            repeat_count: 0,
            first_header: false,
            nibbles_left: 0,
            divider: 0,
            adpcm_data: 0,
            adpcm_state: 0,
            sample: 0,
        })
    }

    /// Whether the chip is currently processing a sample.
    pub fn busy(&self) -> bool {
        self.state != State::Idle
    }

    fn reset_engine(&mut self) {
        self.state = State::Idle;
        self.next_state = State::Idle;
        self.clocks_left = 0;
        self.next_clocks = 0;
        self.elapsed = 0;
        self.requested = 0;
        self.offset = 0;
        self.repeat_offset = 0;
        self.repeat_count = 0;
        self.first_header = false;
        self.nibbles_left = 0;
        self.divider = 0;
        self.adpcm_data = 0;
        self.adpcm_state = 0;
        self.sample = 0;
        self.slave_active = false;
        self.drq_pending = false;
        self.fifo.clear();
    }

    fn read_rom(&self, address: u32) -> u8 {
        let address = (usize::from(self.bank) << 17) | (address as usize & 0x1ffff);
        self.rom.get(address).copied().unwrap_or(0xff)
    }

    fn read_data(&mut self) -> u8 {
        if self.slave_active {
            self.data
        } else {
            let result = self.read_rom(self.offset);
            self.offset = self.offset.wrapping_add(1);
            result
        }
    }

    fn request(&mut self) {
        self.next_state = self.state;
        self.next_clocks = self.clocks_left - 21;
        self.state = State::DropDrq;
        self.clocks_left = 21;
        if self.slave_active {
            if let Some(data) = self.fifo.pop_front() {
                self.data = data;
                self.drq_pending = false;
            } else {
                self.drq_pending = true;
            }
        }
    }

    fn decode(&mut self, nibble: u8) {
        let magnitude = usize::from(nibble & 7);
        let step = STEPS[self.adpcm_state as usize][magnitude];
        self.sample = self
            .sample
            .wrapping_add(if nibble & 8 == 0 { step } else { -step });
        self.adpcm_state = (self.adpcm_state + STATE_STEP[magnitude]).clamp(0, 15);
    }

    fn advance(&mut self) {
        let mut request = false;
        match self.state {
            State::Idle => self.clocks_left = 4,
            State::DropDrq => {
                self.drq_pending = false;
                self.state = self.next_state;
                self.clocks_left = self.next_clocks;
            }
            State::Start => {
                self.requested = if self.slave_active { 0x10 } else { self.data };
                self.clocks_left = 70;
                self.state = State::FirstRequest;
            }
            State::FirstRequest => {
                self.clocks_left = 44;
                self.state = State::LastSample;
                request = true;
            }
            State::LastSample => {
                let last = if self.slave_active {
                    self.data
                } else {
                    self.read_rom(0)
                };
                self.clocks_left = 28;
                self.state = if self.requested > last {
                    State::Idle
                } else {
                    State::Dummy1
                };
                request = true;
            }
            State::Dummy1 => {
                self.clocks_left = 32;
                self.state = State::AddressHigh;
                request = true;
            }
            State::AddressHigh => {
                let byte = if self.slave_active {
                    self.data
                } else {
                    self.read_rom(u32::from(self.requested) * 2 + 5)
                };
                self.offset = u32::from(byte) << 9;
                self.clocks_left = 44;
                self.state = State::AddressLow;
                request = true;
            }
            State::AddressLow => {
                let byte = if self.slave_active {
                    self.data
                } else {
                    self.read_rom(u32::from(self.requested) * 2 + 6)
                };
                self.offset |= u32::from(byte) << 1;
                self.clocks_left = 36;
                self.state = State::Dummy2;
                request = true;
            }
            State::Dummy2 => {
                self.offset += 1;
                self.first_header = false;
                self.clocks_left = 36;
                self.state = State::BlockHeader;
                request = true;
            }
            State::BlockHeader => {
                if self.repeat_count != 0 {
                    self.repeat_count -= 1;
                    self.offset = self.repeat_offset;
                }
                let header = self.read_data();
                self.clocks_left = 36;
                match header & 0xc0 {
                    0 => {
                        self.clocks_left = 1024 * (i32::from(header & 0x3f) + 1);
                        self.state = if header == 0 && self.first_header {
                            State::Idle
                        } else {
                            State::BlockHeader
                        };
                        self.sample = 0;
                        self.adpcm_state = 0;
                    }
                    0x40 => {
                        self.divider = (header & 0x3f) + 1;
                        self.nibbles_left = 256;
                        self.state = State::NibbleHigh;
                    }
                    0x80 => {
                        self.divider = (header & 0x3f) + 1;
                        self.state = State::NibbleCount;
                    }
                    _ => {
                        self.repeat_count = (header & 7) + 1;
                        self.repeat_offset = self.offset;
                        self.state = State::BlockHeader;
                    }
                }
                self.first_header |= header != 0;
                request = true;
            }
            State::NibbleCount => {
                self.nibbles_left = u16::from(self.read_data()) + 1;
                self.clocks_left = 36;
                self.state = State::NibbleHigh;
                request = true;
            }
            State::NibbleHigh | State::NibbleLow => {
                let high = self.state == State::NibbleHigh;
                if high {
                    self.adpcm_data = self.read_data();
                    request = true;
                }
                self.decode(if high {
                    self.adpcm_data >> 4
                } else {
                    self.adpcm_data & 15
                });
                self.clocks_left = i32::from(self.divider) * 4;
                self.nibbles_left -= 1;
                self.state = if self.nibbles_left == 0 {
                    State::BlockHeader
                } else if high {
                    State::NibbleLow
                } else {
                    State::NibbleHigh
                };
            }
        }
        if request {
            self.request();
        }
    }

    fn write_register(&mut self, register: u8, value: u8) -> Result<(), ChipError> {
        match register {
            0 => {
                let high = value != 0;
                if self.reset_line && !high {
                    self.reset_engine();
                }
                self.reset_line = high;
            }
            1 if self.slave => {
                let high = value == 0;
                if self.state == State::Idle && self.reset_line {
                    if self.mode_line && !high {
                        self.slave_active = true;
                        self.state = State::Start;
                        self.clocks_left = 0;
                    } else if !self.mode_line && high {
                        self.slave_active = false;
                    }
                }
                self.mode_line = high;
            }
            1 => {
                let high = value != 0;
                if self.state == State::Idle && self.reset_line && self.start_line && !high {
                    self.state = State::Start;
                    self.clocks_left = 0;
                }
                self.start_line = high;
            }
            2 if self.slave && !self.drq_pending => {
                if self.fifo.len() == MAX_PENDING_BYTES {
                    return Err(ChipError::InvalidConfiguration {
                        chip: Chip::Upd7759,
                        reason: "uPD7759 slave input queue is full",
                    });
                }
                self.fifo.push_back(value);
            }
            2 => {
                self.data = value;
                self.drq_pending = false;
            }
            3 => self.bank = value,
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: Chip::Upd7759,
                    register,
                });
            }
        }
        Ok(())
    }
}

impl SoundChip for Upd7759 {
    fn chip_type(&self) -> &Chip {
        &Chip::Upd7759
    }
    fn clock(&self) -> u32 {
        self.clock
    }
    fn sample_rate(&self) -> u32 {
        self.clock / 4
    }
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(self.clock, NonZeroU32::new(4).unwrap())
    }
    fn channels(&self) -> usize {
        1
    }
    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        match write {
            ChipRegisterWrite::Upd7759(spec) => self.write_register(spec.register, spec.value),
            other => Err(ChipError::ChipMismatch {
                expected: other.chip(),
                actual: Chip::Upd7759,
            }),
        }
    }
    fn write_rom(&mut self, start: usize, data: &[u8]) -> Result<(), ChipError> {
        write_rom_data(&mut self.rom, start, data)
    }
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        for output in buffer {
            *output = if self.busy() {
                i32::from(self.sample) * 128
            } else {
                0
            };
            if self.busy() {
                self.elapsed += 4;
                while self.busy() && self.elapsed >= self.clocks_left {
                    self.elapsed -= self.clocks_left;
                    self.advance();
                }
            }
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.reset_engine();
    }
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, ChipError> {
        Ok(super::save_cloned_state(self))
    }
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), ChipError> {
        super::restore_cloned_state(self, state, Chip::Upd7759)
    }
}

//! SCSP DSP based on MAME revision
//! `367a7f677a86d4806b5f808978590de5463eb90a`.
//! Copyright (c) ElSemi and R. Belmont. See `THIRD_PARTY_NOTICES.md`.

#[derive(Clone)]
pub(super) struct Dsp {
    pub coef: [i16; 64],
    pub addresses: [u16; 32],
    pub program: [u16; 512],
    temp: [i32; 128],
    memory: [i32; 32],
    pub mix: [i32; 16],
    pub effects: [i16; 16],
    pub ring_base: u32,
    pub ring_length: u32,
    counter: u32,
    steps: usize,
    running: bool,
}

impl Default for Dsp {
    fn default() -> Self {
        Self {
            coef: [0; 64],
            addresses: [0; 32],
            program: [0; 512],
            temp: [0; 128],
            memory: [0; 32],
            mix: [0; 16],
            effects: [0; 16],
            ring_base: 0,
            ring_length: 8192,
            counter: 0,
            steps: 0,
            running: false,
        }
    }
}

fn sign_extend(value: i32, bits: u32) -> i32 {
    (value << (32 - bits)) >> (32 - bits)
}

fn pack(mut value: i32) -> u16 {
    let sign = (value >> 23) & 1;
    let mut temp = (value ^ value.wrapping_shl(1)) & 0x00ff_ffff;
    let mut exponent = 0;
    while exponent < 12 && temp & 0x0080_0000 == 0 {
        temp <<= 1;
        exponent += 1;
    }
    value = if exponent < 12 {
        value.wrapping_shl(exponent) & 0x003f_ffff
    } else {
        value.wrapping_shl(11)
    };
    (((value >> 11) & 0x7ff) | (sign << 15) | ((exponent as i32) << 11)) as u16
}

fn unpack(value: u16) -> i32 {
    let sign = i32::from(value >> 15);
    let exponent = i32::from((value >> 11) & 15);
    let mut result = i32::from(value & 0x7ff) << 11;
    result |= if exponent > 11 {
        sign << 22
    } else {
        (sign ^ 1) << 22
    };
    result |= sign << 23;
    sign_extend(result, 24) >> exponent.min(11)
}

impl Dsp {
    #[cfg(feature = "scsp")]
    pub fn read_register(&self, address: usize) -> u16 {
        let half = |value: i32| {
            if address & 2 != 0 {
                value as u16
            } else {
                (value >> 16) as u16
            }
        };
        match address {
            0xc00..=0xdff => half(self.temp[(address >> 2) & 127]),
            0xe00..=0xe7f => half(self.memory[(address >> 2) & 31]),
            0xe80..=0xebf => half(self.mix[(address >> 2) & 15]),
            0xec0..=0xedf => self.effects[(address - 0xec0) / 2] as u16,
            _ => 0,
        }
    }

    pub fn start(&mut self) {
        self.steps = self
            .program
            .chunks_exact(4)
            .rposition(|instruction| instruction.iter().any(|word| *word != 0))
            .map_or(0, |index| index + 1);
        self.running = true;
    }

    pub fn valid(&self) -> bool {
        !self.running
            || self
                .program
                .chunks_exact(4)
                .take(self.steps)
                .all(|instruction| ((instruction[1] >> 6) & 0x3f) <= 0x31)
    }

    pub fn step(&mut self, ram: &mut [u8]) {
        if !self.running {
            return;
        }
        self.effects.fill(0);
        let mut acc = 0_i32;
        let mut mem_value = 0_i32;
        let mut fraction = 0_i32;
        let mut y_reg = 0_i32;
        let mut address_reg = 0_u32;
        let ram_mask = ram.len() - 1;
        for step in 0..self.steps {
            let [w0, w1, w2, w3] = self.program[step * 4..step * 4 + 4] else {
                unreachable!()
            };
            let tra = u32::from((w0 >> 8) & 0x7f);
            let twa = u32::from(w0 & 0x7f);
            let ira = usize::from((w1 >> 6) & 0x3f);
            let iwa = usize::from(w1 & 31);
            let mut input = match ira {
                0..=0x1f => self.memory[ira],
                0x20..=0x2f => self.mix[ira - 0x20].wrapping_shl(4),
                0x30..=0x31 => 0,
                _ => unreachable!("DSP program validated before rendering"),
            };
            input = sign_extend(input, 24);
            if w1 & 0x20 != 0 {
                self.memory[iwa] = mem_value;
                if ira == iwa {
                    input = mem_value;
                }
            }

            let temp = sign_extend(
                self.temp[(tra.wrapping_add(self.counter) & 127) as usize],
                24,
            );
            let mut b = if w2 & 2 != 0 {
                0
            } else if w2 & 1 != 0 {
                acc
            } else {
                temp
            };
            if w2 & 2 == 0 && w2 & 4 != 0 {
                b = b.wrapping_neg();
            }
            let x = if w1 & 0x8000 != 0 { input } else { temp };
            let y = match (w1 >> 13) & 3 {
                0 => fraction,
                1 => i32::from(self.coef[usize::from((w3 >> 9) & 63)]) >> 3,
                2 => (y_reg >> 11) & 0x1fff,
                _ => (y_reg >> 4) & 0xfff,
            };
            if w2 & 8 != 0 {
                y_reg = input;
            }
            let shift = (w2 >> 4) & 3;
            let shifted = match shift {
                0 => acc.clamp(-0x800000, 0x7fffff),
                1 => acc.saturating_mul(2).clamp(-0x800000, 0x7fffff),
                2 => sign_extend(acc.wrapping_mul(2), 24),
                _ => sign_extend(acc, 24),
            };
            acc = (((i64::from(x) * i64::from(sign_extend(y, 13))) >> 12) + i64::from(b)) as i32;
            if w0 & 0x80 != 0 {
                self.temp[(twa.wrapping_add(self.counter) & 127) as usize] = shifted;
            }
            if w2 & 0x40 != 0 {
                fraction = if shift == 3 {
                    shifted & 0xfff
                } else {
                    (shifted >> 11) & 0x1fff
                };
            }
            if w2 & 0x6000 != 0 {
                let table = w2 & 0x8000 != 0;
                let mut address = u32::from(self.addresses[usize::from((w3 >> 2) & 31)]);
                if !table {
                    address = address.wrapping_add(self.counter);
                }
                if w3 & 2 != 0 {
                    address = address.wrapping_add(address_reg & 0xfff);
                }
                if w3 & 1 != 0 {
                    address = address.wrapping_add(1);
                }
                address &= if table { 0xffff } else { self.ring_length - 1 };
                address = (address + (self.ring_base << 12)) << 1;
                let index = address as usize & ram_mask;
                if step & 1 != 0 {
                    if w2 & 0x2000 != 0 {
                        let word = u16::from_be_bytes([ram[index], ram[(index + 1) & ram_mask]]);
                        mem_value = if w3 & 0x8000 != 0 {
                            i32::from(word) << 8
                        } else {
                            unpack(word)
                        };
                    }
                    if w2 & 0x4000 != 0 {
                        let word = if w3 & 0x8000 != 0 {
                            (shifted >> 8) as u16
                        } else {
                            pack(shifted)
                        };
                        let [high, low] = word.to_be_bytes();
                        ram[index] = high;
                        ram[(index + 1) & ram_mask] = low;
                    }
                }
            }
            if w2 & 0x80 != 0 {
                address_reg = if shift == 3 {
                    ((shifted >> 12) & 0xfff) as u32
                } else {
                    (input >> 16) as u32
                };
            }
            if w2 & 0x1000 != 0 {
                let target = &mut self.effects[usize::from((w2 >> 8) & 15)];
                *target = target.wrapping_add((shifted >> 8) as i16);
            }
        }
        self.counter = self.counter.wrapping_sub(1);
        self.mix.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixing_uses_the_previous_accumulator_value() {
        let mut dsp = Dsp::default();
        dsp.coef[0] = 0x4000;
        dsp.mix[0] = 1024;
        dsp.program[1] = 0xa800;
        dsp.program[6] = 0x1000;
        dsp.start();

        assert!(dsp.valid());
        dsp.step(&mut [0; 2]);
        assert_eq!(dsp.effects[0], 32);
        assert_eq!(dsp.mix[0], 0);
    }

    #[test]
    fn memory_reads_feed_later_instructions() {
        let mut dsp = Dsp::default();
        dsp.coef[0] = 0x4000;
        dsp.program[6] = 0x2000;
        dsp.program[7] = 0x8000;
        dsp.program[9] = 0x20;
        dsp.program[13] = 0xa000;
        dsp.program[18] = 0x1000;
        dsp.start();

        dsp.step(&mut [0x10, 0x00]);
        assert_eq!(dsp.effects[0], 2048);
    }

    #[test]
    fn detects_reserved_input_registers() {
        let mut dsp = Dsp::default();
        dsp.program[1] = 0x32 << 6;
        dsp.start();
        assert!(!dsp.valid());
    }

    #[test]
    fn packed_memory_format_preserves_zero_and_full_scale_sign() {
        for value in [0, 0x7f_ffff, -0x80_0000, -1] {
            let restored = unpack(pack(value));
            assert!((restored - value).abs() <= 2048, "{value}: {restored}");
        }
    }

    #[test]
    fn temporary_address_wraps_when_counter_rolls_over() {
        let mut dsp = Dsp::default();
        dsp.program[0] = 0x0181;
        dsp.start();
        dsp.step(&mut [0; 2]);
        dsp.step(&mut [0; 2]);
    }
}

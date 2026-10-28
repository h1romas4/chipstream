//! Device timing metadata and generic clock-range collection.

use soundlog::chip::Chip;
use soundlog::vgm::command::{Instance, VgmCommand};

use crate::chip::{ChipRegisterWrite, SampleRate};

#[derive(Clone, Copy)]
struct ClockRegisters {
    first: u8,
    masks: [u8; 4],
    min_divisor: u32,
    max_divisor: u32,
}

#[derive(Clone, Copy, Default)]
pub(super) struct TimingProfile {
    pub(super) write_interval: Option<usize>,
    clock_registers: Option<ClockRegisters>,
}

impl TimingProfile {
    pub(super) fn for_chip(chip: &Chip) -> Self {
        // Oversampled FM backends need several native frames per FM clock.
        let write_interval = match chip {
            Chip::Ym2203 | Chip::Ym2608 => Some(18),
            Chip::Ym2610b => Some(9),
            Chip::Ym2151
            | Chip::Ym2413
            | Chip::Ym2612
            | Chip::Ym3526
            | Chip::Y8950
            | Chip::Ym3812
            | Chip::Ymf262
            | Chip::Ymf278b => Some(1),
            _ => None,
        };
        let clock_registers = match chip {
            Chip::Okim6258 => Some(ClockRegisters {
                first: 0x08,
                masks: [0xff; 4],
                min_divisor: 512,
                max_divisor: 1024,
            }),
            Chip::Okim6295 => Some(ClockRegisters {
                first: 0x08,
                masks: [0xff, 0xff, 0xff, 0x7f],
                min_divisor: 132,
                max_divisor: 165,
            }),
            _ => None,
        };
        Self {
            write_interval,
            clock_registers,
        }
    }
}

#[derive(Clone, Copy)]
struct ClockChanges {
    low: [u8; 4],
    high: [u8; 4],
}

impl Default for ClockChanges {
    fn default() -> Self {
        Self {
            low: [u8::MAX; 4],
            high: [0; 4],
        }
    }
}

impl ClockChanges {
    fn observe_byte(&mut self, index: usize, value: u8) {
        self.low[index] = self.low[index].min(value);
        self.high[index] = self.high[index].max(value);
    }

    fn bounds(&self, clock: u32, registers: ClockRegisters) -> (u32, u32) {
        // Byte-wise extrema conservatively include intermediate clock writes.
        let mut low = clock.to_le_bytes();
        let mut high = low;
        for index in 0..4 {
            low[index] = low[index].min(self.low[index]);
            high[index] = high[index].max(self.high[index]);
        }

        (
            (u32::from_le_bytes(low) / registers.max_divisor).max(1),
            u32::from_le_bytes(high)
                .div_ceil(registers.min_divisor)
                .max(1),
        )
    }
}

struct DynamicClock {
    chip: Chip,
    registers: ClockRegisters,
    instances: [ClockChanges; 2],
}

pub(super) struct Timing {
    clocks: Vec<DynamicClock>,
}

impl Timing {
    pub(super) fn new(clocks: &[(Chip, u32)]) -> Self {
        Self {
            clocks: clocks
                .iter()
                .filter(|(_, clock)| clock & 0x3fff_ffff != 0)
                .filter_map(|(chip, _)| {
                    TimingProfile::for_chip(chip)
                        .clock_registers
                        .map(|registers| DynamicClock {
                            chip: chip.clone(),
                            registers,
                            instances: [ClockChanges::default(); 2],
                        })
                })
                .collect(),
        }
    }

    pub(super) fn needs_scan(&self) -> bool {
        !self.clocks.is_empty()
    }

    pub(super) fn observe(&mut self, command: &VgmCommand) {
        let Some((instance, write)) = ChipRegisterWrite::from_vgm_command(command) else {
            return;
        };
        let Some(clock) = self
            .clocks
            .iter_mut()
            .find(|clock| clock.chip == write.chip())
        else {
            return;
        };
        let (bank, register, value) = write.parts();
        if bank != 0 {
            return;
        }
        let Some(index) = register.checked_sub(clock.registers.first).map(usize::from) else {
            return;
        };
        if let Some(mask) = clock.registers.masks.get(index) {
            clock.instances[instance as usize].observe_byte(index, (value as u8) & mask);
        }
    }

    pub(super) fn observe_clock(&mut self, chip: &Chip, instance: Instance, clock: u32) {
        if let Some(clock_changes) = self.clocks.iter_mut().find(|entry| &entry.chip == chip) {
            for (index, value) in clock.to_le_bytes().into_iter().enumerate() {
                clock_changes.instances[instance as usize].observe_byte(index, value);
            }
        }
    }

    pub(super) fn bounds(
        &self,
        chip: &Chip,
        instance: Instance,
        clock: u32,
        rate: SampleRate,
    ) -> (u32, u32) {
        if let Some(changes) = self.clocks.iter().find(|entry| &entry.chip == chip) {
            changes.instances[instance as usize].bounds(clock, changes.registers)
        } else {
            (
                rate.floor_hz(),
                rate.numerator().div_ceil(rate.denominator().get()),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::chip::{Okim6258Spec, Okim6295Spec};
    use std::num::NonZeroU32;

    #[test]
    fn clock_bounds_cover_intermediate_byte_combinations_and_fractional_rates() {
        let registers = ClockRegisters {
            first: 0,
            masks: [0xff; 4],
            min_divisor: 3,
            max_divisor: 7,
        };
        let initial = 0x0123_4567_u32;
        let mut changes = ClockChanges::default();
        let alternatives = [0x90, 0x12, 0x80, 0x02];
        for (index, value) in alternatives.into_iter().enumerate() {
            changes.observe_byte(index, value);
        }
        let (min, max) = changes.bounds(initial, registers);
        let base = initial.to_le_bytes();
        for combination in 0..16 {
            let bytes = std::array::from_fn(|i| {
                if combination & (1 << i) == 0 {
                    base[i]
                } else {
                    alternatives[i]
                }
            });
            let clock = u32::from_le_bytes(bytes);
            for divisor in registers.min_divisor..=registers.max_divisor {
                assert!(u64::from(min) * u64::from(divisor) <= u64::from(clock));
                assert!(u64::from(max) * u64::from(divisor) >= u64::from(clock));
            }
        }
    }

    #[test]
    fn clock_observations_are_isolated_by_chip_and_instance() {
        let mut timing = Timing::new(&[
            (Chip::Okim6258, 0x4000_0000 | 4_000_000),
            (Chip::Okim6295, 1_000_000),
        ]);
        let rate = SampleRate::from_hz(1);
        let before = timing.bounds(&Chip::Okim6258, Instance::Primary, 4_000_000, rate);
        timing.observe(&VgmCommand::Okim6258Write(
            Instance::Secondary,
            Okim6258Spec {
                register: 0x0a,
                value: 0x70,
            },
        ));
        timing.observe(&VgmCommand::Okim6295Write(
            Instance::Primary,
            Okim6295Spec {
                register: 0x0b,
                value: 0x80,
            },
        ));
        assert_eq!(
            timing.bounds(&Chip::Okim6258, Instance::Primary, 4_000_000, rate),
            before
        );
        assert_ne!(
            timing.bounds(&Chip::Okim6258, Instance::Secondary, 4_000_000, rate),
            before
        );
        assert_eq!(
            timing.bounds(&Chip::Okim6295, Instance::Primary, 1_000_000, rate),
            (1_000_000 / 165, 1_000_000_u32.div_ceil(132))
        );
    }

    #[test]
    fn fixed_fractional_rate_uses_inclusive_rounded_bounds() {
        let timing = Timing::new(&[]);
        let rate = SampleRate::from_ratio(10, NonZeroU32::new(3).unwrap());
        assert_eq!(
            timing.bounds(&Chip::Ym2151, Instance::Primary, 0, rate),
            (3, 4)
        );
    }
}

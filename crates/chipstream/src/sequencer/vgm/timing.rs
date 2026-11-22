//! Device timing metadata and generic clock-range collection.
//!
//! Timing profiles define write pacing and dynamic-clock register layouts.
//! Observed or declared clock changes provide conservative native sample-rate
//! bounds for resampler construction.

use soundlog::chip::Chip;
use soundlog::vgm::command::{Instance, VgmCommand};

use crate::chip::{ChipRegisterWrite, SampleRate};

#[derive(Clone, Copy)]
/// Register layout and divisor bounds for a dynamically clocked chip.
struct ClockRegisters {
    /// Address of the first clock byte.
    first: u8,
    /// Valid bits for each little-endian clock byte.
    masks: [u8; 4],
    /// Smallest possible native-rate divisor.
    min_divisor: u32,
    /// Largest possible native-rate divisor.
    max_divisor: u32,
}

#[derive(Clone, Copy, Default)]
/// Write pacing and optional dynamic-clock metadata for one chip type.
pub(super) struct TimingProfile {
    /// Native frames between writes, or no pacing.
    pub(super) write_interval: Option<usize>,
    /// Clock register layout, if the chip supports dynamic clocks.
    clock_registers: Option<ClockRegisters>,
}

impl TimingProfile {
    /// Select write pacing and dynamic-clock register metadata for a chip.
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
/// Byte-wise extrema of observed clock writes.
struct ClockChanges {
    /// Smallest observed value for each byte.
    low: [u8; 4],
    /// Largest observed value for each byte.
    high: [u8; 4],
}

impl Default for ClockChanges {
    /// Initialize byte-wise clock bounds to include any future writes.
    fn default() -> Self {
        Self {
            low: [u8::MAX; 4],
            high: [0; 4],
        }
    }
}

impl ClockChanges {
    /// Track byte-wise extrema for a dynamically written clock.
    fn observe_byte(&mut self, index: usize, value: u8) {
        self.low[index] = self.low[index].min(value);
        self.high[index] = self.high[index].max(value);
    }

    /// Estimate conservative sample-rate bounds across clock byte changes.
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

/// Clock observations and declared ranges for both chip instances.
struct DynamicClock {
    /// Chip type to which these observations apply.
    chip: Chip,
    /// Register layout and sample-rate divisors.
    registers: ClockRegisters,
    /// Per-instance byte-wise observations.
    instances: [ClockChanges; 2],
    /// Per-instance inclusive ranges supplied by a generator.
    declared_ranges: [Option<(u32, u32)>; 2],
}

/// Dynamic-clock metadata used to size resampler rate bounds.
pub(super) struct Timing {
    /// Entries for present chips with dynamically writable clocks.
    clocks: Vec<DynamicClock>,
}

impl Timing {
    /// Track dynamic clocks for the supported chips present in the header.
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
                            declared_ranges: [None; 2],
                        })
                })
                .collect(),
        }
    }

    /// Return whether any chip requires a clock-change scan.
    pub(super) fn needs_scan(&self) -> bool {
        !self.clocks.is_empty()
    }

    /// Collect a clock-register write from a VGM command.
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

    /// Include a complete clock value in a chip instance's bounds.
    pub(super) fn observe_clock(&mut self, chip: &Chip, instance: Instance, clock: u32) {
        if let Some(clock_changes) = self.clocks.iter_mut().find(|entry| &entry.chip == chip) {
            for (index, value) in clock.to_le_bytes().into_iter().enumerate() {
                clock_changes.instances[instance as usize].observe_byte(index, value);
            }
        }
    }

    /// Include a declared inclusive range of complete clock values.
    pub(super) fn include_range(&mut self, chip: &Chip, instance: Instance, min: u32, max: u32) {
        if let Some(changes) = self.clocks.iter_mut().find(|entry| &entry.chip == chip) {
            let range = &mut changes.declared_ranges[instance as usize];
            *range = Some(match *range {
                Some((old_min, old_max)) => (old_min.min(min), old_max.max(max)),
                None => (min, max),
            });
        }
    }

    /// Return conservative native-rate bounds, or the fixed rate's bounds.
    pub(super) fn bounds(
        &self,
        chip: &Chip,
        instance: Instance,
        clock: u32,
        rate: SampleRate,
    ) -> (u32, u32) {
        if *chip == Chip::Es5503 {
            return (
                (clock / 272).max(1).min(rate.floor_hz()),
                clock
                    .div_ceil(24)
                    .max(rate.numerator().div_ceil(rate.denominator().get())),
            );
        }
        if matches!(chip, Chip::Es5505 | Chip::Es5506U8 | Chip::Es5506U16) {
            return (clock / 512, clock.div_ceil(16));
        }
        if let Some(changes) = self.clocks.iter().find(|entry| &entry.chip == chip) {
            let (mut min, mut max) =
                changes.instances[instance as usize].bounds(clock, changes.registers);
            if let Some((declared_min, declared_max)) = changes.declared_ranges[instance as usize] {
                min = min.min((declared_min / changes.registers.max_divisor).max(1));
                max = max.max(declared_max.div_ceil(changes.registers.min_divisor).max(1));
            }
            (min, max)
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

    #[test]
    fn declared_ranges_are_merged_per_instance() {
        let mut timing = Timing::new(&[(Chip::Okim6258, 0x4000_0000 | 8_000_000)]);
        let rate = SampleRate::from_hz(7_812);
        timing.include_range(&Chip::Okim6258, Instance::Secondary, 4_000_000, 12_000_000);
        timing.include_range(&Chip::Okim6258, Instance::Secondary, 6_000_000, 16_000_000);
        assert_eq!(
            timing.bounds(&Chip::Okim6258, Instance::Primary, 8_000_000, rate),
            (8_000_000 / 1024, 8_000_000_u32.div_ceil(512))
        );
        assert_eq!(
            timing.bounds(&Chip::Okim6258, Instance::Secondary, 8_000_000, rate),
            (4_000_000 / 1024, 16_000_000_u32.div_ceil(512))
        );
    }
}

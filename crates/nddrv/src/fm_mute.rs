use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use chipstream::sequencer::Chip;
use soundlog::vgm::command::Instance;

pub const CHIP_COUNT: usize = 11;

pub fn chip_index(chip: &Chip) -> Option<usize> {
    Some(match chip {
        Chip::Ym2151 => 0,
        Chip::Ym2203 => 1,
        Chip::Ym2413 => 2,
        Chip::Ym2608 => 3,
        Chip::Ym2610b => 4,
        Chip::Ym2612 => 5,
        Chip::Ym3526 => 6,
        Chip::Y8950 => 7,
        Chip::Ym3812 => 8,
        Chip::Ymf262 => 9,
        Chip::Ymf278b => 10,
        _ => return None,
    })
}

pub fn fm_channels(chip: &Chip) -> Option<u8> {
    Some(match chip {
        Chip::Ym2151 => 8,
        Chip::Ym2203 => 3,
        Chip::Ym2413 | Chip::Ym3526 | Chip::Y8950 | Chip::Ym3812 => 9,
        Chip::Ym2608 | Chip::Ym2610b | Chip::Ym2612 => 6,
        Chip::Ymf262 | Chip::Ymf278b => 18,
        _ => return None,
    })
}

pub fn tracked_channels(chip: &Chip) -> Option<u8> {
    fm_channels(chip).map(|count| match chip {
        Chip::Ym2203 | Chip::Ym2608 | Chip::Ym2610b => count + 3,
        _ => count,
    })
}

fn instance_index(instance: Instance) -> usize {
    match instance {
        Instance::Primary => 0,
        Instance::Secondary => 1,
    }
}

#[derive(Default)]
struct Source {
    muted: AtomicU32,
    soloed: AtomicU32,
    opl_pairs: AtomicU8,
    rhythm: AtomicBool,
}

#[derive(Default)]
pub struct FmMute {
    sources: [[Source; 2]; CHIP_COUNT],
}

impl FmMute {
    pub fn copy_selections_from(&self, other: &Self) {
        for (destination, source) in self
            .sources
            .iter()
            .flatten()
            .zip(other.sources.iter().flatten())
        {
            destination
                .muted
                .store(source.muted.load(Ordering::Acquire), Ordering::Release);
            destination
                .soloed
                .store(source.soloed.load(Ordering::Acquire), Ordering::Release);
        }
    }

    fn source(&self, chip: &Chip, instance: Instance) -> Option<&Source> {
        Some(&self.sources[chip_index(chip)?][instance_index(instance)])
    }

    fn channel_mask(source: &Source, chip: &Chip, channel: u8) -> u32 {
        let mut mask = 1_u32 << channel;
        if matches!(chip, Chip::Ymf262 | Chip::Ymf278b) {
            let bank = channel / 9;
            let local = channel % 9;
            if local < 6 {
                let pair = local % 3 + bank * 3;
                if source.opl_pairs.load(Ordering::Acquire) & (1 << pair) != 0 {
                    mask |= 1_u32 << (bank * 9 + (local + 3) % 6);
                }
            }
        }
        mask
    }

    pub fn is_muted(&self, chip: &Chip, instance: Instance, channel: u8) -> bool {
        let Some(source) = self.source(chip, instance) else {
            return false;
        };
        if fm_channels(chip).is_none_or(|count| channel >= count) {
            return false;
        }
        source.muted.load(Ordering::Acquire) & Self::channel_mask(source, chip, channel) != 0
    }

    pub fn set_muted(&self, chip: &Chip, instance: Instance, channel: u8, muted: bool) -> bool {
        let Some(source) = self.source(chip, instance) else {
            return false;
        };
        if fm_channels(chip).is_none_or(|count| channel >= count) {
            return false;
        }
        let mask = Self::channel_mask(source, chip, channel);
        if muted {
            source.muted.fetch_or(mask, Ordering::AcqRel);
        } else {
            source.muted.fetch_and(!mask, Ordering::AcqRel);
        }
        true
    }

    pub fn is_soloed(&self, chip: &Chip, instance: Instance, channel: u8) -> bool {
        let Some(source) = self.source(chip, instance) else {
            return false;
        };
        if fm_channels(chip).is_none_or(|count| channel >= count) {
            return false;
        }
        source.soloed.load(Ordering::Acquire) & Self::channel_mask(source, chip, channel) != 0
    }

    pub fn set_soloed(&self, chip: &Chip, instance: Instance, channel: u8, soloed: bool) -> bool {
        let Some(source) = self.source(chip, instance) else {
            return false;
        };
        if fm_channels(chip).is_none_or(|count| channel >= count) {
            return false;
        }
        let mask = Self::channel_mask(source, chip, channel);
        if soloed {
            source.soloed.fetch_or(mask, Ordering::AcqRel);
        } else {
            source.soloed.fetch_and(!mask, Ordering::AcqRel);
        }
        true
    }

    pub fn is_effectively_muted(&self, chip: &Chip, instance: Instance, channel: u8) -> bool {
        if self.source(chip, instance).is_none()
            || fm_channels(chip).is_none_or(|count| channel >= count)
        {
            return false;
        }
        let any_solo = self.sources.iter().flatten().any(|source| {
            let soloed = source.soloed.load(Ordering::Acquire);
            let eligible = if source.rhythm.load(Ordering::Acquire) {
                soloed & !(0b111 << 6)
            } else {
                soloed
            };
            eligible != 0
        });
        if any_solo {
            !self.is_soloed(chip, instance, channel)
        } else {
            self.is_muted(chip, instance, channel)
        }
    }

    pub fn paired_channel(&self, chip: &Chip, instance: Instance, channel: u8) -> Option<u8> {
        let source = self.source(chip, instance)?;
        let mask = Self::channel_mask(source, chip, channel) & !(1_u32 << channel);
        (mask != 0).then(|| mask.trailing_zeros() as u8)
    }

    pub fn rhythm_channel(&self, chip: &Chip, instance: Instance, channel: u8) -> bool {
        (6..=8).contains(&channel)
            && matches!(
                chip,
                Chip::Ym2413
                    | Chip::Ym3526
                    | Chip::Y8950
                    | Chip::Ym3812
                    | Chip::Ymf262
                    | Chip::Ymf278b
            )
            && self
                .source(chip, instance)
                .is_some_and(|source| source.rhythm.load(Ordering::Acquire))
    }

    pub fn opll_write(&self, instance: Instance, register: u8, value: u8) -> u8 {
        let chip = Chip::Ym2413;
        if register == 0x0e
            && let Some(source) = self.source(&chip, instance)
        {
            source.rhythm.store(value & 0x20 != 0, Ordering::Release);
        }
        if (0x20..=0x28).contains(&register) {
            let channel = register - 0x20;
            if !self.rhythm_channel(&chip, instance, channel)
                && self.is_effectively_muted(&chip, instance, channel)
            {
                return value & !0x10;
            }
        }
        value
    }

    pub fn opl_write(
        &self,
        chip: &Chip,
        instance: Instance,
        port: u8,
        register: u8,
        value: u8,
    ) -> u8 {
        if port == 0
            && register == 0xbd
            && let Some(source) = self.source(chip, instance)
        {
            source.rhythm.store(value & 0x20 != 0, Ordering::Release);
        }
        if port == 1
            && register == 0x04
            && let Some(source) = self.source(chip, instance)
        {
            source.opl_pairs.store(value & 0x3f, Ordering::Release);
        }
        if port <= 1 && (0xb0..=0xb8).contains(&register) {
            let channel = port * 9 + register - 0xb0;
            if !self.rhythm_channel(chip, instance, channel)
                && self.is_effectively_muted(chip, instance, channel)
            {
                return value & !0x20;
            }
        }
        value
    }

    pub fn opn_write(
        &self,
        chip: &Chip,
        instance: Instance,
        port: u8,
        register: u8,
        value: u8,
    ) -> u8 {
        if port == 0 && register == 0x28 {
            let local = value & 3;
            let channel = local + if value & 4 != 0 { 3 } else { 0 };
            if local < 3 && self.is_effectively_muted(chip, instance, channel) {
                return value & 0x0f;
            }
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solo_is_global_multiple_and_overrides_manual_mutes() {
        let state = FmMute::default();
        let a = (&Chip::Ym2608, Instance::Primary, 0);
        let b = (&Chip::Ym2608, Instance::Secondary, 1);
        let c = (&Chip::Ym2151, Instance::Primary, 2);
        assert!(state.set_muted(a.0, a.1, a.2, true));
        assert!(state.set_muted(c.0, c.1, c.2, true));
        assert!(state.set_soloed(a.0, a.1, a.2, true));
        assert!(state.is_muted(a.0, a.1, a.2));
        assert!(!state.is_effectively_muted(a.0, a.1, a.2));
        assert!(state.is_effectively_muted(b.0, b.1, b.2));
        assert!(state.is_effectively_muted(c.0, c.1, c.2));
        assert!(state.set_soloed(b.0, b.1, b.2, true));
        assert!(!state.is_effectively_muted(b.0, b.1, b.2));
        assert!(state.set_soloed(a.0, a.1, a.2, false));
        assert!(state.is_effectively_muted(a.0, a.1, a.2));
        assert!(state.set_soloed(b.0, b.1, b.2, false));
        assert!(state.is_effectively_muted(a.0, a.1, a.2));
        assert!(!state.is_effectively_muted(b.0, b.1, b.2));
        assert!(state.is_effectively_muted(c.0, c.1, c.2));
        assert!(!state.set_soloed(&Chip::Ym2203, Instance::Primary, 3, true));
        assert!(!state.set_soloed(&Chip::Sn76489, Instance::Primary, 0, true));
    }

    #[test]
    fn solo_obeys_opl_pairs_and_suspends_rhythm_channels() {
        let state = FmMute::default();
        let chip = Chip::Ymf262;
        let instance = Instance::Primary;
        state.opl_write(&chip, instance, 1, 0x04, 1);
        assert!(state.set_soloed(&chip, instance, 3, true));
        assert!(state.is_soloed(&chip, instance, 0));
        assert_eq!(state.opl_write(&chip, instance, 0, 0xb0, 0x20), 0x20);
        assert_eq!(state.opl_write(&chip, instance, 0, 0xb1, 0x20), 0);
        assert!(state.set_soloed(&chip, instance, 0, false));
        assert!(!state.is_soloed(&chip, instance, 3));
        assert!(state.set_soloed(&chip, instance, 6, true));
        assert_eq!(state.opl_write(&chip, instance, 0, 0xb0, 0x20), 0);
        state.opl_write(&chip, instance, 0, 0xbd, 0x20);
        assert_eq!(state.opl_write(&chip, instance, 0, 0xb0, 0x20), 0x20);
        assert_eq!(state.opl_write(&chip, instance, 0, 0xb6, 0x20), 0x20);
        assert!(state.is_soloed(&chip, instance, 6));
        state.opl_write(&chip, instance, 0, 0xbd, 0);
        assert_eq!(state.opl_write(&chip, instance, 0, 0xb0, 0x20), 0);
    }

    #[test]
    fn opn_filters_only_selected_fm_key_on_and_instance() {
        let mute = FmMute::default();
        assert!(mute.set_muted(&Chip::Ym2608, Instance::Primary, 4, true));
        assert_eq!(
            mute.opn_write(&Chip::Ym2608, Instance::Primary, 0, 0x28, 0xf5),
            0x05
        );
        assert_eq!(
            mute.opn_write(&Chip::Ym2608, Instance::Secondary, 0, 0x28, 0xf5),
            0xf5
        );
        assert_eq!(
            mute.opn_write(&Chip::Ym2608, Instance::Primary, 1, 0x28, 0xf5),
            0xf5
        );
        assert_eq!(
            mute.opn_write(&Chip::Ym2608, Instance::Primary, 0, 0x28, 0xf4),
            0xf4
        );
        assert_eq!(
            mute.opn_write(&Chip::Ym2608, Instance::Primary, 0, 0x08, 0x1f),
            0x1f
        );
        assert_eq!(
            mute.opn_write(&Chip::Ym2612, Instance::Primary, 0, 0x2a, 0x80),
            0x80
        );
        assert!(!mute.set_muted(&Chip::Ym2203, Instance::Primary, 3, true));
        assert!(!mute.set_muted(&Chip::Sn76489, Instance::Primary, 0, true));
    }

    #[test]
    fn opl_pairs_share_mute_only_while_enabled() {
        let mute = FmMute::default();
        let chip = Chip::Ymf262;
        let instance = Instance::Primary;
        assert!(mute.set_muted(&chip, instance, 3, true));
        assert_eq!(mute.opl_write(&chip, instance, 0, 0xb0, 0x3f), 0x3f);
        assert_eq!(mute.opl_write(&chip, instance, 1, 0x04, 1), 1);
        assert_eq!(mute.paired_channel(&chip, instance, 0), Some(3));
        assert_eq!(mute.opl_write(&chip, instance, 0, 0xb0, 0x3f), 0x1f);
        assert!(mute.set_muted(&chip, instance, 0, false));
        assert!(!mute.is_muted(&chip, instance, 3));
        assert_eq!(mute.opl_write(&chip, instance, 0, 0xb0, 0x3f), 0x3f);
    }

    #[test]
    fn rhythm_mode_never_filters_percussion_channels() {
        let mute = FmMute::default();
        let instance = Instance::Primary;
        assert!(mute.set_muted(&Chip::Ym2413, instance, 6, true));
        assert_eq!(mute.opll_write(instance, 0x26, 0x1f), 0x0f);
        assert_eq!(mute.opll_write(instance, 0x0e, 0x3f), 0x3f);
        assert!(mute.rhythm_channel(&Chip::Ym2413, instance, 6));
        assert_eq!(mute.opll_write(instance, 0x26, 0x1f), 0x1f);
        mute.opll_write(instance, 0x0e, 0);
        assert_eq!(mute.opll_write(instance, 0x26, 0x1f), 0x0f);

        assert!(mute.set_muted(&Chip::Ym3812, instance, 8, true));
        assert_eq!(mute.opl_write(&Chip::Ym3812, instance, 0, 0xb8, 0x3f), 0x1f);
        assert_eq!(mute.opl_write(&Chip::Ym3812, instance, 0, 0xbd, 0x3f), 0x3f);
        assert_eq!(mute.opl_write(&Chip::Ym3812, instance, 0, 0xb8, 0x3f), 0x3f);
    }

    #[test]
    fn every_opl_pair_is_scoped_to_its_chip_and_instance() {
        let mute = FmMute::default();
        for chip in [Chip::Ymf262, Chip::Ymf278b] {
            for instance in [Instance::Primary, Instance::Secondary] {
                for bit in 0..6 {
                    mute.opl_write(&chip, instance, 1, 0x04, 1 << bit);
                    let first = bit / 3 * 9 + bit % 3;
                    let second = first + 3;
                    assert_eq!(mute.paired_channel(&chip, instance, first), Some(second));
                    assert!(mute.set_muted(&chip, instance, second, true));
                    assert!(mute.is_muted(&chip, instance, first));
                    assert_eq!(
                        mute.opl_write(&chip, instance, first / 9, 0xb0 + first % 9, 0x3f),
                        0x1f
                    );
                    assert!(mute.set_muted(&chip, instance, first, false));
                    assert!(!mute.is_muted(&chip, instance, second));
                }
                mute.opl_write(&chip, instance, 1, 0x04, 0);
            }
        }
    }
}

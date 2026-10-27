//! VGM header volume modifiers and per-chip gain calculation.

use soundlog::chip::Chip;
use soundlog::vgm::command::Instance;
use soundlog::vgm::header::{ChipId, VgmExtraHeader};

pub(super) fn global_gain(raw: u8) -> f32 {
    let value = match raw {
        0x00..=0xc0 => i32::from(raw),
        0xc1 => -64,
        _ => i32::from(raw) - 256,
    };
    2.0_f32.powf(value as f32 / 32.0)
}

pub(super) fn chip_gain(
    extra: Option<&VgmExtraHeader>,
    chip: &Chip,
    chip_id: ChipId,
    instance: Instance,
    paired: bool,
    dual: bool,
) -> f32 {
    let entry = extra.and_then(|header| {
        header.chip_volumes.iter().find(|entry| {
            entry.chip_id == chip_id && entry.instance == instance && entry.paired_chip == paired
        })
    });
    match entry {
        None => 1.0,
        Some(entry) if entry.relative => f32::from(entry.volume) / 256.0,
        Some(entry) => {
            // Normalize absolute levels to libvgm's defaults without changing
            // the sequencer's existing gain when no entry is present.
            let mut default = match chip {
                Chip::Ym2413 => 0x200,
                Chip::Sn76489 => 0x80,
                Chip::SegaPcm => 0x180,
                Chip::Pwm => 0xe0,
                Chip::Ym2203 if paired => 0x80,
                Chip::Ym2608 | Chip::Ym2610b => 0x80,
                Chip::Okim6258 => 0x1c0,
                _ => 0x100,
            };
            if dual {
                default /= 2;
            }
            f32::from(entry.volume) / default as f32
        }
    }
}

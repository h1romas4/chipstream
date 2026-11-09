//! Adapters between sequencer sources and software targets.

use chipstream::chip::{ChipError, SoundChip, YmfmChip};
use chipstream::sequencer::{Chip, ChipFactoryConfig, ChipFactoryRegistry, Instance};
use soundlog::vgm::header::{Ay8910ChipType, VgmHeader};

use crate::routing::{Adapter, ChipInstance, RoutingPlan, SoftwareTarget, Source, Target};

pub fn ym2149_clock_divider(header: &VgmHeader) -> Option<bool> {
    let ym = matches!(
        header.ay_chip_type,
        Ay8910ChipType::Ym2149
            | Ay8910ChipType::Ym3439
            | Ay8910ChipType::Ymz284
            | Ay8910ChipType::Ymz294
    );
    if (!ym
        && !matches!(
            header.ay_chip_type,
            Ay8910ChipType::Ay8910 | Ay8910ChipType::Ay8912 | Ay8910ChipType::Ay8913
        ))
        || header.ay8910_flags.discrete_output
        || header.ay8910_flags.raw_output
        || header.ay8910_flags.reserved != 0
    {
        return None;
    }
    Some(ym && header.ay8910_flags.ym_pin26_low)
}

#[derive(Clone, Copy)]
pub enum AdapterMode {
    Playback,
    Preview,
}

pub fn register_software_adapters(
    factories: &mut ChipFactoryRegistry,
    plan: &RoutingPlan,
    mode: AdapterMode,
) {
    let source = Source {
        chip: Chip::Ay8910,
        instance: ChipInstance::Primary,
    };
    if plan.software_fallback(source).adapter == Adapter::AyToYm2149 {
        let plan = plan.clone();
        factories.register_fallback(Chip::Ay8910, move |config| {
            let source = Source {
                chip: Chip::Ay8910,
                instance: match config.instance() {
                    Instance::Primary => ChipInstance::Primary,
                    Instance::Secondary => ChipInstance::Secondary,
                },
            };
            let assignment = match mode {
                AdapterMode::Playback => plan.assignment(&source).clone(),
                AdapterMode::Preview => plan.software_fallback(source),
            };
            match (&assignment.target, assignment.adapter) {
                (
                    Target::Software {
                        chip: SoftwareTarget::Ym2149,
                        ..
                    },
                    Adapter::AyToYm2149,
                ) => ym2149_backend(config),
                (Target::Hardware { .. }, _) => Err(ChipError::InvalidConfiguration {
                    chip: Chip::Ay8910,
                    reason: "the selected hardware route has no active factory",
                }),
                _ => Err(ChipError::UnsupportedChip(Chip::Ay8910)),
            }
        });
    }
}

fn ym2149_backend(config: &ChipFactoryConfig<'_>) -> Result<Box<dyn SoundChip>, ChipError> {
    let divide = ym2149_clock_divider(config.header()).ok_or(ChipError::InvalidConfiguration {
        chip: Chip::Ay8910,
        reason: "YM2149 emulation does not support this AY variant or output flags",
    })?;
    Ok(Box::new(YmfmChip::new_ym2149(config.clock(), divide)?))
}

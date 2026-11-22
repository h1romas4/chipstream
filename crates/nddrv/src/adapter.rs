//! Adapters between sequencer sources and software targets.

use crate::routing::{Adapter, ChipInstance, RoutingPlan, SoftwareTarget, Source, Target};
use chipstream::chip::{Ay8910, ChipError, SoundChip, YmfmChip};
use chipstream::sequencer::{
    Ay8910Settings, Chip, ChipFactoryConfig, ChipFactoryRegistry, Instance,
};

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
            (
                Target::Software {
                    chip: SoftwareTarget::Native(Chip::Ay8910),
                    ..
                },
                Adapter::Direct,
            ) => native_ay8910_backend(config),
            (Target::Hardware { .. }, _) => Err(ChipError::InvalidConfiguration {
                chip: Chip::Ay8910,
                reason: "the selected hardware route has no active factory",
            }),
            _ => Err(ChipError::UnsupportedChip(Chip::Ay8910)),
        }
    });
}

fn native_ay8910_backend(config: &ChipFactoryConfig) -> Result<Box<dyn SoundChip>, ChipError> {
    let settings = config
        .ay8910_settings()
        .ok_or(ChipError::UnsupportedChip(Chip::Ay8910))?;
    if !settings.supports_native_ay8910() {
        return Err(ChipError::InvalidConfiguration {
            chip: Chip::Ay8910,
            reason: "native AY8910 does not support this model or output configuration",
        });
    }
    Ok(Box::new(Ay8910::new(config.clock())?))
}

fn ym2149_backend(config: &ChipFactoryConfig) -> Result<Box<dyn SoundChip>, ChipError> {
    let divide = config
        .ay8910_settings()
        .and_then(Ay8910Settings::ym2149_clock_divider)
        .ok_or(ChipError::InvalidConfiguration {
            chip: Chip::Ay8910,
            reason: "YM2149 emulation does not support this AY variant or output flags",
        })?;
    Ok(Box::new(YmfmChip::new_ym2149(config.clock(), divide)?))
}

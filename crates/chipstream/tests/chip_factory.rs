use std::sync::{Arc, Mutex};

use chipstream::chip::{ChipError, ChipRegisterWrite, SoundChip};
use chipstream::sequencer::{
    Chip, ChipFactoryRegistry, Instance, MdxSequencer, VgmSequencer, VgmSequencerError,
};
use soundlog::chip::{Ym2151Spec, Ym2612Spec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::WaitSamples;

const MDX: &[u8] = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");

struct FakeChip {
    chip: Chip,
    clock: u32,
    channels: usize,
    writes: Arc<Mutex<Vec<Chip>>>,
}

impl FakeChip {
    fn new(chip: Chip, clock: u32, writes: Arc<Mutex<Vec<Chip>>>) -> Self {
        let channels = match &chip {
            Chip::Ym2203 | Chip::Ymf262 => 4,
            Chip::Ym2608 | Chip::Ym2610b => 3,
            Chip::Ymf278b => 6,
            Chip::Ym2413 | Chip::Ym2151 | Chip::Ym2612 | Chip::Pwm => 2,
            _ => 1,
        };
        Self {
            chip,
            clock,
            channels,
            writes,
        }
    }
}

impl SoundChip for FakeChip {
    fn chip_type(&self) -> &Chip {
        &self.chip
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        44_100
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        if write.chip() != self.chip {
            return Err(ChipError::ChipMismatch {
                expected: write.chip(),
                actual: self.chip.clone(),
            });
        }
        self.writes.lock().unwrap().push(self.chip.clone());
        Ok(())
    }

    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        samples.fill(0);
        Ok(())
    }

    fn reset(&mut self) {}
}

fn ymfm_chips() -> [Chip; 11] {
    [
        Chip::Ym2413,
        Chip::Ym2612,
        Chip::Ym2151,
        Chip::Ym2203,
        Chip::Ym2608,
        Chip::Ym2610b,
        Chip::Ym3812,
        Chip::Ym3526,
        Chip::Y8950,
        Chip::Ymf262,
        Chip::Ymf278b,
    ]
}

#[test]
fn replaces_ymfm_backends_independently_by_chip_type() {
    let chips = ymfm_chips();
    let mut builder = VgmBuilder::new();
    for chip in &chips {
        builder.register_chip(chip.clone(), Instance::Primary, 4_000_000);
    }
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x28,
            value: 0x50,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x2a,
            value: 0x80,
        },
    );
    builder.add_vgm_command(WaitSamples(441));

    let created = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    for chip in &chips {
        let expected = chip.clone();
        let created = Arc::clone(&created);
        let writes = Arc::clone(&writes);
        factories.register(chip.clone(), move |config| {
            assert_eq!(config.chip(), &expected);
            assert_eq!(config.instance(), Instance::Primary);
            assert_eq!(config.clock(), 4_000_000);
            created.lock().unwrap().push(expected.clone());
            Ok(Box::new(FakeChip::new(
                config.chip().clone(),
                config.clock(),
                Arc::clone(&writes),
            )))
        });
    }

    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
    while player.render().unwrap().is_some() {}

    assert_eq!(*created.lock().unwrap(), chips);
    let writes = writes.lock().unwrap();
    assert_eq!(writes.len(), 2);
    assert!(writes.contains(&Chip::Ym2151));
    assert!(writes.contains(&Chip::Ym2612));
}

#[test]
fn rejects_factory_chip_type_mismatch() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Ym2151, |_config| {
        Ok(Box::new(FakeChip::new(
            Chip::Ym2612,
            4_000_000,
            Arc::new(Mutex::new(Vec::new())),
        )))
    });

    assert!(matches!(
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories),
        Err(VgmSequencerError::Chip(ChipError::ChipMismatch {
            expected: Chip::Ym2151,
            actual: Chip::Ym2612,
        }))
    ));
}

#[test]
fn mdx_sequencer_uses_registered_factories() {
    let created = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    let seen = Arc::clone(&created);
    factories.register(Chip::Ym2151, move |config| {
        seen.lock().unwrap().push(config.chip().clone());
        Ok(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::clone(&writes),
        )))
    });

    let mut player =
        MdxSequencer::from_mdx_with_factories(MDX.to_vec(), None, 44_100, 256, factories).unwrap();
    assert_eq!(*created.lock().unwrap(), [Chip::Ym2151]);
    while player.render().unwrap().is_some() {}
}

#[test]
#[cfg(not(feature = "pwm"))]
fn factory_can_replace_a_chip_with_its_builtin_feature_disabled() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Pwm, Instance::Primary, 4_000_000);
    builder.add_vgm_command(WaitSamples(441));

    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Pwm, |config| {
        Ok(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::new(Mutex::new(Vec::new())),
        )))
    });

    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
    assert!(player.render().unwrap().is_some());
}

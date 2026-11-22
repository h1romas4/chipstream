#![cfg(feature = "ay8910")]

use chipstream::chip::{Ay8910, ChipError, ChipRegisterWrite, SoundChip};
use chipstream::sequencer::{Chip, ChipFactoryRegistry, Instance, VgmSequencer, VgmSequencerError};
use soundlog::chip::Ay8910Spec;
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Ay8910StereoMask, WaitSamples};
use soundlog::vgm::header::Ay8910ChipType;

fn write(chip: &mut Ay8910, register: u8, value: u8) {
    chip.apply_register_write(ChipRegisterWrite::Ay8910(Ay8910Spec { register, value }))
        .unwrap();
}

#[test]
fn tone_noise_and_envelope_survive_chunking_and_snapshot() {
    let mut chip = Ay8910::new(1_764_000).unwrap();
    assert_eq!(chip.sample_rate_ratio().numerator(), 1_764_000);
    assert_eq!(chip.sample_rate_ratio().denominator().get(), 8);
    for (register, value) in [(0, 5), (6, 3), (7, 0x36), (8, 0x10), (11, 3), (13, 0x0a)] {
        write(&mut chip, register, value);
    }
    let snapshot = chip.save_state().unwrap();
    let mut whole = vec![0; 3000];
    chip.generate(&mut whole).unwrap();
    assert!(whole.chunks_exact(3).any(|frame| frame[0] != 0));
    assert!(
        whole
            .chunks_exact(3)
            .all(|frame| frame[1] == 0 && frame[2] == 0)
    );
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut chunks = vec![0; 3000];
    for chunk in chunks.chunks_mut(30) {
        chip.generate(chunk).unwrap();
    }
    assert_eq!(whole, chunks);
    chip.reset();
    let mut reset = [1; 30];
    chip.generate(&mut reset).unwrap();
    assert_eq!(reset, [0; 30]);
}

#[test]
fn envelope_shapes_repeat_or_hold() {
    let mut chip = Ay8910::new(1_764_000).unwrap();
    for (register, value) in [(7, 0x3f), (8, 0x10), (11, 1), (13, 0x0a)] {
        write(&mut chip, register, value);
    }
    let mut triangle = [0; 192];
    chip.generate(&mut triangle).unwrap();
    let levels: Vec<_> = triangle.chunks_exact(3).map(|frame| frame[0]).collect();
    assert!(levels[0] > levels[20]);
    assert!(levels[40] > levels[30]);
    write(&mut chip, 13, 0x09);
    let mut held = [0; 180];
    chip.generate(&mut held).unwrap();
    assert!(held[150..].iter().all(|&sample| sample == 0));
}

#[test]
fn invalid_input_leaves_state_and_buffer_unchanged() {
    assert!(matches!(
        Ay8910::new(7),
        Err(ChipError::InvalidClock { .. })
    ));
    let mut chip = Ay8910::new(1_764_000).unwrap();
    let snapshot = chip.save_state().unwrap();
    assert!(matches!(
        chip.apply_register_write(ChipRegisterWrite::Ay8910(Ay8910Spec {
            register: 16,
            value: 1
        })),
        Err(ChipError::InvalidRegister { .. })
    ));
    let mut invalid = [42; 4];
    assert!(chip.generate(&mut invalid).is_err());
    assert_eq!(invalid, [42; 4]);
    let mut after = [0; 30];
    let mut expected = [0; 30];
    chip.generate(&mut after).unwrap();
    chip.restore_state(snapshot.as_ref()).unwrap();
    chip.generate(&mut expected).unwrap();
    assert_eq!(after, expected);
}

#[test]
fn native_factory_is_explicit_and_checks_models_and_flags() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ay8910, Instance::Primary, 1_764_000);
    builder.add_chip_write(
        Instance::Primary,
        Ay8910Spec {
            register: 7,
            value: 0x3f,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ay8910Spec {
            register: 8,
            value: 15,
        },
    );
    builder.add_vgm_command(WaitSamples(100));
    let document = builder.finalize();
    let mut silent = VgmSequencer::new(document.clone(), 44_100, 64, Default::default()).unwrap();
    assert!(
        silent
            .render()
            .unwrap()
            .unwrap()
            .frames()
            .iter()
            .all(|frame| *frame == [0.0; 2])
    );
    for variant in [
        Ay8910ChipType::Ay8910,
        Ay8910ChipType::Ay8912,
        Ay8910ChipType::Ay8913,
    ] {
        let mut document = document.clone();
        document.header.ay_chip_type = variant;
        let mut factories = ChipFactoryRegistry::new();
        factories.register_native_ay8910();
        let mut player =
            VgmSequencer::new_with_factories(document, 44_100, 64, Default::default(), factories)
                .unwrap();
        assert!(
            player
                .render()
                .unwrap()
                .unwrap()
                .frames()
                .iter()
                .any(|frame| frame[0] > 0.0)
        );
    }

    for variant in [Ay8910ChipType::Ay8930, Ay8910ChipType::Ym2149] {
        let mut document = document.clone();
        document.header.ay_chip_type = variant;
        let mut factories = ChipFactoryRegistry::new();
        factories.register_native_ay8910();
        assert!(matches!(
            VgmSequencer::new_with_factories(document, 44_100, 64, Default::default(), factories),
            Err(VgmSequencerError::Chip(
                ChipError::InvalidConfiguration { .. }
            ))
        ));
    }
    let mut document = document;
    document.header.ay8910_flags.raw_output = true;
    let mut factories = ChipFactoryRegistry::new();
    factories.register_native_ay8910();
    assert!(matches!(
        VgmSequencer::new_with_factories(document, 44_100, 64, Default::default(), factories),
        Err(VgmSequencerError::Chip(
            ChipError::InvalidConfiguration { .. }
        ))
    ));
}

#[test]
fn native_factory_honors_stereo_mask_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ay8910, Instance::Primary, 1_764_000);
    builder.add_vgm_command(Ay8910StereoMask::from_mask(0x02));
    for (register, value) in [(0, 100), (7, 0x3e), (8, 15)] {
        builder.add_chip_write(Instance::Primary, Ay8910Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(1000));
    let mut document = builder.finalize();
    document.header.ay_chip_type = Ay8910ChipType::Ay8910;
    let mut factories = ChipFactoryRegistry::new();
    factories.register_native_ay8910();
    let mut player =
        VgmSequencer::new_with_factories(document, 44_100, 64, Default::default(), factories)
            .unwrap();
    let first = player.render().unwrap().unwrap();
    assert!(first.frames().iter().all(|frame| frame[0] == 0.0));
    assert!(first.frames().iter().any(|frame| frame[1] > 0.0));
    let checkpoint = player.checkpoint().unwrap();
    let expected = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(
        player.render().unwrap().unwrap().frames(),
        expected.frames()
    );
}

#[test]
fn native_fallback_preserves_an_existing_factory() {
    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::Ay8910, instance, 1_764_000);
    }
    builder.add_vgm_command(WaitSamples(100));
    let mut document = builder.finalize();
    document.header.ay_chip_type = Ay8910ChipType::Ay8910;
    let calls = std::rc::Rc::new(std::cell::Cell::new(0));
    let observed = std::rc::Rc::clone(&calls);
    let mut factories = ChipFactoryRegistry::new();
    factories.register_optional(Chip::Ay8910, move |config| {
        observed.set(observed.get() + 1);
        if config.instance() == Instance::Primary {
            Ok(Some(Box::new(chipstream::chip::YmfmChip::new_ym2149(
                config.clock(),
                false,
            )?)))
        } else {
            Ok(None)
        }
    });
    factories.register_native_ay8910_fallback();
    let player =
        VgmSequencer::new_with_factories(document, 44_100, 64, Default::default(), factories)
            .unwrap();
    assert_eq!(calls.get(), 2);
    assert_eq!(
        player
            .output_layout(Chip::Ay8910, Instance::Primary)
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        player
            .output_layout(Chip::Ay8910, Instance::Secondary)
            .unwrap()
            .len(),
        3
    );
}

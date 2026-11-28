#![cfg(feature = "k051649")]

use chipstream::chip::{ChipError, ChipRegisterWrite, K051649, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::K051649State;
use soundlog::chip::{Chip, PsgSpec, Scc1Spec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Instance, WaitSamples};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut K051649, port: u8, register: u8, value: u8) {
    chip.write(Scc1Spec {
        port,
        register,
        value,
    })
    .unwrap();
}

#[test]
fn waveform_period_sample_hold_and_frequency_halt_match_mame() {
    let mut chip = K051649::new(3_579_545).unwrap();
    assert_eq!(chip.sample_rate(), 3_579_545);
    for (port, register, value) in [
        (0, 0, 64),
        (0, 1, 192),
        (0, 2, 192),
        (2, 0, 15),
        (3, 0, 1),
        (1, 0, 9),
    ] {
        write(&mut chip, port, register, value);
    }
    let mut samples = [0; 11];
    chip.generate(&mut samples).unwrap();
    assert_eq!(&samples[..10], &[1920; 10]);
    assert_eq!(samples[10], -1920);
    write(&mut chip, 3, 0, 0);
    chip.generate(&mut samples[..9]).unwrap();
    assert_eq!(&samples[..9], &[-1920; 9]);
    chip.generate(&mut samples[..1]).unwrap();
    assert_eq!(samples[0], 0);
    write(&mut chip, 3, 0, 1);
    write(&mut chip, 1, 0, 9);
    chip.generate(&mut samples[..1]).unwrap();
    assert_eq!(samples[0], -1920);
    write(&mut chip, 1, 0, 8);
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [-1920; 11]);
}

#[test]
fn shared_and_independent_wave_ram_and_test_registers() {
    let mut chip = K051649::new(3_579_545).unwrap();
    for (port, register, value) in [
        (0, 0x60, 32),
        (4, 0x80, 224),
        (2, 4, 15),
        (3, 0, 16),
        (1, 8, 9),
    ] {
        write(&mut chip, port, register, value);
    }
    let mut sample = [0];
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample[0], -960);
    write(&mut chip, 5, 0, 0x80);
    write(&mut chip, 0, 0x60, 96);
    write(&mut chip, 1, 8, 9);
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample[0], -960);
    write(&mut chip, 5, 0, 0x40);
    write(&mut chip, 4, 0x80, 96);
    write(&mut chip, 1, 8, 9);
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample[0], -960);
    write(&mut chip, 5, 0, 0);
    write(&mut chip, 0, 0x60, 32);
    write(&mut chip, 1, 8, 9);
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample[0], 960);
    let mut next = [0; 10];
    chip.generate(&mut next).unwrap();
    assert_eq!(next[9], 0);
    write(&mut chip, 5, 0, 0x20);
    write(&mut chip, 1, 8, 9);
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample[0], 960);
}

#[test]
fn snapshot_reset_and_invalid_writes() {
    assert!(K051649::new(0).is_err());
    let mut chip = K051649::new(1_000_000).unwrap();
    for (port, register, value) in [(0, 0, 64), (2, 0, 15), (3, 0, 1), (1, 0, 9)] {
        write(&mut chip, port, register, value);
    }
    let mut sample = [0];
    chip.generate(&mut sample).unwrap();
    let state = chip.save_state().unwrap();
    assert!(matches!(
        chip.write(Scc1Spec {
            port: 4,
            register: 0xa0,
            value: 0
        }),
        Err(ChipError::InvalidRegister { .. })
    ));
    assert!(matches!(
        chip.write(Scc1Spec {
            port: 6,
            register: 0,
            value: 0
        }),
        Err(ChipError::InvalidRegisterBank { .. })
    ));
    assert!(matches!(
        chip.apply_register_write(ChipRegisterWrite::Psg(PsgSpec { value: 0 })),
        Err(ChipError::ChipMismatch { .. })
    ));
    assert!(matches!(
        chip.restore_state(&42_u32),
        Err(ChipError::InvalidStateSnapshot(Chip::K051649))
    ));
    chip.reset();
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample[0], 1920);
    write(&mut chip, 3, 0, 1);
    write(&mut chip, 1, 0, 9);
    chip.generate(&mut sample).unwrap();
    assert_eq!(sample[0], 1920);
    chip.restore_state(state.as_ref()).unwrap();
    let mut expected = [0; 20];
    chip.generate(&mut expected).unwrap();
    chip.restore_state(state.as_ref()).unwrap();
    let mut actual = [0; 20];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn vgm_second_chip_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::K051649, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::K051649, Instance::Secondary, 3_579_545);
    for index in 0..32 {
        builder.add_chip_write(
            Instance::Secondary,
            Scc1Spec {
                port: 0,
                register: index,
                value: if index < 16 { 64 } else { 192 },
            },
        );
    }
    for (port, register, value) in [(2, 0, 15), (1, 0, 0x80), (1, 1, 1), (3, 0, 1)] {
        builder.add_chip_write(
            Instance::Secondary,
            Scc1Spec {
                port,
                register,
                value,
            },
        );
    }
    builder.add_chip_write(
        Instance::Secondary,
        Scc1Spec {
            port: 4,
            register: 0x80,
            value: 9,
        },
    );
    builder.add_vgm_command(WaitSamples(800));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().try_into().unwrap();
    let mut player = VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<K051649State>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&events);
    player.on_write(move |instance, spec: Scc1Spec, _, changes| {
        received.borrow_mut().push((instance, spec, changes));
    });
    let first = player.render().unwrap().unwrap();
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0].abs() > 0.001 && frame[0] == frame[1])
    );
    assert!(
        events
            .borrow()
            .iter()
            .all(|(instance, _, _)| *instance == Instance::Secondary)
    );
    assert!(events.borrow().iter().any(|(_, _, changes)| matches!(
        changes.as_deref(),
        Some([StateEvent::KeyOn { channel: 0, .. }])
    )));
    assert!(events.borrow().iter().any(|(_, spec, changes)| {
        spec.port == 4 && changes.as_deref().is_none_or(|events| events.is_empty())
    }));

    let mut parsed = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    assert_eq!(parsed.render().unwrap().unwrap(), first);
    assert_eq!(parsed.render().unwrap().unwrap(), second);
}

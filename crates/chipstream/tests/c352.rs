#![cfg(feature = "c352")]

use chipstream::chip::{C352, ChipError, ChipRegisterWrite, SoundChip};
use chipstream::sequencer::{ChipBackendSettings, ChipFactoryRegistry, NativeOutput, VgmSequencer};
use soundlog::chip::state::C352State;
use soundlog::chip::{C352Spec, Chip, PsgSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

fn write(chip: &mut C352, register: u16, value: u16) {
    chip.write(C352Spec { register, value }).unwrap();
}

fn render(chip: &mut C352, frames: usize) -> Vec<i32> {
    let mut buffer = vec![0; frames * 4];
    chip.generate(&mut buffer).unwrap();
    buffer
}

#[test]
fn linear_loop_ramp_phase_and_key_execution() {
    let mut chip = C352::new(25_401_600).unwrap();
    assert_eq!(chip.sample_rate(), 44_100);
    chip.write_rom(0, &[0x10, 0x20, 0x30]).unwrap();
    for (register, value) in [
        (0, 0xffff),
        (1, 0xffff),
        (2, 0xffff),
        (3, 0x4286),
        (4, 0),
        (5, 0),
        (6, 2),
        (7, 0),
    ] {
        write(&mut chip, register, value);
    }
    assert_eq!(render(&mut chip, 2), [0; 8]);
    write(&mut chip, 0x202, 0);
    assert_eq!(
        render(&mut chip, 4),
        [
            2, -2, -2, -2, 8, -8, -8, -8, 18, -18, -18, -18, 8, -8, -8, -8
        ]
    );
    assert_eq!(chip.read_register(3) & 0x8800, 0x8800);
    write(&mut chip, 3, 0xa000);
    write(&mut chip, 0x202, 0);
    assert_eq!(chip.read_register(3) & 0xa000, 0);
    assert_eq!(render(&mut chip, 1), [0; 4]);
}

#[test]
fn interpolation_mulaw_and_noise_match_reference_samples() {
    let mut chip = C352::new(25_401_600).unwrap();
    chip.write_rom(0, &[0x40, 0xc0, 0]).unwrap();
    for (register, value) in [(0, 0xff00), (2, 0x8000), (3, 0x4002), (6, 2), (7, 0)] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0x202, 0);
    assert_eq!(render(&mut chip, 3), [3, 0, 0, 0, 15, 0, 0, 0, 0, 0, 0, 0]);

    chip.reset();
    chip.write_rom(0, &[0x7f, 0xff, 0]).unwrap();
    for (register, value) in [(0, 0xff00), (2, 0xffff), (3, 0x400e), (6, 2), (7, 0)] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0x202, 0);
    assert_eq!(render(&mut chip, 2), [15, 0, 0, 0, -31, 0, 0, 0]);

    chip.reset();
    for (register, value) in [(0, 0xff00), (2, 0xffff), (3, 0x4014)] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0x202, 0);
    assert_eq!(render(&mut chip, 3), [1, 0, 0, 0, 1, 0, 0, 0, -1, 0, 0, 0]);
}

#[test]
fn reverse_linked_and_one_shot_end() {
    let mut chip = C352::new(25_401_600).unwrap();
    chip.write_rom(0, &[0x10, 0x20, 0x30, 0x40]).unwrap();
    for (register, value) in [(0, 0xff00), (2, 0xffff), (3, 0x4007), (6, 2), (7, 0)] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0x202, 0);
    let output = render(&mut chip, 6);
    assert_eq!(
        [
            output[0], output[4], output[8], output[12], output[16], output[20]
        ],
        [2, 8, 18, 16, 10, 24]
    );

    chip.reset();
    for (register, value) in [(0, 0xff00), (2, 0xffff), (3, 0x4004), (6, 1)] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0x202, 0);
    let output = render(&mut chip, 3);
    assert_eq!([output[0], output[4], output[8]], [2, 0, 0]);
    assert_eq!(chip.read_register(3) & 0xa000, 0x2000);

    chip.reset();
    chip.write_rom(0x10000, &[0x30]).unwrap();
    for (register, value) in [
        (0, 0xff00),
        (2, 0xffff),
        (3, 0x4026),
        (5, 1),
        (6, 1),
        (7, 0),
    ] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0x202, 0);
    let output = render(&mut chip, 2);
    assert_eq!([output[0], output[4]], [4, 12]);
    assert_eq!(chip.read_register(3) & 0x0800, 0x0800);
}

#[test]
fn snapshot_reset_and_errors() {
    assert!(matches!(
        C352::new(575),
        Err(ChipError::InvalidClock { .. })
    ));
    assert!(matches!(
        C352::with_divider(1000, 0),
        Err(ChipError::InvalidConfiguration { .. })
    ));
    let mut chip = C352::with_divider(1_000_001, 4).unwrap();
    assert_eq!(chip.sample_rate_ratio().numerator(), 1_000_001);
    assert_eq!(chip.sample_rate_ratio().denominator().get(), 4);
    chip.write_rom(0, &[0x7f; 4]).unwrap();
    for (register, value) in [(0, 0xff00), (2, 0xffff), (3, 0x4006), (6, 3)] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0x202, 0);
    render(&mut chip, 5);
    let snapshot = chip.save_state().unwrap();
    let expected = render(&mut chip, 20);
    chip.restore_state(snapshot.as_ref()).unwrap();
    assert_eq!(render(&mut chip, 20), expected);
    chip.reset();
    assert_eq!(chip.read_register(3), 0);
    for (register, value) in [(0, 0xff00), (2, 0xffff), (3, 0x4006), (6, 3)] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0x202, 0);
    assert_eq!(render(&mut chip, 1)[0], 15);
    let mut invalid = [42; 3];
    assert!(matches!(
        chip.generate(&mut invalid),
        Err(ChipError::InvalidBufferLength { .. })
    ));
    assert_eq!(invalid, [42; 3]);
    assert!(matches!(
        chip.write_rom(usize::MAX, &[1]),
        Err(ChipError::RomTooLarge)
    ));
    assert!(matches!(
        chip.apply_register_write(ChipRegisterWrite::Psg(PsgSpec { value: 0 })),
        Err(ChipError::ChipMismatch { .. })
    ));
    assert!(matches!(
        chip.restore_state(&42_u32),
        Err(ChipError::InvalidStateSnapshot(Chip::C352))
    ));
}

fn document(disable_rear: bool) -> soundlog::vgm::VgmDocument {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::C352, Instance::Primary, 25_401_600);
    builder.register_chip(Chip::C352, Instance::Secondary, 25_401_600);
    let mut data = Vec::new();
    data.extend_from_slice(&256_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x7f; 256]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x92,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [
        (1, 0xff00),
        (2, 0xffff),
        (3, 0x4006),
        (6, 255),
        (7, 0),
        (0x202, 0x1234),
    ] {
        builder.add_chip_write(Instance::Secondary, C352Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(600));
    let mut doc = builder.finalize();
    doc.header.c352_clock_divider = 144;
    if disable_rear {
        doc.header.c352_clock |= 0x8000_0000;
    }
    doc
}

#[test]
fn vgm_wide_writes_rear_output_rom_and_checkpoint() {
    let doc = document(false);
    let bytes: Vec<u8> = doc.clone().try_into().unwrap();
    let mut player = VgmSequencer::new(doc, 44_100, 256, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<C352State>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&events);
    player.on_write(move |instance, spec: C352Spec, _, _| {
        received.borrow_mut().push((instance, spec))
    });
    assert_eq!(
        player
            .output_layout(Chip::C352, Instance::Secondary)
            .unwrap(),
        &[
            NativeOutput::Left,
            NativeOutput::Right,
            NativeOutput::LeftAux,
            NativeOutput::RightAux
        ]
    );
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] > 0.05 && frame[1] == 0.0)
    );
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    let expected = 8096.0 / 32768.0;
    assert!((second.frames()[100][0] - expected).abs() < 0.0001);
    assert_eq!(second.frames()[100][1], 0.0);
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    assert!(
        events
            .borrow()
            .iter()
            .any(|(instance, spec)| *instance == Instance::Secondary
                && spec.register == 0x202
                && spec.value == 0x1234)
    );
    let mut from_bytes = VgmSequencer::from_vgm(bytes, 44_100, 256, Default::default()).unwrap();
    assert_eq!(from_bytes.render().unwrap().unwrap(), first);
    assert_eq!(from_bytes.render().unwrap().unwrap(), second);
    let mut muted = VgmSequencer::new(document(true), 44_100, 256, Default::default()).unwrap();
    assert!(
        muted
            .render()
            .unwrap()
            .unwrap()
            .frames()
            .iter()
            .all(|frame| *frame == [0.0, 0.0])
    );
}

#[test]
fn factory_receives_divider_and_rear_flag() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::C352, move |config| {
        captured
            .lock()
            .unwrap()
            .push((config.instance(), *config.settings()));
        Ok(Box::new(C352::with_divider(config.clock(), 576).unwrap()))
    });
    let player = VgmSequencer::new_with_factories(
        document(true),
        44_100,
        128,
        Default::default(),
        factories,
    )
    .unwrap();
    drop(player);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (
                Instance::Primary,
                ChipBackendSettings::C352 {
                    divider: 576,
                    disable_rear: true
                }
            ),
            (
                Instance::Secondary,
                ChipBackendSettings::C352 {
                    divider: 576,
                    disable_rear: true
                }
            ),
        ]
    );
}

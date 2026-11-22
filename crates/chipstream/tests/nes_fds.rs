#![cfg(feature = "nes_fds")]

use chipstream::chip::{NesApu, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, NesApuSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Instance, WaitSamples};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut NesApu, register: u8, value: u8) {
    chip.write(NesApuSpec { register, value }).unwrap();
}

fn setup(mode: usize) -> NesApu {
    let mut chip = NesApu::with_fds(if mode == 1 { 1_662_607 } else { 1_789_773 }).unwrap();
    write(&mut chip, 0x23, 2);
    write(&mut chip, 0x89, 128);
    for i in 0..64 {
        write(&mut chip, 0x40 + i, i);
    }
    write(&mut chip, 0x87, 128);
    for i in 0..32 {
        write(&mut chip, 0x88, i & 7);
    }
    for (register, value) in [
        (0x84, 0x88),
        (0x85, 0),
        (0x86, 0x40),
        (0x87, if mode == 0 { 128 } else { 1 }),
        (0x89, 0),
        (0x80, 0x9f),
        (0x82, 0x7b),
        (0x83, 3),
    ] {
        write(&mut chip, register, value);
    }
    if mode == 2 {
        for (register, value) in [(0x8a, 1), (0x80, 0x40), (0x84, 0x42), (0x85, 0x3e)] {
            write(&mut chip, register, value);
        }
    }
    if mode == 3 {
        for (register, value) in [(0x15, 1), (0, 0xbf), (2, 0x40), (3, 8)] {
            write(&mut chip, register, value);
        }
    }
    chip
}

#[test]
fn fds_matches_native_reference() {
    // Tetorica nes_core.cpp b4f2543c, 44.1 kHz; NTSC, PAL, envelopes, and APU mix.
    let expected = [
        [0, 0, 949, -1146, 477, 564, 628, -4033, -2953],
        [0, 0, 867, 4777, -402, -344, -300, -1004, 2177],
        [0, 38, 2108, 4008, -5330, -5413, -5470, 1768, 655],
        [3964, 3955, 842, 1949, -4127, -2836, -2921, 1251, 1681],
    ];
    let indexes = [0, 1, 16, 128, 999, 1000, 1001, 5000, 10000];
    for (mode, expected) in expected.iter().enumerate() {
        let mut chip = setup(mode);
        let mut samples = [0; 10001];
        chip.generate(&mut samples[..1000]).unwrap();
        if mode >= 2 {
            for (register, value) in [(0x23, 0), (0x80, 0x80), (0x23, 2), (0x89, 2), (0x82, 0xd1)] {
                write(&mut chip, register, value);
            }
        }
        chip.generate(&mut samples[1000..]).unwrap();
        for (&index, &target) in indexes.iter().zip(expected) {
            assert!(
                (samples[index] - target).abs() <= 1,
                "mode {mode}, sample {index}: {} != {target}",
                samples[index]
            );
        }
    }
}

#[test]
fn fds_reset_and_snapshot_preserve_configuration_and_state() {
    let mut chip = setup(2);
    chip.generate(&mut [0; 100]).unwrap();
    let state = chip.save_state().unwrap();
    let mut expected = [0; 1000];
    chip.generate(&mut expected).unwrap();
    write(&mut chip, 0x89, 0x80);
    write(&mut chip, 0x40, 63);
    chip.restore_state(state.as_ref()).unwrap();
    let mut actual = [0; 1000];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
    chip.reset();
    write(&mut chip, 0x89, 0x80);
    write(&mut chip, 0x40, 63);
    assert!(
        NesApu::new(1_789_773)
            .unwrap()
            .write(NesApuSpec {
                register: 0x89,
                value: 0
            })
            .is_err()
    );
}

#[test]
fn io_and_wave_write_gates_preserve_playback() {
    let mut baseline = setup(1);
    let mut guarded = setup(1);
    write(&mut guarded, 0x40, 63);
    write(&mut guarded, 0x23, 0);
    write(&mut guarded, 0x80, 0x80);
    write(&mut guarded, 0x89, 128);
    write(&mut guarded, 0x40, 63);
    write(&mut guarded, 0x23, 2);
    let mut expected = [0; 1000];
    let mut actual = [0; 1000];
    baseline.generate(&mut expected).unwrap();
    guarded.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn dual_vgm_fds_roundtrip_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::NesApu, instance, 1_789_773 | 0x8000_0000);
        for (register, value) in [(0x23, 2), (0x89, 128)] {
            builder.add_chip_write(instance, NesApuSpec { register, value });
        }
        for i in 0..64 {
            builder.add_chip_write(
                instance,
                NesApuSpec {
                    register: 0x40 + i,
                    value: if instance == Instance::Primary {
                        i
                    } else {
                        63 - i
                    },
                },
            );
        }
        for (register, value) in [(0x89, 0), (0x80, 0x9f), (0x82, 0x7b), (0x83, 3)] {
            builder.add_chip_write(instance, NesApuSpec { register, value });
        }
    }
    builder.add_vgm_command(WaitSamples(1000));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().into();
    let mut direct = VgmSequencer::new(document.clone(), 44_100, 57, Default::default()).unwrap();
    let mut serialized =
        VgmSequencer::from_vgm(bytes.clone(), 44_100, 57, Default::default()).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let events = Rc::clone(&seen);
    serialized.on_write(move |instance, spec: NesApuSpec, _, _| {
        if matches!(spec.register, 0x23 | 0x80 | 0x89) {
            events.borrow_mut().push((instance, spec.register));
        }
    });
    let first = direct.render().unwrap().unwrap();
    assert_eq!(first, serialized.render().unwrap().unwrap());
    assert!(first.frames().iter().any(|frame| frame[0].abs() > 0.001));
    let checkpoint = serialized.checkpoint().unwrap();
    let next = serialized.render().unwrap().unwrap();
    assert_eq!(next, direct.render().unwrap().unwrap());
    serialized.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(next, serialized.render().unwrap().unwrap());
    for instance in [Instance::Primary, Instance::Secondary] {
        assert!(seen.borrow().contains(&(instance, 0x23)));
        assert!(seen.borrow().contains(&(instance, 0x80)));
        assert!(seen.borrow().contains(&(instance, 0x89)));
    }

    let render = |mut player: VgmSequencer| {
        player
            .set_chip_gain(Chip::NesApu, Instance::Primary, 0.0)
            .unwrap();
        let mut frames = Vec::new();
        while let Some(block) = player.render().unwrap() {
            frames.extend_from_slice(block.frames());
        }
        frames
    };
    let primary_muted =
        render(VgmSequencer::new(document, 44_100, 57, Default::default()).unwrap());
    assert!(primary_muted.iter().any(|frame| frame[0].abs() > 0.001));
    assert_eq!(
        primary_muted,
        render(VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap())
    );
}

#![cfg(feature = "gb_dmg")]

use chipstream::chip::{ChipError, GbDmg, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, GbDmgSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Instance, WaitSamples};

fn write(chip: &mut GbDmg, register: u8, value: u8) {
    chip.write(GbDmgSpec { register, value }).unwrap();
}

#[test]
fn multi_channel_output_matches_tetorica_reference() {
    let expected = [
        [24576, 16384],
        [-4096, 5461],
        [0, 5461],
        [1638, 5461],
        [4915, 5461],
        [13107, 3276],
        [-2457, 5461],
        [-1235, 5461],
        [-6553, 3276],
        [6144, 8192],
        [4505, -6007],
        [1228, 8192],
        [4505, 8192],
        [-15155, -6007],
        [7782, 8192],
        [-1228, 16384],
        [-2048, 16384],
        [4505, 16384],
        [-5324, 1092],
        [2048, 15603],
        [409, 5966],
        [-3686, 16384],
        [6144, 8744],
        [-3686, 0],
        [409, 3276],
        [-1270, 3276],
        [5324, -11021],
        [4505, 4662],
        [-2048, 3276],
        [1228, 12221],
        [1638, 16384],
        [0, -2376],
        [9011, 16384],
        [-2457, 169],
        [819, 4369],
        [-7372, -9830],
        [2457, 13764],
        [4915, 2184],
        [4096, 9534],
        [4505, -9830],
        [-5324, 12080],
        [2048, 1092],
        [409, 16384],
        [3686, -331],
        [8192, 8192],
        [8192, 8192],
        [-8192, -8192],
        [-8192, -8192],
        [8192, 8192],
        [-8192, -8192],
        [-8192, -8192],
        [8192, 8192],
        [8192, 8192],
        [-8192, -8192],
        [8192, 8192],
        [8192, 8192],
        [-8192, -8192],
        [8192, 8192],
        [8192, 8192],
    ];
    let mut chip = GbDmg::new(4_194_304).unwrap();
    write(&mut chip, 0x16, 0x80);
    for i in 0..16 {
        write(&mut chip, 0x20 + i, (i << 4) | (15 - i));
    }
    for (register, value) in [
        (0x14, 0x75),
        (0x15, 0xf3),
        (0x00, 0x23),
        (0x01, 0x80),
        (0x02, 0xa2),
        (0x03, 0x41),
        (0x04, 0x86),
        (0x06, 0x40),
        (0x07, 0xb9),
        (0x08, 0xe7),
        (0x09, 0x83),
        (0x0a, 0x80),
        (0x0b, 0),
        (0x0c, 0x20),
        (0x0d, 0x81),
        (0x0e, 0x84),
        (0x10, 0),
        (0x11, 0xd3),
        (0x12, 0x34),
        (0x13, 0x80),
    ] {
        write(&mut chip, register, value);
    }
    let mut frame = [0; 2];
    for i in 0..8000 {
        let changes: &[(u8, u8)] = match i {
            2000 => &[(0x15, 0x5a), (0x12, 0x1d)],
            4000 => &[(0x04, 0xc5), (0x09, 0xc4)],
            6000 => &[
                (0x16, 0),
                (0x16, 0x80),
                (0x14, 0x77),
                (0x15, 0x11),
                (0x01, 0x80),
                (0x02, 0xf0),
                (0x04, 0x83),
            ],
            _ => &[],
        };
        for &(register, value) in changes {
            write(&mut chip, register, value);
        }
        chip.generate(&mut frame).unwrap();
        if i % 137 == 0 {
            let reference = expected[i / 137];
            assert!(
                (frame[0] - reference[0]).abs() <= 1,
                "frame {i}: {frame:?} vs {reference:?}"
            );
            assert!(
                (frame[1] - reference[1]).abs() <= 1,
                "frame {i}: {frame:?} vs {reference:?}"
            );
        }
    }
}

fn document() -> soundlog::vgm::VgmDocument {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::GbDmg, Instance::Primary, 4_194_304);
    builder.register_chip(Chip::GbDmg, Instance::Secondary, 4_194_304);
    for (register, value) in [
        (0x16, 0x80),
        (0x14, 0x77),
        (0x15, 0x11),
        (0x01, 0x80),
        (0x02, 0xf0),
        (0x03, 0x80),
        (0x04, 0x87),
    ] {
        builder.add_chip_write(Instance::Secondary, GbDmgSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(2500));
    builder.finalize()
}

#[test]
fn vgm_secondary_document_bytes_and_checkpoint() {
    let doc = document();
    let bytes: Vec<u8> = doc.clone().into();
    let mut doc_player = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
    let mut byte_player = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    let a = doc_player.render().unwrap().unwrap();
    let b = byte_player.render().unwrap().unwrap();
    assert_eq!(a, b);
    assert!(a.frames().iter().any(|frame| frame[0].abs() > 0.01));
    let state = byte_player.checkpoint().unwrap();
    let expected = byte_player.render().unwrap();
    byte_player.restore_checkpoint(&state).unwrap();
    assert_eq!(expected, byte_player.render().unwrap());
    byte_player
        .set_chip_gain(Chip::GbDmg, Instance::Secondary, 0.0)
        .unwrap();
    assert!(
        byte_player
            .render()
            .unwrap()
            .unwrap()
            .frames()
            .iter()
            .all(|frame| *frame == [0.0; 2])
    );
}

fn start_wave(chip: &mut GbDmg) {
    for (register, value) in [
        (0x16, 0x80),
        (0x14, 0x77),
        (0x15, 0x44),
        (0x0a, 0x80),
        (0x0c, 0x20),
        (0x0d, 0),
        (0x0e, 0x80),
    ] {
        write(chip, register, value);
    }
}

#[test]
fn wave_ram_access_snapshot_and_reset() {
    let mut chip = GbDmg::new(4_194_304).unwrap();
    for register in 0x20..=0x2f {
        write(&mut chip, register, 0xff);
    }
    start_wave(&mut chip);
    let mut first = [0; 2];
    chip.generate(&mut first).unwrap();
    let state = chip.save_state().unwrap();
    let mut other = GbDmg::new(4_194_304).unwrap();
    other.restore_state(state.as_ref()).unwrap();
    // At this point no wave sample has yet been fetched, so active RAM is locked.
    write(&mut chip, 0x20, 0);
    let mut actual = [0; 400];
    let mut expected = [0; 400];
    chip.generate(&mut actual).unwrap();
    other.generate(&mut expected).unwrap();
    assert_eq!(actual, expected);
    assert!(actual.chunks_exact(2).any(|frame| frame[0] < -1000));
    chip.reset();
    let mut fresh = GbDmg::new(4_194_304).unwrap();
    start_wave(&mut chip);
    start_wave(&mut fresh);
    chip.generate(&mut actual).unwrap();
    fresh.generate(&mut expected).unwrap();
    assert_eq!(actual, expected);
    assert!(matches!(
        chip.write(GbDmgSpec {
            register: 0x30,
            value: 0
        }),
        Err(ChipError::InvalidRegister { .. })
    ));
}

#[test]
fn vgm_callbacks_exclude_wave_ram_from_noise_state_changes() {
    use soundlog::chip::state::GbDmgState;
    use std::{cell::RefCell, rc::Rc};
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::GbDmg, Instance::Primary, 4_194_304);
    for (register, value) in [
        (0x16, 0x80),
        (0x11, 0xf0),
        (0x12, 0x20),
        (0x13, 0x80),
        (0x20, 0xff),
    ] {
        builder.add_chip_write(Instance::Primary, GbDmgSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(100));
    let events = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&events);
    let mut player =
        VgmSequencer::new(builder.finalize(), 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<GbDmgState>();
    player.on_write::<GbDmgSpec, _>(move |_, spec, _, state| {
        observed.borrow_mut().push((spec.register, state));
    });
    player.render().unwrap();
    let events = events.borrow();
    assert!(events.iter().any(|(register, _)| *register == 0x13));
    assert!(events.iter().any(
        |(register, changes)| *register == 0x20 && changes.as_ref().is_some_and(Vec::is_empty)
    ));
}

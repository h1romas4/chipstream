#![cfg(feature = "wonderswan")]

use chipstream::chip::{ChipError, SoundChip, WonderSwan};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, WonderSwanRegSpec, WonderSwanSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Instance, WaitSamples};

fn reg(chip: &mut WonderSwan, register: u8, value: u8) {
    chip.write(WonderSwanRegSpec { register, value }).unwrap();
}

#[test]
fn output_matches_tetorica_reference() {
    let expected = [
        [0, 0],
        [12000, 9792],
        [11904, 9376],
        [14016, 8992],
        [7200, 11520],
        [3840, 2176],
        [8064, 4864],
        [11040, 9376],
        [7584, 6912],
        [9888, 6592],
        [9024, 5024],
        [9792, 8896],
        [13856, 12256],
        [14048, 12608],
        [14816, 10496],
        [13568, 11808],
        [6560, 6816],
        [19616, 13408],
        [11168, 10080],
        [12416, 8064],
        [19136, 11360],
        [18656, 13216],
        [11072, 11232],
        [12704, 10464],
        [3936, 4448],
        [16416, 9760],
        [8352, 3104],
        [11616, 6720],
        [13344, 11680],
        [5664, 7520],
        [14592, 10816],
        [17184, 11328],
        [7776, 8224],
        [14688, 8512],
        [13536, 10400],
        [4992, 3488],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [15264, 11840],
        [15264, 9632],
        [9504, 9760],
        [3360, 3328],
        [9600, 10208],
        [10080, 5792],
        [9888, 5056],
        [12480, 12000],
        [15840, 10752],
        [11232, 10496],
    ];
    let mut chip = WonderSwan::new(3_072_000).unwrap();
    for i in 0..64 {
        chip.write(WonderSwanSpec {
            offset: 0x100 + i,
            value: (i * 37 + 13) as u8,
        })
        .unwrap();
    }
    for (register, value) in [
        (0x0f, 4),
        (0x00, 0x31),
        (0x01, 6),
        (0x02, 0xd0),
        (0x03, 7),
        (0x04, 0x55),
        (0x05, 6),
        (0x06, 0xb0),
        (0x07, 7),
        (0x08, 0xf3),
        (0x09, 0x6a),
        (0x0a, 0x9e),
        (0x0b, 0xc5),
        (0x0c, 0xf9),
        (0x0d, 0),
        (0x0e, 0x1b),
        (0x11, 8),
        (0x10, 0xcf),
    ] {
        reg(&mut chip, register, value);
    }
    let mut frame = [0; 2];
    for i in 0..5000 {
        let writes: &[(u8, u8)] = match i {
            1200 => &[(0x10, 0xef), (0x14, 0x0b), (0x09, 0x9a)],
            2400 => &[(0x0e, 0x16), (0x10, 0xcf)],
            3600 => &[(0x11, 0)],
            4000 => &[(0x11, 8), (0x10, 0x0f)],
            _ => &[],
        };
        for &(register, value) in writes {
            reg(&mut chip, register, value);
        }
        chip.generate(&mut frame).unwrap();
        if i % 101 == 0 {
            assert_eq!(frame, expected[i / 101], "frame {i}");
        }
    }
}

#[test]
fn ram_snapshot_reset_and_errors() {
    let mut chip = WonderSwan::new(3_072_000).unwrap();
    chip.write_ram(0, &[0xff; 16]).unwrap();
    for (register, value) in [(0x11, 8), (0x08, 0xff), (0x10, 1)] {
        reg(&mut chip, register, value);
    }
    let state = chip.save_state().unwrap();
    let mut expected = [0; 200];
    chip.generate(&mut expected).unwrap();
    assert!(expected.iter().any(|&sample| sample > 0));
    chip.restore_state(state.as_ref()).unwrap();
    let mut actual = [0; 200];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
    assert!(matches!(
        chip.write_ram(0x3fff, &[1, 2]),
        Err(ChipError::RamOutOfRange { .. })
    ));
    chip.write(WonderSwanSpec {
        offset: 0x4000,
        value: 1,
    })
    .unwrap();
    chip.reset();
    chip.generate(&mut actual).unwrap();
    assert!(actual.iter().all(|&sample| sample == 0));
    assert!(WonderSwan::new(127).is_err());
}

#[test]
fn speaker_output_and_headphone_selection() {
    let mut chip = WonderSwan::new(3_072_000).unwrap();
    chip.write_ram(0, &[0xff; 16]).unwrap();
    for (register, value) in [(0x11, 7), (0x08, 0xff), (0x10, 1)] {
        reg(&mut chip, register, value);
    }
    let mut frames = [0; 40];
    chip.generate(&mut frames).unwrap();
    assert_eq!(&frames[38..], &[7168, 7168]);
    reg(&mut chip, 0x11, 1);
    chip.generate(&mut frames[..2]).unwrap();
    assert_eq!(&frames[..2], &[24832, 24832]);
    reg(&mut chip, 0x11, 9);
    chip.generate(&mut frames[..2]).unwrap();
    assert_eq!(&frames[..2], &[7200, 7200]);
    reg(&mut chip, 0x11, 0);
    chip.generate(&mut frames[..2]).unwrap();
    assert_eq!(&frames[..2], &[0, 0]);
}

#[test]
fn vgm_memory_register_callbacks_secondary_and_checkpoint() {
    use soundlog::chip::state::WonderSwanState;
    use std::{cell::RefCell, rc::Rc};

    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::WonderSwan, Instance::Primary, 3_072_000);
    builder.register_chip(Chip::WonderSwan, Instance::Secondary, 3_072_000);
    for offset in 0..16 {
        builder.add_chip_write(
            Instance::Secondary,
            WonderSwanSpec {
                offset,
                value: 0xff,
            },
        );
    }
    builder.add_chip_write(
        Instance::Secondary,
        WonderSwanSpec {
            offset: 0x88,
            value: 0xff,
        },
    );
    for (register, value) in [(0x11, 8), (0x08, 0xff), (0x10, 1)] {
        builder.add_chip_write(Instance::Secondary, WonderSwanRegSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(2500));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().try_into().unwrap();
    let mut doc_player = VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap();
    let mut player = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = events.clone();
    let register_events = Rc::new(RefCell::new(Vec::new()));
    let captured_registers = register_events.clone();
    player
        .track_state::<WonderSwanState>(Instance::Secondary, 3_072_000.0)
        .unwrap();
    player.on_write(move |instance, spec: WonderSwanSpec, _, changes| {
        captured
            .borrow_mut()
            .push((instance, spec.offset, changes.unwrap().len()));
    });
    player.on_write(move |_, spec: WonderSwanRegSpec, _, changes| {
        captured_registers
            .borrow_mut()
            .push((spec.register, changes.unwrap().len()));
    });
    let first = player.render().unwrap().unwrap();
    assert_eq!(first, doc_player.render().unwrap().unwrap());
    assert!(first.frames().iter().any(|frame| frame[0] > 0.01));
    assert_eq!(events.borrow().len(), 17);
    assert!(
        events
            .borrow()
            .iter()
            .all(|&(i, _, changes)| i == Instance::Secondary && changes == 0)
    );
    assert!(register_events.borrow().contains(&(0x10, 1)));
    let state = player.checkpoint().unwrap();
    let expected = player.render().unwrap();
    player.restore_checkpoint(&state).unwrap();
    assert_eq!(player.render().unwrap(), expected);
}

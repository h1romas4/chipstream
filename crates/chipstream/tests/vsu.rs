#![cfg(feature = "vsu")]

use chipstream::chip::{SoundChip, Vsu};
use soundlog::chip::VsuSpec;

fn write(chip: &mut Vsu, address: u16, value: u8) {
    chip.write(VsuSpec {
        offset: address / 4,
        value,
    })
    .unwrap();
}

#[test]
fn output_matches_rustual_boy_reference() {
    let expected = [
        [3552, 1440],
        [8832, 4384],
        [3328, 1184],
        [6272, 2336],
        [6592, 2624],
        [9536, 3680],
        [8256, 6080],
        [7744, 5088],
        [3616, 1120],
        [8352, 3424],
        [5856, 2720],
        [10368, 5632],
        [8576, 5344],
        [9600, 4672],
        [5760, 1184],
        [3552, 672],
        [3904, 2688],
        [9600, 4896],
        [8672, 4960],
        [6336, 1056],
        [3424, 2080],
        [6528, 3264],
        [5536, 3008],
        [5184, 3776],
        [8288, 3936],
        [11680, 5376],
        [6208, 2432],
        [8512, 4064],
        [4608, 3040],
        [7680, 3584],
        [7552, 5088],
        [7104, 2432],
        [6208, 3648],
        [5984, 3808],
        [5888, 768],
        [6720, 4480],
        [3904, 1344],
        [2560, 1312],
        [3360, 480],
        [8608, 4832],
        [5312, 4000],
        [6880, 4352],
        [7296, 3616],
        [5920, 4544],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
        [0, 0],
    ];
    let mut chip = Vsu::new(5_000_000).unwrap();
    for bank in 0..5 {
        for i in 0..32 {
            write(
                &mut chip,
                bank * 0x80 + i * 4,
                ((i * 13 + bank * 7) & 63) as u8,
            );
        }
    }
    for i in 0..32 {
        write(&mut chip, 0x280 + i * 4, (i * 17) as u8);
    }
    for ch in 0..6 {
        let base = 0x400 + ch * 0x40;
        for (off, value) in [
            (4, (0xf3_u8).wrapping_sub(ch as u8 * 17)),
            (8, (0x77_u8).wrapping_add(ch as u8 * 13)),
            (12, 7),
            (16, 0xc2),
            (
                20,
                if ch == 4 {
                    0x71
                } else if ch == 5 {
                    0x41
                } else {
                    1
                },
            ),
            (24, ch as u8 % 5),
            (28, if ch == 4 { 0x19 } else { 0 }),
            (0, 0x80),
        ] {
            write(&mut chip, base + off, value);
        }
    }
    let mut frame = [0; 2];
    for i in 0..8000 {
        match i {
            2000 => {
                write(&mut chip, 0x514, 0x61);
                write(&mut chip, 0x51c, 0x91);
            }
            4000 => {
                write(&mut chip, 0x554, 0x61);
                write(&mut chip, 0x408, 0xe7);
            }
            6000 => write(&mut chip, 0x580, 1),
            _ => {}
        }
        chip.generate(&mut frame).unwrap();
        if i % 137 == 0 {
            assert_eq!(frame, expected[i / 137], "frame {i}");
        }
    }
}

#[test]
fn ram_restrictions_snapshot_reset_and_errors() {
    let mut chip = Vsu::new(5_000_000).unwrap();
    for i in 0..32 {
        write(&mut chip, i * 4, 63);
    }
    for (address, value) in [(0x404, 0xff), (0x410, 0xf0), (0x400, 0x80)] {
        write(&mut chip, address, value);
    }
    let mut control = chip.clone();
    for i in 0..32 {
        write(&mut chip, i * 4, 0);
    }
    let state = chip.save_state().unwrap();
    let mut expected = [0; 400];
    chip.generate(&mut expected).unwrap();
    assert!(expected.iter().any(|&value| value != 0));
    let mut unchanged = [0; 400];
    control.generate(&mut unchanged).unwrap();
    assert_eq!(unchanged, expected);
    chip.restore_state(state.as_ref()).unwrap();
    let mut actual = [0; 400];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
    assert!(chip.generate(&mut [0; 3]).is_err());
    chip.reset();
    chip.generate(&mut actual).unwrap();
    assert!(actual.iter().all(|&value| value == 0));
    assert!(Vsu::new(119).is_err());
}

#[test]
fn interval_expiration_silences_channel() {
    let mut chip = Vsu::new(5_000_000).unwrap();
    for i in 0..32 {
        write(&mut chip, i * 4, 0xff);
    }
    for (address, value) in [(0x404, 0xf0), (0x410, 0xf0), (0x400, 0xa0)] {
        write(&mut chip, address, value);
    }
    let mut before = [0; 318];
    chip.generate(&mut before).unwrap();
    assert!(
        before
            .chunks_exact(2)
            .all(|frame| frame[0] > 0 && frame[1] == 0)
    );
    let mut after = [1; 2];
    chip.generate(&mut after).unwrap();
    assert_eq!(after, [0, 0]);
}

#[test]
fn vgm_playback_callbacks_and_checkpoint() {
    use chipstream::sequencer::VgmSequencer;
    use soundlog::chip::{Chip, state::VsuState};
    use soundlog::vgm::{
        VgmBuilder,
        command::{Instance, WaitSamples},
    };
    use std::{cell::RefCell, rc::Rc};

    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Vsu, Instance::Primary, 5_000_000);
    builder.register_chip(Chip::Vsu, Instance::Secondary, 5_000_000);
    for offset in 0..32 {
        builder.add_chip_write(Instance::Secondary, VsuSpec { offset, value: 63 });
    }
    for (offset, value) in [
        (0x101, 0xff),
        (0x102, 0xa0),
        (0x103, 7),
        (0x104, 0xf0),
        (0x100, 0x80),
    ] {
        builder.add_chip_write(Instance::Secondary, VsuSpec { offset, value });
    }
    builder.add_vgm_command(WaitSamples(2500));
    let bytes: Vec<u8> = builder.finalize().try_into().unwrap();
    let mut player = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    player
        .track_state::<VsuState>(Instance::Secondary, 5_000_000.0)
        .unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&events);
    player.on_write(move |instance, write: VsuSpec, _, changes| {
        captured
            .borrow_mut()
            .push((instance, write.offset, changes.unwrap().len()));
    });
    let first = player.render().unwrap().unwrap();
    assert!(first.frames().iter().any(|frame| frame[0] > 0.01));
    assert!(events.borrow().contains(&(Instance::Secondary, 0x100, 1)));
    let state = player.checkpoint().unwrap();
    let expected = player.render().unwrap();
    player.restore_checkpoint(&state).unwrap();
    assert_eq!(player.render().unwrap(), expected);
}

#![cfg(feature = "k053260")]

use chipstream::chip::{ChipError, ChipRegisterWrite, K053260, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::state::K053260State;
use soundlog::chip::{Chip, K053260Spec, PsgSpec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut K053260, register: u8, value: u8) {
    chip.write(K053260Spec { register, value }).unwrap();
}

fn configure_voice(chip: &mut K053260, index: u8, start: u32, length: u16, pitch: u16, volume: u8) {
    let base = 8 + index * 8;
    for (offset, value) in [
        (0, pitch as u8),
        (1, (pitch >> 8) as u8),
        (2, length as u8),
        (3, (length >> 8) as u8),
        (4, start as u8),
        (5, (start >> 8) as u8),
        (6, (start >> 16) as u8),
        (7, volume),
    ] {
        write(chip, base + offset, value);
    }
}

fn samples(chip: &mut K053260, frames: usize) -> Vec<i32> {
    let mut buffer = vec![0; frames * 2];
    chip.generate(&mut buffer).unwrap();
    buffer
}

#[test]
fn pcm_preincrement_pan_pitch_and_end() {
    let mut chip = K053260::new(3_579_545).unwrap();
    assert_eq!(chip.sample_rate(), 3_579_545 / 64);
    chip.write_rom(0, &[0x55, 0x7f, 0x80, 0x40]).unwrap();
    configure_voice(&mut chip, 0, 0, 3, 0xfc0, 64);
    write(&mut chip, 0x2c, 1);
    write(&mut chip, 0x2f, 2);
    write(&mut chip, 0x28, 1);
    assert_eq!(samples(&mut chip, 4), [16129, 0, -16256, 0, 8128, 0, 0, 0]);
    assert_eq!(chip.channel_active(0), Some(false));
    assert_eq!(chip.channel_active(4), None);
    write(&mut chip, 0x28, 1);
    assert_eq!(samples(&mut chip, 1), [0; 2]);
    write(&mut chip, 0x28, 0);
    write(&mut chip, 0x28, 1);
    write(&mut chip, 0x2c, 7);
    assert_eq!(samples(&mut chip, 1), [0, 16129]);
}

#[test]
fn high_pitch_advances_multiple_samples_and_pan_updates_live() {
    let mut chip = K053260::new(3_579_545).unwrap();
    chip.write_rom(0, &(0..100).collect::<Vec<u8>>()).unwrap();
    configure_voice(&mut chip, 0, 0, 80, 0x0fff, 64);
    for (reg, value) in [(0x2c, 1), (0x2f, 2), (0x28, 1)] {
        write(&mut chip, reg, value);
    }
    assert_eq!(samples(&mut chip, 3), [127, 0, 8255, 0, 0, 0]);

    write(&mut chip, 0x28, 0);
    chip.write_rom(1, &[64]).unwrap();
    configure_voice(&mut chip, 0, 0, 1, 0, 64);
    write(&mut chip, 0x28, 1);
    for (pan, expected) in [
        (0, [0, 0]),
        (1, [8128, 0]),
        (2, [7424, 3328]),
        (3, [6656, 4672]),
        (4, [5760, 5760]),
        (5, [4672, 6656]),
        (6, [3328, 7424]),
        (7, [0, 8128]),
    ] {
        write(&mut chip, 0x2c, pan);
        assert_eq!(samples(&mut chip, 1), expected);
    }
}

#[test]
fn kadpcm_nibble_order_wrap_loop_and_snapshot() {
    let mut chip = K053260::new(3_579_545).unwrap();
    chip.write_rom(0, &[0x00, 0x12, 0x87]).unwrap();
    configure_voice(&mut chip, 0, 0, 2, 0xfc0, 64);
    for (reg, value) in [(0x2c, 1), (0x2a, 0x10), (0x2f, 2), (0x28, 1)] {
        write(&mut chip, reg, value);
    }
    assert_eq!(samples(&mut chip, 4), [254, 0, 381, 0, 8509, 0, -7747, 0]);
    assert_eq!(samples(&mut chip, 1), [0; 2]);
    chip.write_rom(1, &[0x77, 0x08]).unwrap();
    write(&mut chip, 0x28, 0);
    write(&mut chip, 0x2a, 0x11);
    write(&mut chip, 0x28, 1);
    assert_eq!(samples(&mut chip, 3), [8128, 0, -16256, 0, 0, 0]);
    let snapshot = chip.save_state().unwrap();
    let expected = samples(&mut chip, 12);
    assert!(expected.iter().any(|sample| *sample != 0));
    chip.restore_state(snapshot.as_ref()).unwrap();
    assert_eq!(samples(&mut chip, 12), expected);
}

#[test]
fn reverse_wrap_loop_mute_and_reset_semantics() {
    let mut chip = K053260::new(3_579_545).unwrap();
    chip.write_rom(0x1f_ffff, &[0x20]).unwrap();
    chip.write_rom(0, &[0x30, 0x40]).unwrap();
    configure_voice(&mut chip, 0, 1, 2, 0xfc0, 64);
    for (register, value) in [(0x2c, 1), (0x2a, 1), (0x2f, 2), (0x28, 0x11)] {
        write(&mut chip, register, value);
    }
    assert_eq!(samples(&mut chip, 3), [6096, 0, 4064, 0, 8128, 0]);
    write(&mut chip, 0x2f, 0);
    assert_eq!(samples(&mut chip, 10), [0; 20]);
    write(&mut chip, 0x2f, 2);
    assert_eq!(samples(&mut chip, 1), [6096, 0]);
    chip.reset();
    configure_voice(&mut chip, 0, 1, 2, 0xfc0, 64);
    write(&mut chip, 0x2c, 1);
    write(&mut chip, 0x28, 0x11);
    assert_eq!(chip.channel_active(0), Some(false));
    write(&mut chip, 0x28, 0);
    write(&mut chip, 0x28, 0x11);
    assert_eq!(samples(&mut chip, 1), [6096, 0]);
}

#[test]
fn multivoice_clipping_and_invalid_operations() {
    assert!(matches!(
        K053260::new(63),
        Err(ChipError::InvalidClock { .. })
    ));
    let mut chip = K053260::new(3_579_545).unwrap();
    chip.write_rom(0, &[0, 0x80]).unwrap();
    for i in 0..4 {
        configure_voice(&mut chip, i, 0, 1, 0xfc0, 127);
    }
    for (register, value) in [(0x2c, 9), (0x2d, 9), (0x2f, 2), (0x28, 0x0f)] {
        write(&mut chip, register, value);
    }
    assert_eq!(samples(&mut chip, 1), [-32768, 0]);
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
        Err(ChipError::InvalidStateSnapshot(Chip::K053260))
    ));
}

#[test]
fn vgm_rom_secondary_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::K053260, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::K053260, Instance::Secondary, 3_579_545);
    let mut data = Vec::new();
    data.extend_from_slice(&512_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x7f; 512]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x8e,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [
        (8, 0xc0),
        (9, 15),
        (10, 0xff),
        (11, 1),
        (15, 64),
        (0x2a, 1),
        (0x2c, 7),
        (0x2f, 2),
        (0x28, 1),
    ] {
        builder.add_chip_write(Instance::Secondary, K053260Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().try_into().unwrap();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut player = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<K053260State>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&events);
    player.on_write(move |instance, spec: K053260Spec, _, changes| {
        captured.borrow_mut().push((instance, spec, changes));
    });
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] == 0.0 && frame[1] > 0.1)
    );
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    assert!(
        events
            .borrow()
            .iter()
            .any(|(instance, spec, _)| *instance == Instance::Secondary && spec.register == 0x28)
    );
    assert!(
        events
            .borrow()
            .iter()
            .all(|(_, _, changes)| changes.as_ref().is_none_or(Vec::is_empty))
    );
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    let mut parsed = VgmSequencer::new(parsed, 44_100, 128, Default::default()).unwrap();
    for other in [&mut stream, &mut parsed] {
        assert_eq!(other.render().unwrap().unwrap(), first);
        assert_eq!(other.render().unwrap().unwrap(), second);
    }
}

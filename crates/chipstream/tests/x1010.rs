#![cfg(feature = "x1010")]

use chipstream::chip::{ChipError, SoundChip, X1010};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::state::X1010State;
use soundlog::chip::{Chip, X1010Spec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut X1010, offset: u16, value: u8) {
    chip.write(X1010Spec { offset, value }).unwrap();
}

fn samples(chip: &mut X1010, frames: usize) -> Vec<i32> {
    let mut buffer = vec![0; frames * 2];
    chip.generate(&mut buffer).unwrap();
    buffer
}

#[test]
fn pcm_pitch_volume_key_on_and_end() {
    let mut chip = X1010::new(16_000_000).unwrap();
    assert_eq!(chip.sample_rate(), 16_000_000 / 512);
    chip.write_rom(0, &[64, 0x80, 32, 127]).unwrap();
    for (offset, value) in [(1, 0xf8), (2, 0x10), (5, 0xff), (0, 1)] {
        write(&mut chip, offset, value);
    }
    assert_eq!(samples(&mut chip, 3), [2047, 1092, -4095, -2184, 1023, 546]);
    write(&mut chip, 0, 1);
    assert_eq!(samples(&mut chip, 1), [4063, 2166]);
    write(&mut chip, 0, 0);
    write(&mut chip, 0, 0x81);
    assert_eq!(
        samples(&mut chip, 3),
        [2047, 1092, 2047, 1092, -4095, -2184]
    );
    let snapshot = chip.save_state().unwrap();
    let expected = samples(&mut chip, 20);
    chip.restore_state(snapshot.as_ref()).unwrap();
    assert_eq!(samples(&mut chip, 20), expected);
    write(&mut chip, 0, 0);
    write(&mut chip, 4, 1);
    write(&mut chip, 0, 1);
    assert_eq!(samples(&mut chip, 1), [0, 0]);
    assert_eq!(chip.read_register(0).unwrap() & 1, 0);
    chip.reset();
    assert_eq!(chip.read_register(1).unwrap(), 0);
    for (offset, value) in [(1, 0xf0), (2, 16), (5, 0xff), (0, 1)] {
        write(&mut chip, offset, value);
    }
    assert_eq!(samples(&mut chip, 1), [2047, 0]);
}

#[test]
fn wavetable_envelope_one_shot_and_retrigger() {
    let mut chip = X1010::new(16_000_000).unwrap();
    write(&mut chip, 0x80, 0xf0);
    write(&mut chip, 0x81, 0x0f);
    write(&mut chip, 0x1080, 64);
    write(&mut chip, 0x1081, 0x80);
    write(&mut chip, 0x1085, 0x80);
    for (offset, value) in [(1, 1), (2, 0), (3, 4), (4, 255), (5, 1), (0, 7)] {
        write(&mut chip, offset, value);
    }
    assert_eq!(samples(&mut chip, 2), [2047, 0, -4095, 0]);
    let snapshot = chip.save_state().unwrap();
    let expected = samples(&mut chip, 4);
    assert_eq!(&expected[6..8], [0, -4095]);
    chip.restore_state(snapshot.as_ref()).unwrap();
    assert_eq!(samples(&mut chip, 4), expected);
    samples(&mut chip, 510);
    assert_eq!(chip.read_register(0).unwrap() & 1, 0);
    write(&mut chip, 0, 7);
    assert_eq!(samples(&mut chip, 1), [2047, 0]);
}

#[test]
fn pcm_zero_frequency_and_waveform_envelope_repeat() {
    let mut chip = X1010::new(16_000_000).unwrap();
    chip.write_rom(0, &[64, 32]).unwrap();
    for (offset, value) in [(1, 0xf0), (5, 0xff), (0, 1)] {
        write(&mut chip, offset, value);
    }
    assert_eq!(
        samples(&mut chip, 5),
        [2047, 0, 2047, 0, 2047, 0, 2047, 0, 1023, 0]
    );
    write(&mut chip, 0, 0);
    write(&mut chip, 0x80, 0xf0);
    write(&mut chip, 0xff, 0x0f);
    write(&mut chip, 0x1000, 64);
    for (offset, value) in [(1, 0), (4, 255), (5, 1), (0, 3)] {
        write(&mut chip, offset, value);
    }
    let output = samples(&mut chip, 516);
    assert_eq!(&output[0..2], [2047, 0]);
    assert_eq!(&output[511 * 2..511 * 2 + 2], [0, 2047]);
    assert_eq!(&output[515 * 2..515 * 2 + 2], [2047, 0]);
    assert_eq!(chip.read_register(0).unwrap() & 1, 1);
}

#[test]
fn invalid_operations_leave_state_and_buffer_unchanged() {
    assert!(matches!(
        X1010::new(511),
        Err(ChipError::InvalidClock { .. })
    ));
    let mut chip = X1010::new(16_000_000).unwrap();
    assert!(
        chip.write(X1010Spec {
            offset: 0x2000,
            value: 0
        })
        .is_err()
    );
    write(&mut chip, 1, 32);
    write(&mut chip, 0, 3);
    let mut out = [42; 4];
    assert!(matches!(
        chip.generate(&mut out),
        Err(ChipError::InvalidConfiguration { .. })
    ));
    assert_eq!(out, [42; 4]);
    assert_eq!(chip.read_register(0).unwrap(), 3);
    write(&mut chip, 1, 0);
    write(&mut chip, 5, 64);
    assert!(matches!(
        chip.generate(&mut out),
        Err(ChipError::InvalidConfiguration { .. })
    ));
    let mut odd = [42; 3];
    assert!(matches!(
        chip.generate(&mut odd),
        Err(ChipError::InvalidBufferLength { .. })
    ));
    assert_eq!(odd, [42; 3]);
}

#[test]
fn vgm_secondary_rom_waveform_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::X1010, Instance::Primary, 16_000_000);
    builder.register_chip(Chip::X1010, Instance::Secondary, 16_000_000);
    let mut data = Vec::new();
    data.extend_from_slice(&4096_u32.to_le_bytes());
    data.extend_from_slice(&0_u32.to_le_bytes());
    data.extend_from_slice(&[0x40; 4096]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x91,
        size: data.len() as u32,
        data,
    });
    for (offset, value) in [(1, 0xf0), (2, 0x10), (5, 0xff), (0, 1)] {
        builder.add_chip_write(Instance::Secondary, X1010Spec { offset, value });
    }
    for (offset, value) in [(0x80, 0x0f), (0x1080, 0x40), (9, 1), (13, 1), (8, 3)] {
        builder.add_chip_write(Instance::Secondary, X1010Spec { offset, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut player = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<X1010State>();
    let seen = Rc::new(RefCell::new(false));
    let captured = Rc::clone(&seen);
    player.on_write(move |instance, spec: X1010Spec, _, _| {
        if instance == Instance::Secondary && spec.offset == 0x1080 {
            *captured.borrow_mut() = true;
        }
    });
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] > 0.01 && frame[1] > 0.01)
    );
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    assert!(*seen.borrow());
    for mut other in [
        VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap(),
        VgmSequencer::new(parsed, 44_100, 128, Default::default()).unwrap(),
    ] {
        assert_eq!(other.render().unwrap().unwrap(), first);
        assert_eq!(other.render().unwrap().unwrap(), second);
    }
}

#![cfg(feature = "ga20")]

use chipstream::chip::{ChipError, ChipRegisterWrite, Ga20, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::state::Ga20State;
use soundlog::chip::{Chip, Ga20Spec, PsgSpec};
use soundlog::vgm::command::{DataBlock, Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut Ga20, register: u8, value: u8) {
    chip.write(Ga20Spec { register, value }).unwrap();
}

#[test]
fn sample_terminator_rate_volume_and_status() {
    let mut chip = Ga20::new(4_000_000).unwrap();
    assert_eq!(chip.sample_rate(), 1_000_000);
    chip.write_rom(16, &[0xff, 0x80, 0x40, 0]).unwrap();
    for (register, value) in [(0, 1), (4, 254), (5, 10), (6, 2)] {
        write(&mut chip, register, value);
    }
    assert_eq!(chip.channel_active(0), Some(true));
    let mut pcm = [0; 14];
    chip.generate(&mut pcm).unwrap();
    assert_eq!(
        pcm,
        [
            4064, 4064, 4064, 4064, 0, 0, 0, 0, -2048, -2048, -2048, -2048, 0, 0
        ]
    );
    assert_eq!(chip.channel_active(0), Some(false));
    assert_eq!(chip.channel_active(4), None);
}

#[test]
fn zero_rate_and_live_volume_changes_preserve_sample_position() {
    let mut chip = Ga20::new(4_000_000).unwrap();
    chip.write_rom(0, &[0xff, 0x40, 0]).unwrap();
    for (register, value) in [(2, 0), (3, 0), (4, 0), (5, 10), (6, 2)] {
        write(&mut chip, register, value);
    }
    let mut initial = [0; 510];
    chip.generate(&mut initial).unwrap();
    assert!(initial.iter().all(|sample| *sample == 4064));
    write(&mut chip, 5, 0);
    let mut frame = [1; 2];
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [0; 2]);
    assert_eq!(chip.channel_active(0), Some(true));
    write(&mut chip, 5, 10);
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [-2048; 2]);
}

#[test]
fn start_latching_mixing_reset_snapshot_and_rom_wrap() {
    let mut chip = Ga20::new(4_000_000).unwrap();
    chip.write_rom(0, &[0xff, 0]).unwrap();
    chip.write_rom(0xffff0, &[0x81; 16]).unwrap();
    for (register, value) in [(0, 0xff), (1, 0xff), (4, 0xff), (5, 10), (6, 2)] {
        write(&mut chip, register, value);
    }
    write(&mut chip, 0, 0);
    write(&mut chip, 1, 0);
    for (register, value) in [(12, 0xff), (13, 10), (14, 2)] {
        write(&mut chip, register, value);
    }
    let mut first = [0; 2];
    chip.generate(&mut first).unwrap();
    assert_eq!(first, [4096; 2]);
    let snapshot = chip.save_state().unwrap();
    let mut next = [0; 32];
    chip.generate(&mut next).unwrap();
    assert_eq!(&next[30..], &[4064; 2]);
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut restored = [0; 32];
    chip.generate(&mut restored).unwrap();
    assert_eq!(restored, next);
    chip.reset();
    assert_eq!(chip.channel_active(0), Some(false));
    for (register, value) in [(4, 0xff), (5, 10), (6, 2)] {
        write(&mut chip, register, value);
    }
    chip.generate(&mut first).unwrap();
    assert_eq!(first, [4064; 2]);
    write(&mut chip, 0x26, 0);
    assert_eq!(chip.channel_active(0), Some(false));
}

#[test]
fn invalid_operations_leave_state_unchanged() {
    assert!(matches!(Ga20::new(3), Err(ChipError::InvalidClock { .. })));
    let mut chip = Ga20::new(4_000_000).unwrap();
    let mut buffer = [42; 3];
    assert!(matches!(
        chip.generate(&mut buffer),
        Err(ChipError::InvalidBufferLength { .. })
    ));
    assert_eq!(buffer, [42; 3]);
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
        Err(ChipError::InvalidStateSnapshot(Chip::Ga20))
    ));
}

#[test]
fn vgm_rom_secondary_callbacks_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ga20, Instance::Primary, 176_400);
    builder.register_chip(Chip::Ga20, Instance::Secondary, 176_400);
    let mut data = Vec::new();
    data.extend_from_slice(&1024_u32.to_le_bytes());
    data.extend_from_slice(&16_u32.to_le_bytes());
    data.extend_from_slice(&[0xff; 512]);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0x93,
        size: data.len() as u32,
        data,
    });
    for (register, value) in [(0, 1), (4, 255), (5, 10), (6, 2)] {
        builder.add_chip_write(Instance::Secondary, Ga20Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(400));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut player = VgmSequencer::new(document, 44_100, 64, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<Ga20State>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&events);
    player.on_write(move |instance, spec: Ga20Spec, _, changes| {
        captured.borrow_mut().push((instance, spec, changes));
    });
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0] > 0.1 && frame[1] > 0.1)
    );
    let checkpoint = player.checkpoint().unwrap();
    let next = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), next);
    assert!(
        events
            .borrow()
            .iter()
            .any(|(instance, spec, _)| *instance == Instance::Secondary && spec.register == 6)
    );
    assert!(
        events
            .borrow()
            .iter()
            .all(|(_, _, changes)| changes.as_ref().is_none_or(Vec::is_empty))
    );
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 64, Default::default()).unwrap();
    let mut parsed = VgmSequencer::new(parsed, 44_100, 64, Default::default()).unwrap();
    for other in [&mut stream, &mut parsed] {
        assert_eq!(other.render().unwrap().unwrap(), first);
        assert_eq!(other.render().unwrap().unwrap(), next);
    }
}

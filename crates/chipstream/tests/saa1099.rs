#![cfg(feature = "saa1099")]

use chipstream::chip::{ChipError, ChipRegisterWrite, Saa1099, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Saa1099State;
use soundlog::chip::{Chip, PsgSpec, Saa1099Spec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Instance, WaitSamples};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut Saa1099, register: u8, value: u8) {
    chip.write(Saa1099Spec { register, value }).unwrap();
}

fn tone(chip: &mut Saa1099) {
    for (register, value) in [(0, 0x8f), (8, 0xff), (0x10, 7), (0x14, 1), (0x1c, 1)] {
        write(chip, register, value);
    }
}

#[test]
fn tracked_frequency_matches_generated_tone() {
    let mut chip = Saa1099::new(8_000_000).unwrap();
    let mut state = Saa1099State::new(8_000_000.0);
    use soundlog::chip::state::ChipState;
    let mut events = Vec::new();
    for (register, value) in [(0, 0xff), (8, 0xe3), (0x10, 3), (0x14, 1), (0x1c, 1)] {
        write(&mut chip, register, value);
        events.extend(state.on_register_write(register, value).unwrap_or_default());
    }
    let frequency = events
        .iter()
        .find_map(|event| match event {
            StateEvent::KeyOn { tone, .. } => tone.freq_hz,
            _ => None,
        })
        .unwrap();
    let mut samples = vec![0; chip.sample_rate() as usize * 2];
    chip.generate(&mut samples).unwrap();
    let rising_edges = samples
        .chunks_exact(2)
        .map(|frame| frame[0] > 0)
        .zip(samples.chunks_exact(2).skip(1).map(|frame| frame[0] > 0))
        .filter(|&(before, after)| !before && after)
        .count();
    assert!((rising_edges as f32 - frequency).abs() < 2.0);
}

#[test]
fn stereo_tone_matches_mame_counters_and_amplitude() {
    let mut chip = Saa1099::new(8_000_000).unwrap();
    assert_eq!(chip.sample_rate(), 31_250);
    tone(&mut chip);
    let mut samples = [0; 10];
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [0, 0, 5119, 2730, 0, 0, 0, 0, 5119, 2730]);
    chip.reset();
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [0; 10]);
    tone(&mut chip);
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [0, 0, 5119, 2730, 0, 0, 0, 0, 5119, 2730]);
}

#[test]
fn external_envelope_selection_clocks_and_reverses_right() {
    let mut chip = Saa1099::new(8_000_000).unwrap();
    for (register, value) in [
        (0, 0xff),
        (8, 0xff),
        (0x10, 7),
        (0x14, 1),
        (0x18, 0xaf),
        (0x19, 0),
        (0x1c, 1),
    ] {
        write(&mut chip, register, value);
    }
    let mut frame = [0; 2];
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [319, 4479]);
    write(&mut chip, 0x19, 0);
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [639, 4159]);
}

#[test]
fn three_bit_envelope_and_sync_reset() {
    let mut chip = Saa1099::new(8_000_000).unwrap();
    tone(&mut chip);
    write(&mut chip, 0x18, 0xbf);
    write(&mut chip, 0x19, 0);
    let mut frame = [0; 2];
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [0, 2389]);
    write(&mut chip, 0x19, 0);
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [639, 2047]);
    write(&mut chip, 0x1c, 3);
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [0, 0]);
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [0, 0]);
    chip.generate(&mut frame).unwrap();
    assert_eq!(frame, [639, 2047]);
}

#[test]
fn snapshot_restores_noise_and_invalid_buffer_does_not_advance() {
    let mut chip = Saa1099::new(8_000_000).unwrap();
    for (register, value) in [(0, 0xff), (0x15, 1), (0x16, 0), (0x1c, 1)] {
        write(&mut chip, register, value);
    }
    let mut warmup = [0; 8];
    chip.generate(&mut warmup).unwrap();
    let snapshot = chip.save_state().unwrap();
    let mut expected = [0; 96];
    chip.generate(&mut expected).unwrap();
    assert!(expected.iter().any(|sample| *sample != 0));
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut invalid = [99; 3];
    assert!(matches!(
        chip.generate(&mut invalid),
        Err(ChipError::InvalidBufferLength { .. })
    ));
    assert_eq!(invalid, [99; 3]);
    let mut actual = [0; 96];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);
    assert!(Saa1099::new(255).is_err());
    assert!(matches!(
        chip.apply_register_write(ChipRegisterWrite::Psg(PsgSpec { value: 0 })),
        Err(ChipError::ChipMismatch { .. })
    ));
    assert!(matches!(
        chip.restore_state(&42_u32),
        Err(ChipError::InvalidStateSnapshot(Chip::Saa1099))
    ));
}

#[test]
fn vgm_secondary_chip_and_typed_callbacks_produce_audio() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Saa1099, Instance::Primary, 8_000_000);
    builder.register_chip(Chip::Saa1099, Instance::Secondary, 8_000_000);
    for (register, value) in [(0, 0x8f), (8, 0xff), (0x10, 7), (0x14, 1), (0x1c, 1)] {
        builder.add_chip_write(Instance::Secondary, Saa1099Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(2000));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().into();
    let mut checkpoint_player =
        VgmSequencer::new(document.clone(), 44_100, 128, Default::default()).unwrap();
    checkpoint_player.render().unwrap();
    let checkpoint = checkpoint_player.checkpoint().unwrap();
    let expected = checkpoint_player.render().unwrap();
    checkpoint_player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(checkpoint_player.render().unwrap(), expected);

    let events = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&events);
    let mut player = VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<Saa1099State>();
    player.on_write(move |instance, spec: Saa1099Spec, _, changes| {
        received.borrow_mut().push((instance, spec, changes));
    });
    let mut frames = Vec::new();
    while let Some(block) = player.render().unwrap() {
        frames.extend_from_slice(block.frames());
    }
    assert!(
        frames
            .iter()
            .any(|frame| frame[0] > frame[1] && frame[1] > 0.0)
    );
    assert_eq!(events.borrow().len(), 5);
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

    let mut parsed = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    let mut parsed_frames = Vec::new();
    while let Some(block) = parsed.render().unwrap() {
        parsed_frames.extend_from_slice(block.frames());
    }
    assert_eq!(frames, parsed_frames);
}

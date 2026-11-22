#![cfg(feature = "pokey")]

use chipstream::chip::{ChipError, ChipRegisterWrite, Pokey, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::PokeyState;
use soundlog::chip::{Chip, PokeySpec, PsgSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Instance, WaitSamples};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut Pokey, register: u8, value: u8) {
    chip.write(PokeySpec { register, value }).unwrap();
}

fn samples(chip: &mut Pokey, count: usize) -> Vec<i32> {
    let mut result = vec![0; count];
    chip.generate(&mut result).unwrap();
    result
}

#[test]
fn timers_match_pokey_clock_dividers_and_joined_periods() {
    for (audctl, low, high, period) in [
        (0x00, 5, None, 28 * 6),
        (0x01, 5, None, 114 * 6),
        (0x40, 5, None, 5 + 4),
        (0x10, 5, Some(1), 28 * (256 + 5 + 1)),
        (0x50, 5, Some(1), 256 + 5 + 7),
    ] {
        let mut chip = Pokey::new(1_789_790).unwrap();
        write(&mut chip, 0, low);
        write(&mut chip, 1, if high.is_none() { 0xaf } else { 0xa0 });
        if let Some(high) = high {
            write(&mut chip, 2, high);
            write(&mut chip, 3, 0xaf);
        }
        write(&mut chip, 8, audctl);
        write(&mut chip, 9, 0);
        write(&mut chip, 15, 3);
        let pcm = samples(&mut chip, period * 5 + 200);
        let transitions: Vec<_> = pcm
            .windows(2)
            .enumerate()
            .filter_map(|(i, pair)| (pair[0] != pair[1]).then_some(i))
            .collect();
        assert!(transitions.len() >= 4, "{audctl:02x}: {transitions:?}");
        for pair in transitions[1..].windows(2) {
            assert_eq!(pair[1] - pair[0], period, "AUDCTL={audctl:02x}");
        }
    }
}

#[test]
fn volume_reset_snapshot_and_invalid_writes() {
    assert!(Pokey::new(0).is_err());
    let mut chip = Pokey::new(1_789_790).unwrap();
    assert_eq!(chip.channels(), 1);
    assert_eq!(chip.sample_rate(), 1_789_790);
    for register in [1, 3, 5, 7] {
        write(&mut chip, register, 0x1f);
    }
    assert_eq!(samples(&mut chip, 4), [32767; 4]);
    write(&mut chip, 1, 0xaf);
    write(&mut chip, 3, 0);
    write(&mut chip, 5, 0);
    write(&mut chip, 7, 0);
    write(&mut chip, 0, 3);
    write(&mut chip, 8, 0x40);
    write(&mut chip, 9, 0);
    write(&mut chip, 15, 3);
    samples(&mut chip, 30);
    let saved = chip.save_state().unwrap();
    let expected = samples(&mut chip, 100);
    chip.restore_state(saved.as_ref()).unwrap();
    chip.reset();
    assert_eq!(samples(&mut chip, 100), expected);
    write(&mut chip, 15, 0);
    samples(&mut chip, 8);
    assert!(
        samples(&mut chip, 100)
            .windows(2)
            .all(|pair| pair[0] == pair[1])
    );
    assert!(matches!(
        chip.apply_register_write(ChipRegisterWrite::Psg(PsgSpec { value: 0 })),
        Err(ChipError::ChipMismatch { .. })
    ));
    assert!(matches!(
        chip.restore_state(&42_u32),
        Err(ChipError::InvalidStateSnapshot(Chip::Pokey))
    ));
}

#[test]
fn noise_filters_and_two_tone_change_the_output() {
    fn render(control: u8, audctl: u8, skctl: u8) -> Vec<i32> {
        let mut chip = Pokey::new(1_789_790).unwrap();
        for (reg, value) in [
            (0, 4),
            (1, control),
            (2, 4),
            (3, 0xa0),
            (4, 8),
            (5, 0xa0),
            (8, audctl),
            (9, 0),
            (15, skctl),
        ] {
            write(&mut chip, reg, value);
        }
        samples(&mut chip, 3000)
    }
    let poly4 = render(0xcf, 0x40, 3);
    let poly5 = render(0x2f, 0x40, 3);
    let poly9 = render(0x8f, 0xc0, 3);
    let poly17 = render(0x8f, 0x40, 3);
    for noise in [&poly4, &poly5, &poly9, &poly17] {
        assert!(noise.contains(&0) && noise.contains(&11160));
    }
    assert_ne!(poly4, poly9);
    assert_ne!(poly9, poly17);
    assert_ne!(render(0xaf, 0x40, 3), render(0xaf, 0x44, 3));
    assert_ne!(render(0xaf, 0x40, 3), render(0xaf, 0x40, 0x8b));
}

#[test]
fn vgm_secondary_events_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Pokey, Instance::Primary, 1_789_790);
    builder.register_chip(Chip::Pokey, Instance::Secondary, 1_789_790);
    for (register, value) in [(0, 20), (1, 0xaf), (8, 0x40), (9, 0), (15, 3)] {
        builder.add_chip_write(Instance::Secondary, PokeySpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().into();
    let mut player = VgmSequencer::new(doc, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<PokeyState>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&events);
    player.on_write(move |instance, spec: PokeySpec, _, changes| {
        received.borrow_mut().push((instance, spec, changes));
    });
    let first = player.render().unwrap().unwrap();
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0].abs() > 0.001 && frame[1].abs() > 0.001)
    );
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
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
    assert_eq!(parsed.render().unwrap().unwrap(), first);
    assert_eq!(parsed.render().unwrap().unwrap(), second);
}

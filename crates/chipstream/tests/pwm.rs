#![cfg(feature = "pwm")]

use chipstream::chip::{ChipError, ChipRegisterWrite, Pwm, SoundChip};
use chipstream::sequencer::{VgmEventFilter, VgmSequencer, VgmSequencerError};
use soundlog::chip::{Chip, PwmSpec};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::vgm::{VgmBuilder, VgmDocument};

#[test]
fn fifo_routing_full_values_and_reset() {
    let mut chip = Pwm::new(44_100).unwrap();
    chip.write(PwmSpec {
        register: 1,
        value: 3,
    })
    .unwrap();
    chip.write(PwmSpec {
        register: 0,
        value: 5,
    })
    .unwrap();
    for value in [0x10, 0x100, 0x200, 0x300] {
        chip.write(PwmSpec { register: 2, value }).unwrap();
    }
    chip.write(PwmSpec {
        register: 3,
        value: 0xabc,
    })
    .unwrap();
    let mut output = [0; 8];
    chip.generate(&mut output).unwrap();
    assert_eq!(
        output,
        [0x100 * 32, 0xabc * 32, 0x200 * 32, 0, 0x300 * 32, 0, 0, 0]
    );
    chip.reset();
    output.fill(1);
    chip.generate(&mut output).unwrap();
    assert_eq!(output, [0; 8]);
}

#[test]
fn invalid_operations_do_not_change_state() {
    assert!(matches!(Pwm::new(0), Err(ChipError::InvalidClock { .. })));
    let mut chip = Pwm::new(44_100).unwrap();
    assert!(
        chip.write(PwmSpec {
            register: 0,
            value: 3
        })
        .is_err()
    );
    assert!(
        chip.write(PwmSpec {
            register: 4,
            value: 0x1000
        })
        .is_err()
    );
    assert!(matches!(
        chip.apply_register_write(ChipRegisterWrite::Okim6258(soundlog::chip::Okim6258Spec {
            register: 0,
            value: 0
        })),
        Err(ChipError::ChipMismatch { .. })
    ));
    assert!(
        chip.write(PwmSpec {
            register: 5,
            value: 0
        })
        .is_err()
    );
    let mut invalid = [99; 3];
    assert!(chip.generate(&mut invalid).is_err());
    assert_eq!(invalid, [99; 3]);
}

#[test]
fn vgm_playback_and_events_preserve_12_bit_samples() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Pwm, Instance::Primary, 44_100);
    for (register, value) in [(1, 3), (0, 5), (4, 0xabc)] {
        builder.add_chip_write(Instance::Primary, PwmSpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(200));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 30).unwrap();
    let mut document = VgmSequencer::new(parsed, 44_100, 128).unwrap();
    for player in [&mut stream, &mut document] {
        player
            .configure_events(VgmEventFilter {
                chip: Some(Chip::Pwm),
                register_writes: true,
                ..Default::default()
            })
            .unwrap();
    }
    let render = |player: &mut VgmSequencer| {
        let mut audio = Vec::new();
        let mut values = Vec::new();
        while let Some(block) = player
            .render_with_events(|event| values.push(event.value))
            .unwrap()
        {
            audio.extend_from_slice(block.frames());
        }
        (audio, values)
    };
    let (a, events) = render(&mut stream);
    let (b, other_events) = render(&mut document);
    assert_eq!(a, b);
    assert_eq!(events, vec![3, 5, 0xabc]);
    assert_eq!(events, other_events);
    assert!(a.iter().any(|frame| frame[0] != 0.0 && frame[1] != 0.0));
}

fn pwm_rip(control: Option<u32>, non_control_writes: usize) -> VgmDocument {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Pwm, Instance::Primary, 44_100);
    if let Some(value) = control {
        builder.add_chip_write(Instance::Primary, PwmSpec { register: 0, value });
    }
    builder.add_chip_write(
        Instance::Primary,
        PwmSpec {
            register: 1,
            value: 3,
        },
    );
    for _ in 1..non_control_writes {
        builder.add_chip_write(
            Instance::Primary,
            PwmSpec {
                register: 4,
                value: 0x100,
            },
        );
        builder.add_vgm_command(WaitSamples(2));
    }
    builder.finalize()
}

type CapturedEvents = Vec<(u8, u32, usize)>;

fn render_events(player: &mut VgmSequencer) -> (Vec<[f32; 2]>, CapturedEvents) {
    player
        .configure_events(VgmEventFilter {
            chip: Some(Chip::Pwm),
            register_writes: true,
            ..Default::default()
        })
        .unwrap();
    let mut audio = Vec::new();
    let mut events = Vec::new();
    while let Some(block) = player
        .render_with_events(|event| {
            events.push((
                event.register,
                event.value,
                audio.len() + event.frame_offset as usize,
            ))
        })
        .unwrap()
    {
        audio.extend_from_slice(block.frames());
    }
    (audio, events)
}

#[test]
fn vgm_bad_rip_workaround_triggers_on_32nd_non_control_write() {
    let document = pwm_rip(Some(0), 33);
    let bytes: Vec<u8> = document.clone().into();
    let mut streamed = VgmSequencer::from_vgm(bytes, 44_100, 11).unwrap();
    let mut parsed = VgmSequencer::new(document.clone(), 44_100, 128).unwrap();
    let (audio, events) = render_events(&mut streamed);
    let (other_audio, other_events) = render_events(&mut parsed);
    assert_eq!(audio, other_audio);
    assert_eq!(events, other_events);
    let control = events
        .iter()
        .position(|event| *event == (0, 5, 60))
        .unwrap();
    assert_eq!(events[control + 1], (4, 0x100, 60));
    assert_eq!(
        events
            .iter()
            .filter(|event| event.0 == 0 && event.1 == 5)
            .count(),
        1
    );
    assert!(
        audio
            .iter()
            .any(|frame| frame[0] > 0.001 && frame[1] > 0.001)
    );

    let mut disabled = VgmSequencer::new(document, 44_100, 128).unwrap();
    disabled.set_pwm_compatibility(false).unwrap();
    let (silent, events) = render_events(&mut disabled);
    assert!(silent.iter().flatten().all(|sample| *sample == 0.0));
    assert!(events.iter().all(|event| event.0 != 0 || event.1 != 5));
    assert!(matches!(
        disabled.set_pwm_compatibility(true),
        Err(VgmSequencerError::PwmCompatibilityConfigurationAfterPlayback)
    ));
}

#[test]
fn pwm_workaround_respects_threshold_and_original_control_write() {
    for (control, count, expect_workaround) in [
        (None, 31, false),
        (None, 32, true),
        (Some(0), 32, true),
        (Some(0x100), 32, true),
        (Some(5), 33, false),
    ] {
        let mut player = VgmSequencer::new(pwm_rip(control, count), 44_100, 16).unwrap();
        let (_, events) = render_events(&mut player);
        let control_writes = events
            .iter()
            .filter(|event| event.0 == 0 && event.1 == 5)
            .count();
        assert_eq!(
            control_writes,
            usize::from(expect_workaround || control == Some(5))
        );
        if expect_workaround {
            let index = events
                .iter()
                .position(|event| event.0 == 0 && event.1 == 5)
                .unwrap();
            assert_eq!(
                events[..index].iter().filter(|event| event.0 != 0).count(),
                31
            );
        }
    }
}

#[test]
fn pwm_compatibility_is_independent_for_each_chip_instance() {
    let mut document = pwm_rip(None, 32);
    document.header.pwm_clock |= 0x4000_0000;
    let end = document.commands.len() - 1;
    let secondary = document.commands[..end]
        .iter()
        .filter_map(|command| match command {
            soundlog::vgm::command::VgmCommand::PwmWrite(_, spec) => Some(
                soundlog::vgm::command::VgmCommand::PwmWrite(Instance::Secondary, spec.clone()),
            ),

            _ => None,
        })
        .collect::<Vec<_>>();
    document.commands.splice(end..end, secondary);
    let mut player = VgmSequencer::new(document, 44_100, 32).unwrap();
    player
        .configure_events(VgmEventFilter {
            chip: Some(Chip::Pwm),
            register: Some(0),
            register_writes: true,
            ..Default::default()
        })
        .unwrap();
    let mut instances = Vec::new();
    while player
        .render_with_events(|event| instances.push((event.instance, event.value)))
        .unwrap()
        .is_some()
    {}
    assert_eq!(
        instances,
        vec![(Instance::Primary, 5), (Instance::Secondary, 5)]
    );
}

#[test]
fn pwm_workaround_does_not_repeat_on_vgm_loop() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Pwm, Instance::Primary, 44_100);
    builder.set_loop_index(0);
    builder.add_chip_write(
        Instance::Primary,
        PwmSpec {
            register: 1,
            value: 3,
        },
    );
    for _ in 0..31 {
        builder.add_chip_write(
            Instance::Primary,
            PwmSpec {
                register: 4,
                value: 0x100,
            },
        );
        builder.add_vgm_command(WaitSamples(2));
    }
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 17).unwrap();
    player.set_loop_count(Some(2));
    let (_, events) = render_events(&mut player);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.0 == 0 && event.1 == 5)
            .count(),
        1
    );
}

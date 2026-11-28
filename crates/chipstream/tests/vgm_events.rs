use std::cell::RefCell;
use std::rc::Rc;

use chipstream::sequencer::{
    Chip, ExecutionEventError, ExecutionEventSender, Instance, PlaybackOptions, StateEvent,
    VgmSequencer, VgmSequencerError, VgmSequencerOptions, execution_event_channel,
};
use soundlog::chip::state::{ChipState, Ym2151State};
use soundlog::chip::{Ay8910Spec, Ym2151Spec, Ym2612Spec};
use soundlog::vgm::command::{DataBlock, WaitSamples, Ym2612Port0Address2AWriteAndWaitN};
use soundlog::vgm::{VgmBuilder, VgmDocument};
use soundlog::{ExecutionEvent, ParseError, VgmCommandGenerator};

type Ym2151Event = (usize, Instance, Ym2151Spec, Option<Vec<StateEvent>>);

#[derive(Debug)]
struct SourcePosition(&'static str);

#[derive(Debug)]
struct ObservedGenerator {
    sender: ExecutionEventSender<SourcePosition>,
    events: Vec<(u64, &'static str)>,
    emitted: bool,
}

impl VgmCommandGenerator for ObservedGenerator {
    fn next_command(&mut self) -> Result<Option<soundlog::vgm::command::VgmCommand>, ParseError> {
        if self.emitted {
            return Ok(None);
        }
        self.emitted = true;
        for &(sample, name) in &self.events {
            if self
                .sender
                .send(ExecutionEvent {
                    sample,
                    data: SourcePosition(name),
                })
                .is_err()
            {
                break;
            }
        }
        Ok(Some(WaitSamples(1000).into()))
    }
}

fn source_events() -> Vec<(u64, &'static str)> {
    vec![
        (0, "a"),
        (100, "b"),
        (100, "c"),
        (300, "d"),
        (1000, "end"),
        (1001, "future"),
    ]
}

#[test]
fn custom_execution_events_follow_output_and_survive_checkpoint_seek() {
    for (rate, chunk) in [(44_100, 31), (48_000, 64), (22_050, 17)] {
        let (sender, receiver) = execution_event_channel();
        let factory = move || -> Result<Box<dyn VgmCommandGenerator>, ParseError> {
            Ok(Box::new(ObservedGenerator {
                sender: sender.clone(),
                events: source_events(),
                emitted: false,
            }))
        };
        let mut player =
            VgmSequencer::from_generator_factory(factory, Default::default(), rate, chunk).unwrap();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&seen);
        player
            .on_execution(receiver, move |frame, event| {
                captured
                    .borrow_mut()
                    .push((frame, event.sample, event.data.0));
            })
            .unwrap();
        let initial = player.checkpoint().unwrap();
        player.render().unwrap().unwrap();
        let checkpoint = player.checkpoint().unwrap();
        let position = player.position_frames();
        seen.borrow_mut().clear();
        while player.render().unwrap().is_some() {}
        let expected = seen.borrow().clone();
        assert_eq!(
            expected.iter().map(|event| event.2).collect::<Vec<_>>(),
            ["b", "c", "d", "end"]
        );
        for (frame, sample, _) in &expected {
            assert_eq!(
                *frame,
                ((*sample * u64::from(rate) + 22_050) / 44_100) as usize
            );
        }
        seen.borrow_mut().clear();
        player.restore_checkpoint(&checkpoint).unwrap();
        assert_eq!(player.position_frames(), position);
        assert!(seen.borrow().is_empty());
        while player.render().unwrap().is_some() {}
        assert_eq!(*seen.borrow(), expected);

        seen.borrow_mut().clear();
        let target = ((100_u64 * u64::from(rate) + 22_050) / 44_100) as usize;
        player.seek_to_frame(&initial, target).unwrap();
        assert!(seen.borrow().is_empty());
        while player.render().unwrap().is_some() {}
        assert_eq!(
            seen.borrow()
                .iter()
                .map(|event| event.2)
                .collect::<Vec<_>>(),
            ["b", "c", "d", "end"]
        );
    }
}

#[test]
fn execution_queue_errors_are_explicit_and_terminal() {
    for too_many in [false, true] {
        let (sender, receiver) = execution_event_channel();
        let events = if too_many {
            vec![(0, "event"); 4097]
        } else {
            vec![(10, "first"), (9, "out-of-order")]
        };
        let mut player = VgmSequencer::from_generator(
            Box::new(ObservedGenerator {
                sender,
                events,
                emitted: false,
            }),
            Default::default(),
            44_100,
            64,
        )
        .unwrap();
        player
            .on_execution(receiver, |_, _| {
                panic!("events must not be delivered after overflow")
            })
            .unwrap();
        let error = player.render().unwrap_err();
        if too_many {
            assert!(matches!(
                error,
                VgmSequencerError::ExecutionEvent(ExecutionEventError::QueueFull { limit: 4096 })
            ));
        } else {
            assert!(matches!(
                error,
                VgmSequencerError::ExecutionEvent(ExecutionEventError::OutOfOrder {
                    previous: 10,
                    sample: 9
                })
            ));
        }
        assert!(matches!(
            player.render(),
            Err(VgmSequencerError::StreamFailed)
        ));
    }
}

#[test]
fn late_execution_events_fail_after_the_source_passes_them() {
    #[derive(Debug)]
    struct LateGenerator {
        sender: ExecutionEventSender<SourcePosition>,
        step: usize,
    }
    impl VgmCommandGenerator for LateGenerator {
        fn next_command(
            &mut self,
        ) -> Result<Option<soundlog::vgm::command::VgmCommand>, ParseError> {
            self.step += 1;
            match self.step {
                1 => Ok(Some(WaitSamples(100).into())),
                2 => {
                    assert!(
                        self.sender
                            .send(ExecutionEvent {
                                sample: 50,
                                data: SourcePosition("late")
                            })
                            .is_err()
                    );
                    Ok(Some(WaitSamples(100).into()))
                }
                _ => Ok(None),
            }
        }
    }
    let (sender, receiver) = execution_event_channel();
    let mut player = VgmSequencer::from_generator(
        Box::new(LateGenerator { sender, step: 0 }),
        Default::default(),
        44_100,
        64,
    )
    .unwrap();
    player
        .on_execution(receiver, |_, _| panic!("late event must not be delivered"))
        .unwrap();
    let error = loop {
        match player.render() {
            Ok(Some(_)) => {}
            Err(error) => break error,
            Ok(None) => panic!("late event was not rejected"),
        }
    };
    assert!(matches!(
        error,
        VgmSequencerError::ExecutionEvent(ExecutionEventError::Late {
            sample: 50,
            current: 100
        })
    ));
}

#[test]
fn execution_events_at_empty_end_are_delivered_once() {
    #[derive(Debug)]
    struct EmptyGenerator(ExecutionEventSender<SourcePosition>);
    impl VgmCommandGenerator for EmptyGenerator {
        fn next_command(
            &mut self,
        ) -> Result<Option<soundlog::vgm::command::VgmCommand>, ParseError> {
            self.0
                .send(ExecutionEvent {
                    sample: 0,
                    data: SourcePosition("end"),
                })
                .map_err(|error| ParseError::GeneratorError(Box::new(error)))?;
            Ok(None)
        }
    }
    let (sender, receiver) = execution_event_channel();
    let mut player = VgmSequencer::from_generator(
        Box::new(EmptyGenerator(sender)),
        Default::default(),
        44_100,
        64,
    )
    .unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&seen);
    player
        .on_execution(receiver, move |frame, event| {
            captured.borrow_mut().push((frame, event.data.0))
        })
        .unwrap();
    assert!(player.render().unwrap().is_none());
    assert!(player.render().unwrap().is_none());
    assert_eq!(*seen.borrow(), [(0, "end")]);
}

#[test]
fn execution_channel_requires_registration_and_matching_checkpoint() {
    let (sender, receiver) = execution_event_channel();
    assert_eq!(
        sender.send(ExecutionEvent {
            sample: 0,
            data: SourcePosition("before")
        }),
        Err(ExecutionEventError::ReceiverNotAttached)
    );
    let generator_sender = sender.clone();
    let factory = move || -> Result<Box<dyn VgmCommandGenerator>, ParseError> {
        Ok(Box::new(ObservedGenerator {
            sender: generator_sender.clone(),
            events: source_events(),
            emitted: false,
        }))
    };
    let mut player =
        VgmSequencer::from_generator_factory(factory, Default::default(), 44_100, 64).unwrap();
    let checkpoint = player.checkpoint().unwrap();
    player.on_execution(receiver, |_, _| {}).unwrap();
    assert!(matches!(
        player.restore_checkpoint(&checkpoint),
        Err(VgmSequencerError::CheckpointMismatch)
    ));
    drop(player);
    assert_eq!(
        sender.send(ExecutionEvent {
            sample: 0,
            data: SourcePosition("after")
        }),
        Err(ExecutionEventError::ReceiverNotAttached)
    );
}

#[test]
fn execution_events_cannot_arrive_after_their_output_frame() {
    let (sender, receiver) = execution_event_channel();
    let mut player = VgmSequencer::from_generator(
        Box::new(ObservedGenerator {
            sender: sender.clone(),
            events: Vec::new(),
            emitted: false,
        }),
        Default::default(),
        44_100,
        64,
    )
    .unwrap();
    player.on_execution(receiver, |_, _| {}).unwrap();
    player.render().unwrap().unwrap();
    sender
        .send(ExecutionEvent {
            sample: 0,
            data: SourcePosition("late"),
        })
        .unwrap();
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::ExecutionEvent(
            ExecutionEventError::LateOutput {
                frame: 0,
                current: 64
            }
        ))
    ));
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::StreamFailed)
    ));
}

#[test]
fn tracking_reports_missing_instances_and_counts_matches() {
    let mut player = VgmSequencer::new(document(), 44_100, 64, Default::default()).unwrap();
    assert!(matches!(
        player.track_state::<soundlog::chip::state::Ym2612State>(Instance::Primary, 4_000_000.0),
        Err(VgmSequencerError::ChipNotFound {
            chip: Chip::Ym2612,
            instance: Instance::Primary
        })
    ));
    assert_eq!(
        player.track_state_with_voice_clocks::<soundlog::chip::state::Ym2612State>(),
        0
    );
    assert_eq!(player.track_state_with_voice_clocks::<Ym2151State>(), 2);
    player
        .track_state::<Ym2151State>(Instance::Secondary, 4_000_000.0)
        .unwrap();
    assert!(player.render().unwrap().is_some());
}

fn document() -> VgmDocument {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.register_chip(Chip::Ym2151, Instance::Secondary, 4_000_000);
    for (instance, register, value) in [
        (Instance::Primary, 0x28, 0x4c),
        (Instance::Primary, 0x30, 0),
        (Instance::Secondary, 0x28, 0x50),
        (Instance::Primary, 0x08, 0x78),
    ] {
        builder.add_chip_write(instance, Ym2151Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(100));
    builder.set_loop_index(5);
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x28,
            value: 0x50,
        },
    );
    builder.add_vgm_command(WaitSamples(100));
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x08,
            value: 0,
        },
    );
    builder.finalize()
}

fn collect(player: &mut VgmSequencer<'_>) -> (Vec<[f32; 2]>, Vec<Ym2151Event>) {
    let captured = Rc::new(RefCell::new(Vec::new()));
    let callback_events = Rc::clone(&captured);
    player.on_write(move |instance, spec: Ym2151Spec, sample, state_events| {
        callback_events
            .borrow_mut()
            .push((sample, instance, spec, state_events));
    });

    let mut pcm = Vec::new();
    while let Some(block) = player.render().unwrap() {
        pcm.extend_from_slice(block.frames());
    }
    (pcm, captured.borrow().clone())
}

#[test]
fn identity_write_filter_preserves_audio_and_observer_timing() {
    let options = VgmSequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(2),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut reference = VgmSequencer::new(document(), 48_000, 31, options).unwrap();
    let mut filtered = VgmSequencer::new(document(), 48_000, 31, options).unwrap();
    for player in [&mut reference, &mut filtered] {
        player.track_state_with_voice_clocks::<Ym2151State>();
    }
    filtered.filter_write(|_, spec: Ym2151Spec, _| spec);
    assert_eq!(collect(&mut filtered), collect(&mut reference));
}

#[test]
fn filters_preserve_original_observer_and_tracker_events() {
    let mut reference = VgmSequencer::new(document(), 44_100, 31, Default::default()).unwrap();
    let mut filtered = VgmSequencer::new(document(), 44_100, 31, Default::default()).unwrap();
    for player in [&mut reference, &mut filtered] {
        player.track_state_with_voice_clocks::<Ym2151State>();
    }
    filtered.filter_write(|_, mut spec: Ym2151Spec, _| {
        if spec.register == 0x08 {
            spec.value &= 0x07;
        }
        spec
    });
    let (_, original_events) = collect(&mut reference);
    let (_, filtered_events) = collect(&mut filtered);
    assert_eq!(filtered_events, original_events);
    assert!(
        filtered_events
            .iter()
            .any(|(_, _, _, events)| matches!(events.as_deref(), Some([StateEvent::KeyOn { .. }])))
    );
}

#[test]
fn ay8910_filters_affect_audio_while_callbacks_observe_original_writes() {
    use chipstream::chip::YmfmChip;
    use chipstream::sequencer::ChipFactoryRegistry;

    fn player(volume: u8) -> VgmSequencer<'static> {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ay8910, Instance::Primary, 2_000_000);
        for (register, value) in [(0, 100), (1, 0), (7, 0x3e), (8, volume)] {
            builder.add_chip_write(Instance::Primary, Ay8910Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(1000));
        let mut factories = ChipFactoryRegistry::new();
        factories.register(Chip::Ay8910, |config| {
            Ok(Box::new(YmfmChip::new_ym2149(config.clock(), false)?))
        });
        VgmSequencer::new_with_factories(
            builder.finalize(),
            44_100,
            128,
            Default::default(),
            factories,
        )
        .unwrap()
    }

    let mut filtered = player(15);
    let mut reference = player(0);
    let observed = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&observed);
    filtered.on_write(move |instance, spec: Ay8910Spec, frame, _| {
        captured
            .borrow_mut()
            .push((instance, spec.register, spec.value, frame));
    });
    filtered.filter_write(|_, mut spec: Ay8910Spec, _| {
        if spec.register == 8 {
            spec.value = 0;
        }
        spec
    });
    while let Some(block) = filtered.render().unwrap() {
        assert_eq!(Some(block), reference.render().unwrap());
    }
    assert!(reference.render().unwrap().is_none());
    assert!(observed.borrow().contains(&(Instance::Primary, 8, 15, 0)));
}

#[test]
fn callback_timing_is_chunk_invariant_across_loops_and_fadeout() {
    let options = VgmSequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(2),
            fadeout: Some(std::time::Duration::from_secs_f64(150.0 / 44_100.0)),
        },
        ..Default::default()
    };
    let mut small = VgmSequencer::new(document(), 44_100, 31, options).unwrap();
    let mut large = VgmSequencer::new(document(), 44_100, 512, options).unwrap();
    for player in [&mut small, &mut large] {
        player
            .track_state::<Ym2151State>(Instance::Primary, 4_000_000.0)
            .unwrap();
        player
            .track_state::<Ym2151State>(Instance::Secondary, 4_000_000.0)
            .unwrap();
    }
    let (a, events_a) = collect(&mut small);
    let (b, events_b) = collect(&mut large);
    assert_eq!(a, b);
    assert_eq!(a.len(), 450);
    assert_eq!(a.last(), Some(&[0.0, 0.0]));
    assert_eq!(events_a, events_b);
}

#[test]
fn event_timing_matches_pcm_across_chunks_loop_and_final_boundary() {
    let doc = document();
    let options = VgmSequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(2),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut small = VgmSequencer::new(doc.clone(), 44_100, 31, options).unwrap();
    let mut large = VgmSequencer::new(doc.clone(), 44_100, 256, options).unwrap();
    let mut plain = VgmSequencer::new(doc, 44_100, 71, options).unwrap();
    for player in [&mut small, &mut large] {
        player
            .track_state::<Ym2151State>(Instance::Primary, 4_000_000.0)
            .unwrap();
        player
            .track_state::<Ym2151State>(Instance::Secondary, 4_000_000.0)
            .unwrap();
    }
    small.on_write(|_, _: Ym2151Spec, _, _| {});
    large.on_write(|_, _: Ym2151Spec, _, _| {});
    let (a, events_a) = collect(&mut small);
    let (b, events_b) = collect(&mut large);
    let (c, _) = collect(&mut plain);
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(a.len(), 300);
    assert_eq!(events_a, events_b);
    assert_eq!(
        events_a
            .iter()
            .map(|(sample, ..)| *sample)
            .collect::<Vec<_>>(),
        vec![0, 0, 1, 1, 100, 200, 201, 300]
    );
    assert!(matches!(
        events_a[3].3.as_deref(),
        Some([StateEvent::KeyOn { channel: 0, .. }])
    ));
    assert!(matches!(
        events_a[4].3.as_deref(),
        Some([StateEvent::ToneChange { channel: 0, .. }])
    ));
    assert!(matches!(
        events_a[5].3.as_deref(),
        Some([StateEvent::KeyOff { channel: 0 }])
    ));
    assert_eq!(events_a.last().unwrap().3, Some(Vec::new()));
}

#[test]
fn event_positions_follow_output_rate_rounding() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    for (wait, value) in [(0, 0x4c), (1, 0x50), (100, 0x54)] {
        builder.add_vgm_command(WaitSamples(wait));
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x28,
                value,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(100));
    let mut player = VgmSequencer::new(builder.finalize(), 48_000, 17, Default::default()).unwrap();
    let (_, events) = collect(&mut player);
    assert_eq!(
        events
            .iter()
            .map(|(sample, ..)| *sample)
            .collect::<Vec<_>>(),
        vec![0, 2, 111]
    );
}

#[test]
fn typed_callbacks_filter_multiport_registers() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
    for port in [0, 1] {
        builder.add_chip_write(
            Instance::Primary,
            Ym2612Spec {
                port,
                register: 0xa0,
                value: 7,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(10));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 32, Default::default()).unwrap();
    let captured = Rc::new(RefCell::new(Vec::new()));
    let callback_events = Rc::clone(&captured);
    player.on_write(move |instance, spec: Ym2612Spec, sample, state_events| {
        if spec.port == 1 {
            callback_events
                .borrow_mut()
                .push((sample, instance, spec, state_events));
        }
    });
    while player.render().unwrap().is_some() {}
    let events = captured.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].0, events[0].2.port), (1, 1));
    assert_eq!(events[0].3, None);
}

#[test]
fn state_tracking_uses_requested_clock_and_is_instance_specific() {
    use soundlog::vgm::header::{ChipClock, ChipId, VgmExtraHeader};

    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.set_extra_header(VgmExtraHeader {
        chip_clocks: vec![ChipClock::new(ChipId::Ym2151, Instance::Primary, 2_000_000)],
        ..Default::default()
    });
    for (register, value) in [(0x28, 0x4c), (0x30, 0), (0x08, 0x78)] {
        builder.add_chip_write(Instance::Primary, Ym2151Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(10));
    let bytes: Vec<u8> = builder.finalize().try_into().unwrap();
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 8, Default::default()).unwrap();
    let mut parsed = VgmSequencer::new(document, 44_100, 8, Default::default()).unwrap();
    for player in [&mut stream, &mut parsed] {
        player
            .track_state::<Ym2151State>(Instance::Primary, 2_000_000.0)
            .unwrap();
    }
    let (a, events) = collect(&mut stream);
    assert_eq!((a, events.clone()), collect(&mut parsed));
    let mut expected = Ym2151State::new(2_000_000.0);
    expected.on_register_write(0x28, 0x4c);
    expected.on_register_write(0x30, 0);
    let key_on = events
        .iter()
        .find(|(_, _, spec, _)| spec.register == 0x08)
        .unwrap();
    assert_eq!(
        key_on.3,
        Some(expected.on_register_write(0x08, 0x78).unwrap())
    );
}

#[test]
fn state_only_callbacks_use_effective_voice_clock_and_skip_unrelated_writes() {
    use soundlog::vgm::header::{ChipClock, ChipId, VgmExtraHeader};

    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.set_extra_header(VgmExtraHeader {
        chip_clocks: vec![ChipClock::new(ChipId::Ym2151, Instance::Primary, 2_000_000)],
        ..Default::default()
    });
    for _ in 0..5000 {
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x20,
                value: 0xc7,
            },
        );
    }
    for (register, value) in [(0x28, 0x4c), (0x30, 0), (0x08, 0x78)] {
        builder.add_chip_write(Instance::Primary, Ym2151Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(10));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 8, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<Ym2151State>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let output = Rc::clone(&events);
    player.on_state_change::<Ym2151Spec, _>(move |instance, frame, changes| {
        output.borrow_mut().push((instance, frame, changes));
    });
    while player.render().unwrap().is_some() {}
    let events = events.borrow();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, Instance::Primary);
    let mut expected = Ym2151State::new(2_000_000.0);
    expected.on_register_write(0x28, 0x4c);
    expected.on_register_write(0x30, 0);
    assert_eq!(events[0].2, expected.on_register_write(0x08, 0x78).unwrap());
}

#[test]
fn state_only_overflow_is_reported_without_stopping_playback() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x28,
            value: 0x4c,
        },
    );
    for index in 0..5000 {
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x08,
                value: if index % 2 == 0 { 0x78 } else { 0 },
            },
        );
    }
    builder.add_vgm_command(WaitSamples(10));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 8, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<Ym2151State>();
    player.on_state_change::<Ym2151Spec, _>(|_, _, _| {});
    let mut overflow = false;
    while player.render().unwrap().is_some() {
        overflow |= player.take_state_event_overflow();
    }
    overflow |= player.take_state_event_overflow();
    assert!(overflow);
    assert!(!player.take_state_event_overflow());
}

#[test]
fn state_tracking_reports_optional_events_for_filtered_writes() {
    let mut player = VgmSequencer::new(document(), 44_100, 40, Default::default()).unwrap();
    player
        .track_state::<Ym2151State>(Instance::Primary, 4_000_000.0)
        .unwrap();
    let (_, events) = collect(&mut player);
    let key_events = events
        .iter()
        .filter(|(_, instance, spec, _)| *instance == Instance::Primary && spec.register == 0x08)
        .collect::<Vec<_>>();
    assert_eq!(key_events.len(), 2);
    match key_events[0].3.as_deref() {
        Some([StateEvent::KeyOn { channel: 0, tone }]) => {
            assert!(tone.freq_hz.unwrap() > 0.0)
        }
        other => panic!("unexpected events: {other:?}"),
    }
    assert!(matches!(
        key_events[1].3.as_deref(),
        Some([StateEvent::KeyOff { channel: 0 }])
    ));
}

#[test]
fn generated_dac_writes_are_delivered_as_typed_events() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 0,
        data_type: 0,
        size: 2,
        data: vec![0x34, 0x56],
    });
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 1,
            register: 0xa0,
            value: 7,
        },
    );
    builder.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(1));
    builder.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(1));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 1, Default::default()).unwrap();
    let captured = Rc::new(RefCell::new(Vec::new()));
    let callback_events = Rc::clone(&captured);
    player.on_write(move |instance, spec: Ym2612Spec, sample, _| {
        if spec.port == 0 && spec.register == 0x2a {
            callback_events.borrow_mut().push((
                sample,
                instance,
                spec.port,
                spec.register,
                spec.value,
            ));
        }
    });
    while player.render().unwrap().is_some() {}
    assert_eq!(
        *captured.borrow(),
        vec![
            (1, Instance::Primary, 0, 0x2a, 0x34),
            (2, Instance::Primary, 0, 0x2a, 0x56)
        ]
    );
}

#[test]
fn callback_queue_limit_and_render_failure_are_explicit() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    for value in 0..=4096 {
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x20,
                value: value as u8,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(10));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 8, Default::default()).unwrap();
    player.on_write(|_, _: Ym2151Spec, _, _| {});
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::EventQueueFull { limit: 4096 })
    ));
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::StreamFailed)
    ));
}

#[test]
fn zero_length_stream_delivers_final_event_at_zero() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x28,
            value: 0x4c,
        },
    );
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 64, Default::default()).unwrap();
    let captured = Rc::new(RefCell::new(Vec::new()));
    let callback_events = Rc::clone(&captured);
    player.on_write(move |instance, spec: Ym2151Spec, sample, _| {
        callback_events.borrow_mut().push((sample, instance, spec));
    });
    assert!(player.render().unwrap().is_none());
    assert_eq!(
        *captured.borrow(),
        vec![(
            0,
            Instance::Primary,
            Ym2151Spec {
                register: 0x28,
                value: 0x4c,
            }
        )]
    );
}

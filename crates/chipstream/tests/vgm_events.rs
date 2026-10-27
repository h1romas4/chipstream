use chipstream::sequencer::{
    Chip, Instance, StateEvent, VgmEventFilter, VgmSequencer, VgmSequencerError, VgmWriteEvent,
};
use soundlog::chip::{Ym2151Spec, Ym2612Spec};
use soundlog::vgm::command::{DataBlock, WaitSamples, Ym2612Port0Address2AWriteAndWaitN};
use soundlog::vgm::{VgmBuilder, VgmDocument};

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

fn collect(player: &mut VgmSequencer) -> (Vec<[f32; 2]>, Vec<(usize, VgmWriteEvent)>) {
    let mut pcm = Vec::new();
    let mut events = Vec::new();
    loop {
        let base = pcm.len();
        let block = player
            .render_with_events(|event| {
                events.push((base + event.frame_offset as usize, event.clone()));
            })
            .unwrap();
        let Some(block) = block else { break };
        pcm.extend_from_slice(block.frames());
    }
    (pcm, events)
}

#[test]
fn fade_preserves_event_offsets_and_rejects_late_configuration() {
    let mut small = VgmSequencer::new(document(), 44_100, 31).unwrap();
    let mut large = VgmSequencer::new(document(), 44_100, 512).unwrap();
    for player in [&mut small, &mut large] {
        player.set_loop_count(Some(2));
        player.set_fadeout_samples(Some(150)).unwrap();
        player
            .configure_events(VgmEventFilter {
                register_writes: true,
                ..Default::default()
            })
            .unwrap();
    }
    let (a, events_a) = collect(&mut small);
    let (b, events_b) = collect(&mut large);
    assert_eq!(a, b);
    assert_eq!(a.len(), 450);
    assert_eq!(a.last(), Some(&[0.0, 0.0]));
    let normalize = |events: Vec<(usize, VgmWriteEvent)>| {
        events
            .into_iter()
            .map(|(position, mut event)| {
                event.frame_offset = 0;
                (position, event)
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(normalize(events_a), normalize(events_b));
    assert!(matches!(
        small.set_fadeout_samples(Some(1)),
        Err(VgmSequencerError::FadeoutConfigurationAfterPlayback)
    ));
}

#[test]
fn event_timing_matches_pcm_across_chunks_loop_and_final_boundary() {
    let doc = document();
    let mut small = VgmSequencer::new(doc.clone(), 44_100, 31).unwrap();
    let mut large = VgmSequencer::new(doc.clone(), 44_100, 256).unwrap();
    let mut plain = VgmSequencer::new(doc, 44_100, 71).unwrap();
    let filter = VgmEventFilter {
        register_writes: true,
        ..Default::default()
    };
    small.configure_events(filter.clone()).unwrap();
    large.configure_events(filter).unwrap();
    small.set_loop_count(Some(2));
    large.set_loop_count(Some(2));
    plain.set_loop_count(Some(2));
    let (a, events_a) = collect(&mut small);
    let (b, events_b) = collect(&mut large);
    let (c, _) = collect(&mut plain);
    assert_eq!(a, b);
    assert_eq!(a, c);
    assert_eq!(a.len(), 300);
    let flatten = |events: Vec<(usize, VgmWriteEvent)>| {
        events
            .into_iter()
            .map(|(position, mut event)| {
                event.frame_offset = 0;
                (position, event)
            })
            .collect::<Vec<_>>()
    };
    let events = flatten(events_a);
    assert_eq!(events, flatten(events_b));
    assert_eq!(
        events.iter().map(|(pos, _)| *pos).collect::<Vec<_>>(),
        vec![0, 0, 0, 0, 100, 200, 200, 300]
    );
    assert!(matches!(
        events[3].1.state_events.as_slice(),
        [StateEvent::KeyOn { channel: 0, .. }]
    ));
    assert!(matches!(
        events[4].1.state_events.as_slice(),
        [StateEvent::ToneChange { channel: 0, .. }]
    ));
    assert!(matches!(
        events[5].1.state_events.as_slice(),
        [StateEvent::KeyOff { channel: 0 }]
    ));
    assert!(events.last().unwrap().1.state_events.is_empty());
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
    let mut player = VgmSequencer::new(builder.finalize(), 48_000, 17).unwrap();
    player
        .configure_events(VgmEventFilter {
            register_writes: true,
            state_changes: false,
            ..Default::default()
        })
        .unwrap();
    let (_, events) = collect(&mut player);
    assert_eq!(
        events
            .iter()
            .map(|(position, _)| *position)
            .collect::<Vec<_>>(),
        vec![0, 1, 110]
    );
}

#[test]
fn multiport_register_writes_report_their_port() {
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
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 32).unwrap();
    player
        .configure_events(VgmEventFilter {
            port: Some(1),
            register_writes: true,
            ..Default::default()
        })
        .unwrap();
    let (_, events) = collect(&mut player);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1.port, 1);
}

#[test]
fn streamed_tracker_uses_extra_header_clock() {
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
    let bytes: Vec<u8> = builder.finalize().into();
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 8).unwrap();
    let mut parsed = VgmSequencer::new(document, 44_100, 8).unwrap();
    stream.configure_events(VgmEventFilter::default()).unwrap();
    parsed.configure_events(VgmEventFilter::default()).unwrap();
    let (a, events) = collect(&mut stream);
    assert_eq!((a, events.clone()), collect(&mut parsed));
    use soundlog::chip::state::{ChipState, Ym2151State};
    let mut expected = Ym2151State::new(2_000_000.0);
    expected.on_register_write(0x28, 0x4c);
    expected.on_register_write(0x30, 0);
    assert_eq!(
        events[0].1.state_events,
        expected.on_register_write(0x08, 0x78).unwrap()
    );
}

#[test]
fn state_tracking_includes_suppressed_writes_and_filters_instances() {
    let mut player = VgmSequencer::new(document(), 44_100, 40).unwrap();
    player
        .configure_events(VgmEventFilter {
            chip: Some(Chip::Ym2151),
            instance: Some(Instance::Primary),
            register: Some(0x08),
            ..Default::default()
        })
        .unwrap();
    let (_, events) = collect(&mut player);
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, 0);
    match events[0].1.state_events.as_slice() {
        [StateEvent::KeyOn { channel: 0, tone }] => assert!(tone.freq_hz.unwrap() > 0.0),
        other => panic!("unexpected events: {other:?}"),
    }
    assert!(matches!(
        events[1].1.state_events.as_slice(),
        [StateEvent::KeyOff { channel: 0 }]
    ));
}

#[test]
fn port_and_generated_dac_writes_are_filtered() {
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
    let doc = builder.finalize();
    let mut player = VgmSequencer::new(doc, 44_100, 1).unwrap();
    player
        .configure_events(VgmEventFilter {
            chip: Some(Chip::Ym2612),
            port: Some(0),
            register: Some(0x2a),
            register_writes: true,
            state_changes: false,
            ..Default::default()
        })
        .unwrap();
    let (_, events) = collect(&mut player);
    assert_eq!(events.len(), 2);
    assert_eq!(
        events
            .iter()
            .map(|(pos, event)| (*pos, event.port, event.register, event.value))
            .collect::<Vec<_>>(),
        vec![(0, 0, 0x2a, 0x34), (1, 0, 0x2a, 0x56)]
    );
}

#[test]
fn invalid_configuration_queue_limit_and_required_callback_are_explicit() {
    let doc = document();
    let mut player = VgmSequencer::new(doc.clone(), 44_100, 8).unwrap();
    assert!(matches!(
        player.configure_events(VgmEventFilter {
            max_pending_events: 0,
            ..Default::default()
        }),
        Err(VgmSequencerError::InvalidEventFilter)
    ));
    player
        .configure_events(VgmEventFilter {
            register_writes: true,
            max_pending_events: 1,
            ..Default::default()
        })
        .unwrap();
    let mut called = false;
    assert!(matches!(
        player.render_with_events(|_| called = true),
        Err(VgmSequencerError::EventQueueFull { limit: 1 })
    ));
    assert!(!called);
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::StreamFailed)
    ));
    assert!(matches!(
        player.configure_events(VgmEventFilter::default()),
        Err(VgmSequencerError::EventConfigurationAfterPlayback)
    ));

    let mut player = VgmSequencer::new(doc, 44_100, 40).unwrap();
    player.configure_events(VgmEventFilter::default()).unwrap();
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::EventCallbackRequired)
    ));
    let (_, events) = collect(&mut player);
    assert_eq!(events[0].0, 0);
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
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 64).unwrap();
    player
        .configure_events(VgmEventFilter {
            register_writes: true,
            ..Default::default()
        })
        .unwrap();
    let (pcm, events) = collect(&mut player);
    assert!(pcm.is_empty());
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, 0);
}

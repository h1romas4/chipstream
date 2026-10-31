use std::cell::RefCell;
use std::rc::Rc;

use chipstream::sequencer::{Chip, Instance, StateEvent, VgmSequencer, VgmSequencerError};
use soundlog::chip::state::{ChipState, Ym2151State};
use soundlog::chip::{Ym2151Spec, Ym2612Spec};
use soundlog::vgm::command::{DataBlock, WaitSamples, Ym2612Port0Address2AWriteAndWaitN};
use soundlog::vgm::{VgmBuilder, VgmDocument};

type Ym2151Event = (usize, Instance, Ym2151Spec, Option<Vec<StateEvent>>);

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
fn callback_timing_is_chunk_invariant_across_loops_and_fadeout() {
    let mut small = VgmSequencer::new(document(), 44_100, 31).unwrap();
    let mut large = VgmSequencer::new(document(), 44_100, 512).unwrap();
    for player in [&mut small, &mut large] {
        player.set_loop_count(Some(2));
        player.set_fadeout_samples(Some(150)).unwrap();
        player.track_state::<Ym2151State>(Instance::Primary, 4_000_000.0);
        player.track_state::<Ym2151State>(Instance::Secondary, 4_000_000.0);
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
    let mut small = VgmSequencer::new(doc.clone(), 44_100, 31).unwrap();
    let mut large = VgmSequencer::new(doc.clone(), 44_100, 256).unwrap();
    let mut plain = VgmSequencer::new(doc, 44_100, 71).unwrap();
    for player in [&mut small, &mut large] {
        player.set_loop_count(Some(2));
        player.track_state::<Ym2151State>(Instance::Primary, 4_000_000.0);
        player.track_state::<Ym2151State>(Instance::Secondary, 4_000_000.0);
    }
    small.on_write(|_, _: Ym2151Spec, _, _| {});
    large.on_write(|_, _: Ym2151Spec, _, _| {});
    plain.set_loop_count(Some(2));
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
    let mut player = VgmSequencer::new(builder.finalize(), 48_000, 17).unwrap();
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
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 32).unwrap();
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
    let bytes: Vec<u8> = builder.finalize().into();
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
    let mut stream = VgmSequencer::from_vgm(bytes, 44_100, 8).unwrap();
    let mut parsed = VgmSequencer::new(document, 44_100, 8).unwrap();
    for player in [&mut stream, &mut parsed] {
        player.track_state::<Ym2151State>(Instance::Primary, 2_000_000.0);
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
fn state_tracking_reports_optional_events_for_filtered_writes() {
    let mut player = VgmSequencer::new(document(), 44_100, 40).unwrap();
    player.track_state::<Ym2151State>(Instance::Primary, 4_000_000.0);
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
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 1).unwrap();
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
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 8).unwrap();
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
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 64).unwrap();
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

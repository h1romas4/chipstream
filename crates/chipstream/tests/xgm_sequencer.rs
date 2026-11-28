#![cfg(all(feature = "xgm", feature = "sn76496"))]

use std::{cell::RefCell, rc::Rc, time::Duration};

use chipstream::audio::Gain;
use chipstream::sequencer::{
    Chip, Instance, PlaybackDuration, PlaybackOptions, StateEvent, XgmSequencer,
    XgmSequencerOptions, measure_xgm_duration_frames,
};
use soundlog::chip::{PsgSpec, state::Sn76489State};
use soundlog::xgm::{XgmBuilder, XgmCommand, XgmDocument, XgmTiming, XgmTrack};

fn document(timing: XgmTiming, looping: bool) -> XgmDocument {
    let mut builder = XgmBuilder::new(timing);
    builder.add_track(XgmTrack {
        commands: vec![
            XgmCommand::Psg(vec![0x84, 0x10, 0x90]),
            XgmCommand::Frame,
            XgmCommand::Frame,
            if looping {
                XgmCommand::Loop(1)
            } else {
                XgmCommand::End
            },
        ],
        gd3: None,
    });
    builder.finalize().unwrap()
}

#[test]
fn duration_and_seek_match_rendering_with_loop_and_fade() {
    for (timing, samples_per_frame) in [(XgmTiming::Ntsc, 735u32), (XgmTiming::Pal, 882)] {
        for rate in [44_100, 48_000] {
            let doc = document(timing, true);
            let options = XgmSequencerOptions {
                playback: PlaybackOptions {
                    loop_count: Some(2),
                    fadeout: Some(Duration::from_millis(10)),
                },
                ..Default::default()
            };
            let measured = measure_xgm_duration_frames(doc.clone(), rate, options).unwrap();
            let mut player = XgmSequencer::new(doc, rate, 128, options).unwrap();
            let start = player.checkpoint().unwrap();
            let mut frames = Vec::new();
            let mut fade_checkpoint = None;
            while let Some(block) = player.render().unwrap() {
                frames.extend_from_slice(block.frames());
                if fade_checkpoint.is_none()
                    && player
                        .fade_start_frame()
                        .is_some_and(|start| frames.len() > start)
                {
                    fade_checkpoint = Some(player.checkpoint().unwrap());
                }
            }
            let step = ((u128::from(rate) << 32) + 22_050) / 44_100;
            let expected =
                ((u128::from(samples_per_frame * 4 + 441) * step + (1 << 31)) >> 32) as usize;
            assert_eq!(frames.len(), expected);
            assert_eq!(measured, PlaybackDuration::Finite(expected as u32));
            let fade_start = player.fade_start_frame().unwrap();
            assert!(frames.iter().any(|frame| frame[0] != 0.0));
            for target in [777, fade_start + 10] {
                player.seek_to_frame(&start, target).unwrap();
                let mut actual = Vec::new();
                while let Some(block) = player.render().unwrap() {
                    actual.extend_from_slice(block.frames());
                }
                assert_eq!(actual, frames[target..]);
            }
            let checkpoint = fade_checkpoint.unwrap();
            let target = frames.len() - 10;
            player.seek_to_frame(&checkpoint, target).unwrap();
            let mut actual = Vec::new();
            while let Some(block) = player.render().unwrap() {
                actual.extend_from_slice(block.frames());
            }
            assert_eq!(actual, frames[target..]);
        }
    }
}

#[test]
fn unlimited_playback_still_allows_natural_end() {
    let options = XgmSequencerOptions {
        playback: PlaybackOptions {
            loop_count: None,
            fadeout: Some(Duration::from_secs(1)),
        },
        max_frames: None,
        ..Default::default()
    };
    let doc = document(XgmTiming::Ntsc, false);
    assert_eq!(
        measure_xgm_duration_frames(doc, 44_100, options).unwrap(),
        PlaybackDuration::Finite(1470)
    );
    let doc = document(XgmTiming::Ntsc, true);
    assert_eq!(
        measure_xgm_duration_frames(doc.clone(), 44_100, options).unwrap(),
        PlaybackDuration::Unknown
    );
    let mut player = XgmSequencer::new(doc, 44_100, 128, options).unwrap();
    for _ in 0..50 {
        assert!(player.render().unwrap().is_some());
    }
    assert_eq!(player.fade_start_frame(), None);
}

#[test]
fn natural_end_and_resource_limits_are_preserved() {
    let options = XgmSequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(3),
            fadeout: Some(Duration::from_secs(1)),
        },
        ..Default::default()
    };
    let doc = document(XgmTiming::Ntsc, false);
    let mut player = XgmSequencer::new(doc.clone(), 44_100, 128, options).unwrap();
    let mut total = 0;
    while let Some(block) = player.render().unwrap() {
        total += block.frames().len();
    }
    assert_eq!(total, 1470);
    assert_eq!(player.fade_start_frame(), None);
    assert_eq!(
        measure_xgm_duration_frames(doc, 44_100, options).unwrap(),
        PlaybackDuration::Finite(1470)
    );

    let limited = XgmSequencerOptions {
        max_frames: Some(1),
        ..options
    };
    let doc = document(XgmTiming::Ntsc, true);
    assert!(measure_xgm_duration_frames(doc.clone(), 44_100, limited).is_err());
    let mut player = XgmSequencer::new(doc, 44_100, 128, limited).unwrap();
    assert!(loop {
        match player.render() {
            Err(_) => break true,
            Ok(None) => break false,
            Ok(Some(_)) => {}
        }
    });
}

#[test]
fn typed_writes_and_state_tracking_follow_xgm_playback() {
    let options = XgmSequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(2),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut player =
        XgmSequencer::new(document(XgmTiming::Ntsc, true), 44_100, 128, options).unwrap();
    assert!(matches!(
        player.track_state::<Sn76489State>(Instance::Secondary, 3_579_545.0),
        Err(chipstream::sequencer::VgmSequencerError::ChipNotFound {
            chip: Chip::Sn76489,
            instance: Instance::Secondary
        })
    ));
    assert_eq!(player.track_state_with_voice_clocks::<Sn76489State>(), 1);
    player
        .track_state::<Sn76489State>(Instance::Primary, 3_579_545.0)
        .unwrap();
    let writes = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&writes);
    player.on_write(move |instance, spec: PsgSpec, frame, events| {
        captured
            .borrow_mut()
            .push((instance, spec.value, frame, events));
    });

    let mut previous_sample = player.current_sample();
    while player.render().unwrap().is_some() {
        assert!(player.current_sample() >= previous_sample);
        previous_sample = player.current_sample();
    }
    assert!(previous_sample > 1470);
    let writes = writes.borrow();
    assert!(writes.iter().any(|(instance, value, frame, events)| {
        *instance == Instance::Primary
            && *value == 0x90
            && *frame == 0
            && events.as_ref().is_some_and(|events| {
                events
                    .iter()
                    .any(|event| matches!(event, StateEvent::KeyOn { channel: 0, .. }))
            })
    }));
}

#[test]
fn xgm_output_effects_and_pan_are_applied_and_checkpointed() {
    let mut player = XgmSequencer::new(
        document(XgmTiming::Ntsc, false),
        44_100,
        128,
        Default::default(),
    )
    .unwrap();
    player
        .set_chip_gain(Chip::Ym2612, Instance::Primary, 0.0)
        .unwrap();
    for output in 0..2 {
        player
            .set_output_pan(Chip::Sn76489, Instance::Primary, output, Some(-1.0))
            .unwrap();
    }
    let native = player
        .add_output_effect(Chip::Sn76489, Instance::Primary, 0, Gain::new(0.5).unwrap())
        .unwrap();
    let group = player
        .create_output_group(Chip::Sn76489, Instance::Primary, &[0, 1])
        .unwrap();
    let gain = player
        .add_group_effect(&group, Gain::new(1.0).unwrap())
        .unwrap();
    let mute = player
        .add_group_effect(&group, Gain::new(0.0).unwrap())
        .unwrap();
    player.move_effect(&mute, 0).unwrap();
    let checkpoint = player.checkpoint().unwrap();
    let muted = player.render().unwrap().unwrap().frames().to_vec();
    assert!(muted.iter().all(|frame| *frame == [0.0, 0.0]));

    player.effect_mut(&mute).unwrap().set_gain(1.0).unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.effect_mut(&mute).unwrap().gain(), 0.0);
    assert_eq!(player.render().unwrap().unwrap().frames(), muted);

    player.effect_mut(&mute).unwrap().set_gain(1.0).unwrap();
    let audible = player.render().unwrap().unwrap();
    assert!(audible.frames().iter().any(|frame| frame[0] != 0.0));
    assert!(audible.frames().iter().all(|frame| frame[1] == 0.0));

    player.remove_effect(&native).unwrap();
    player.remove_effect(&gain).unwrap();
    player.remove_output_group(&group).unwrap();
    assert!(player.effect_mut(&mute).is_err());
}

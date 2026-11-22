#![cfg(feature = "sn76496")]

use std::time::Duration;

use chipstream::sequencer::{
    PlaybackDuration, PlaybackOptions, XgmSequencer, XgmSequencerOptions,
    measure_xgm_duration_frames,
};
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

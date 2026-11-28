use chipstream::sequencer::{
    GeneratorConfig, GeneratorLoopMarker, GeneratorStreamLimits, VgmSequencer,
    execution_event_channel,
};
use soundlog::s98::{
    S98Builder, S98Command, S98Header, S98LoopMarker, S98PlaybackOptions, S98Port, S98Version,
    S98VgmGenerator,
};
use soundlog::{ParseError, VgmCommand, VgmCommandGenerator};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

#[derive(Debug)]
struct Bridge {
    generator: S98VgmGenerator,
    source_marker: S98LoopMarker,
    marker: GeneratorLoopMarker,
}

impl VgmCommandGenerator for Bridge {
    fn next_command(&mut self) -> Result<Option<VgmCommand>, ParseError> {
        let command = self.generator.next_command()?;
        if self.marker.sample().is_none()
            && let Some(sample) = self.source_marker.sample()
        {
            self.marker.mark(usize::try_from(sample).map_err(|_| {
                ParseError::DataInconsistency("S98 loop sample exceeds usize".into())
            })?)?;
        }
        Ok(command)
    }
}

#[test]
fn factory_events_fade_and_checkpoint_replay() {
    let mut header = S98Header::new(S98Version::V3);
    header.timer_numerator = 1;
    header.timer_denominator = 44_100;
    let mut builder = S98Builder::new(header);
    builder.push(S98Command::Write {
        device: 0,
        port: S98Port::Normal,
        register: 0x22,
        value: 0,
    });
    builder.push(S98Command::Wait(100)).mark_loop();
    builder.push(S98Command::Write {
        device: 0,
        port: S98Port::Normal,
        register: 0x22,
        value: 8,
    });
    builder.push(S98Command::Wait(100));
    let document = builder.finalize().unwrap();
    let header = S98VgmGenerator::new(document.clone(), Default::default())
        .unwrap()
        .vgm_header()
        .clone();
    let marker = GeneratorLoopMarker::default();
    let config = GeneratorConfig {
        header,
        loop_marker: Some(marker.clone()),
        fadeout: Some(Duration::from_secs_f64(50.0 / 44_100.0)),
        stream_limits: GeneratorStreamLimits {
            max_commands_without_wait: Some(20),
            ..Default::default()
        },
        ..Default::default()
    };
    let (sender, receiver) = execution_event_channel();
    let factory_marker = marker.clone();
    let factory = move || -> Result<Box<dyn VgmCommandGenerator>, ParseError> {
        let (mut generator, source_marker) =
            S98VgmGenerator::new_with_loop_marker(document.clone(), S98PlaybackOptions::default())?;
        let sender = sender.clone();
        generator.set_observer(move |event| {
            sender
                .send(event)
                .map_err(|error| ParseError::GeneratorError(Box::new(error)))
        });
        Ok(Box::new(Bridge {
            generator,
            source_marker,
            marker: factory_marker.clone(),
        }))
    };
    let mut player = VgmSequencer::from_generator_factory(factory, config, 44_100, 64).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let captured = seen.clone();
    player
        .on_execution(receiver, move |frame, event| {
            captured.borrow_mut().push((frame, event.data))
        })
        .unwrap();
    let initial = player.checkpoint().unwrap();
    player.render().unwrap().unwrap();
    let checkpoint = player.checkpoint().unwrap();
    seen.borrow_mut().clear();
    let mut expected = Vec::new();
    while let Some(block) = player.render().unwrap() {
        expected.extend_from_slice(block.frames());
    }

    let expected_events = seen.borrow().clone();
    assert_eq!(marker.sample(), Some(200));
    assert_eq!(player.position_frames(), 250);
    assert_eq!(
        expected_events,
        vec![(100, 2), (100, 3), (200, 2), (200, 3)]
    );
    seen.borrow_mut().clear();
    player.restore_checkpoint(&checkpoint).unwrap();
    let mut replayed = Vec::new();
    while let Some(block) = player.render().unwrap() {
        replayed.extend_from_slice(block.frames());
    }
    assert_eq!(replayed, expected);
    assert_eq!(*seen.borrow(), expected_events);
    seen.borrow_mut().clear();
    player.seek_to_frame(&initial, 100).unwrap();
    while player.render().unwrap().is_some() {}
    assert_eq!(*seen.borrow(), expected_events);
}

#[test]
fn generator_stream_limits_apply_to_s98() {
    let mut builder = S98Builder::new(S98Header::new(S98Version::V3));
    builder
        .push(S98Command::Write {
            device: 0,
            port: S98Port::Normal,
            register: 0x22,
            value: 0,
        })
        .push(S98Command::Wait(1));
    let generator = S98VgmGenerator::new(builder.finalize().unwrap(), Default::default()).unwrap();
    let config = GeneratorConfig {
        header: generator.vgm_header().clone(),
        stream_limits: GeneratorStreamLimits {
            max_commands_without_wait: Some(0),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut player = VgmSequencer::from_generator(Box::new(generator), config, 44_100, 64).unwrap();
    assert!(player.render().is_err());
}

#[cfg(feature = "s98")]
#[test]
fn s98_sequencer_duration_fade_and_checkpoint_agree() {
    use chipstream::sequencer::{
        PlaybackDuration, PlaybackOptions, S98Sequencer, S98SequencerOptions,
        measure_s98_duration_frames,
    };

    let mut header = S98Header::new(S98Version::V3);
    header.timer_numerator = 1;
    header.timer_denominator = 44_100;
    let mut builder = S98Builder::new(header);
    builder.push(S98Command::Wait(100)).mark_loop();
    builder.push(S98Command::Wait(100_000));
    let document = builder.finalize().unwrap();
    for rate in [44_100, 48_000] {
        let options = S98SequencerOptions {
            playback: PlaybackOptions {
                loop_count: Some(2),
                fadeout: Some(Duration::from_secs_f64(50.0 / 44_100.0)),
                ..Default::default()
            },
            ..Default::default()
        };
        let measured = measure_s98_duration_frames(document.clone(), rate, options).unwrap();
        let mut player = S98Sequencer::new(document.clone(), rate, 257, options).unwrap();
        let initial = player.checkpoint().unwrap();
        let mut frames = 0;
        while let Some(block) = player.render().unwrap() {
            frames += block.frames().len();
        }
        let step = ((u128::from(rate) << 32) + 22_050) / 44_100;
        let expected = ((200_150_u128 * step + (1 << 31)) >> 32) as usize;
        assert_eq!(frames, expected);
        assert_eq!(measured, PlaybackDuration::Finite(expected as u32));
        player.restore_checkpoint(&initial).unwrap();
        player.seek_to_frame(&initial, 100).unwrap();
        while player.render().unwrap().is_some() {}
        assert_eq!(player.position_frames(), expected);
    }
}

#[cfg(feature = "s98")]
#[test]
fn s98_metadata_decodes_tags_and_rejects_invalid_text() {
    use chipstream::metadata::TrackMetadata;

    let mut legacy = S98Builder::new(S98Header::new(S98Version::V1));
    legacy.set_tag(vec![0x83, 0x65, 0x83, 0x58, 0x83, 0x67]);
    assert_eq!(
        TrackMetadata::from_s98(&legacy.finalize().unwrap())
            .unwrap()
            .title
            .as_deref(),
        Some("テスト")
    );
    let mut modern = S98Builder::new(S98Header::new(S98Version::V3));
    modern.set_tag(
        "[S98]\u{feff}title=曲\nartist=A=B\ngame=Game"
            .as_bytes()
            .to_vec(),
    );
    let metadata = TrackMetadata::from_s98(&modern.finalize().unwrap()).unwrap();
    assert_eq!(metadata.title.as_deref(), Some("曲"));
    assert_eq!(metadata.artist.as_deref(), Some("A=B"));
    assert_eq!(metadata.album.as_deref(), Some("Game"));
    let mut invalid = S98Builder::new(S98Header::new(S98Version::V1));
    invalid.set_tag(vec![0x81]);
    assert!(TrackMetadata::from_s98(&invalid.finalize().unwrap()).is_err());
    let mut invalid_utf8 = S98Builder::new(S98Header::new(S98Version::V3));
    invalid_utf8.set_tag(b"[S98]\xef\xbb\xbf\xff".to_vec());
    assert!(TrackMetadata::from_s98(&invalid_utf8.finalize().unwrap()).is_err());
}

#[cfg(feature = "s98")]
#[test]
fn s98_natural_end_and_tick_limit() {
    use chipstream::sequencer::{
        PlaybackDuration, PlaybackOptions, S98Sequencer, S98SequencerOptions,
        measure_s98_duration_frames,
    };
    let mut header = S98Header::new(S98Version::V3);
    header.timer_numerator = 1;
    header.timer_denominator = 44_100;
    let mut builder = S98Builder::new(header);
    builder.push(S98Command::Wait(100));
    let document = builder.finalize().unwrap();
    let options = S98SequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(2),
            fadeout: Some(Duration::from_secs(1)),
        },
        ..Default::default()
    };
    assert_eq!(
        measure_s98_duration_frames(document.clone(), 44_100, options).unwrap(),
        PlaybackDuration::Finite(100)
    );
    let mut player = S98Sequencer::new(document.clone(), 44_100, 64, options).unwrap();
    while player.render().unwrap().is_some() {}
    assert_eq!(player.position_frames(), 100);
    let limited = S98SequencerOptions {
        max_ticks: Some(99),
        ..options
    };
    assert!(measure_s98_duration_frames(document.clone(), 44_100, limited).is_err());
    let mut player = S98Sequencer::new(document, 44_100, 64, limited).unwrap();
    assert!(player.render().is_err());
}

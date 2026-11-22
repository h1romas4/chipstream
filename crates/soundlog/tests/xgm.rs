#![cfg(feature = "xgm")]

use soundlog::ParseError;
use soundlog::meta::Gd3;
use soundlog::vgm::command::VgmCommand;
use soundlog::vgm::stream::StreamResult;
use soundlog::vgm::{VgmCommandGenerator, VgmStream};
use soundlog::xgm::{
    XgmBuilder, XgmCommand, XgmDocument, XgmParseLimits, XgmPlaybackError, XgmPlaybackOptions,
    XgmRegisterWrite, XgmTiming, XgmTrack, XgmVgmGenerator,
};

fn track(commands: Vec<XgmCommand>) -> XgmTrack {
    XgmTrack {
        commands,
        gd3: None,
    }
}

#[test]
fn typed_multitrack_document_roundtrips_with_samples_and_metadata() {
    let mut builder = XgmBuilder::new(XgmTiming::Pal);
    builder.set_sample(1, &[0x40; 256]).unwrap();
    let metadata = Gd3 {
        track_name_en: Some("Test".into()),
        ..Default::default()
    };
    builder.add_track(XgmTrack {
        commands: vec![
            XgmCommand::Psg(vec![0x90, 0x20]),
            XgmCommand::Ym2612Port0(vec![XgmRegisterWrite {
                register: 0x22,
                value: 8,
            }]),
            XgmCommand::Ym2612Port1(vec![XgmRegisterWrite {
                register: 0x30,
                value: 3,
            }]),
            XgmCommand::Ym2612Key(vec![0xf0]),
            XgmCommand::Pcm {
                channel: 2,
                priority: 3,
                sample_id: 1,
            },
            XgmCommand::Frame,
            XgmCommand::Loop(1),
        ],
        gd3: Some(metadata.clone()),
    });
    builder.add_track(XgmTrack {
        commands: vec![XgmCommand::Frame, XgmCommand::End],
        gd3: Some(metadata),
    });
    let original = builder.finalize().unwrap();
    let bytes = original.to_bytes().unwrap();
    let parsed = XgmDocument::parse(&bytes).unwrap();
    assert_eq!(parsed, original);
    assert_eq!(parsed.header.timing, XgmTiming::Pal);
    assert_eq!(parsed.header.samples[0].unwrap().length, 256);
    assert!(parsed.header.samples[1].is_none());
    assert_eq!(parsed.to_bytes().unwrap(), bytes);
    let mut modified = parsed.clone();
    modified.header.timing = XgmTiming::Ntsc;
    modified.tracks.truncate(1);
    modified.tracks[0].gd3 = None;
    let changed = modified.to_bytes().unwrap();
    assert_eq!(changed[0x103], 0);
    assert_eq!(XgmDocument::parse(&changed).unwrap(), modified);

    let mut legacy = bytes.clone();
    let music_start = 0x104 + 256;
    let music_len =
        u32::from_le_bytes(legacy[music_start..music_start + 4].try_into().unwrap()) as usize;
    let gd3 = music_start + 4 + music_len;
    legacy[gd3 + 8..gd3 + 12].fill(0);
    assert_eq!(XgmDocument::parse(&legacy).unwrap(), original);
    assert!(XgmDocument::parse(&legacy[..gd3 + 16]).is_err());
}

#[test]
fn rejects_invalid_commands_sample_ranges_and_loop_targets() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    assert!(builder.set_sample(0, &[0; 256]).is_err());
    assert!(builder.set_sample(1, &[0; 255]).is_err());
    builder.add_track(track(vec![XgmCommand::Psg(vec![]), XgmCommand::End]));
    assert!(builder.finalize().is_err());

    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![XgmCommand::Loop(2)]));
    assert!(builder.finalize().is_err());

    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![
        XgmCommand::Pcm {
            channel: 0,
            priority: 0,
            sample_id: 1,
        },
        XgmCommand::End,
    ]));
    assert!(builder.finalize().is_err());

    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![
        XgmCommand::Psg(vec![0x90]),
        XgmCommand::Loop(0),
    ]));
    let bytes = builder.finalize().unwrap().to_bytes().unwrap();
    let mut bad = bytes.clone();
    let len = bad.len();
    bad[len - 3] = 1;
    assert!(XgmDocument::parse(&bad).is_err());
    assert!(XgmDocument::parse(&bytes[..bytes.len() - 1]).is_err());
}

#[test]
fn generator_mixes_pcm_and_preserves_frame_and_priority() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.set_sample(1, &[0x40; 256]).unwrap();
    builder.set_sample(2, &[0x20; 256]).unwrap();
    builder.add_track(track(vec![
        XgmCommand::Pcm {
            channel: 0,
            priority: 3,
            sample_id: 1,
        },
        XgmCommand::Pcm {
            channel: 0,
            priority: 0,
            sample_id: 2,
        },
        XgmCommand::Pcm {
            channel: 1,
            priority: 0,
            sample_id: 2,
        },
        XgmCommand::Frame,
        XgmCommand::End,
    ]));
    let generator = XgmVgmGenerator::new(builder.finalize().unwrap(), Default::default()).unwrap();
    let mut stream = VgmStream::from_generator(Box::new(generator));
    let mut time = 0;
    let mut dac = Vec::new();
    let mut enabled = false;
    loop {
        match stream.next().unwrap().unwrap() {
            StreamResult::Command(VgmCommand::WaitSamples(wait)) => time += usize::from(wait.0),
            StreamResult::Command(VgmCommand::Ym2612Write(_, write)) if write.register == 0x2b => {
                assert_eq!(write.value, 0x80);
                enabled = true;
            }
            StreamResult::Command(VgmCommand::Ym2612Write(_, write)) if write.register == 0x2a => {
                assert!(enabled);
                dac.push((time, write.value));
            }
            StreamResult::EndOfStream => break,
            StreamResult::NeedsMoreData => panic!("generator unexpectedly needs input"),
            _ => {}
        }
    }
    assert_eq!(time, 735);
    assert_eq!(dac.len(), 234);
    assert_eq!(
        &dac[..5],
        &[(0, 0xe0), (3, 0xe0), (6, 0xe0), (9, 0xe0), (12, 0xe0)]
    );
    assert!(dac.iter().all(|(_, value)| *value == 0xe0));
}

#[test]
fn loops_are_bounded_by_options_and_pal_timing() {
    let mut builder = XgmBuilder::new(XgmTiming::Pal);
    builder.add_track(track(vec![XgmCommand::Frame, XgmCommand::Loop(0)]));
    let doc = builder.finalize().unwrap();
    let mut generator = XgmVgmGenerator::new(
        doc,
        XgmPlaybackOptions {
            track: 0,
            loop_count: Some(2),
            ..Default::default()
        },
    )
    .unwrap();
    let mut waits = Vec::new();
    while let Some(command) = generator.next_command().unwrap() {
        if let VgmCommand::WaitSamples(wait) = command {
            waits.push(wait.0);
        }
    }
    assert_eq!(waits, [882, 882]);
    assert!(generator.finished_at_loop());
}

#[test]
fn loop_marker_continues_playback_but_respects_limits_and_natural_end() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![XgmCommand::Frame, XgmCommand::Loop(0)]));
    let document = builder.finalize().unwrap();
    let (mut generator, marker) =
        XgmVgmGenerator::new_with_loop_marker(document.clone(), Default::default()).unwrap();
    assert!(matches!(
        generator.next_command().unwrap(),
        Some(VgmCommand::WaitSamples(_))
    ));
    assert!(!marker.reached());
    assert!(matches!(
        generator.next_command().unwrap(),
        Some(VgmCommand::WaitSamples(_))
    ));
    assert!(marker.reached());
    assert!(
        XgmVgmGenerator::new_with_loop_marker(
            document,
            XgmPlaybackOptions {
                loop_count: None,
                ..Default::default()
            }
        )
        .is_err()
    );

    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![XgmCommand::Frame, XgmCommand::End]));
    let (mut generator, marker) =
        XgmVgmGenerator::new_with_loop_marker(builder.finalize().unwrap(), Default::default())
            .unwrap();
    while generator.next_command().unwrap().is_some() {}
    assert!(!marker.reached());

    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![XgmCommand::Loop(0)]));
    let (mut generator, marker) = XgmVgmGenerator::new_with_loop_marker(
        builder.finalize().unwrap(),
        XgmPlaybackOptions {
            max_commands_per_frame: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(generator.next_command().is_err());
    assert!(marker.reached());
}

#[test]
fn pcm_mixing_saturates_per_channel_and_returns_to_center() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.set_sample(1, &[127; 256]).unwrap();
    builder.set_sample(2, &[0x81; 256]).unwrap();
    builder.add_track(track(vec![
        XgmCommand::Pcm {
            channel: 0,
            priority: 0,
            sample_id: 1,
        },
        XgmCommand::Pcm {
            channel: 1,
            priority: 0,
            sample_id: 1,
        },
        XgmCommand::Pcm {
            channel: 2,
            priority: 0,
            sample_id: 2,
        },
        XgmCommand::Frame,
        XgmCommand::Frame,
        XgmCommand::End,
    ]));
    let mut generator =
        XgmVgmGenerator::new(builder.finalize().unwrap(), Default::default()).unwrap();
    let mut values = Vec::new();
    let mut enables = Vec::new();
    while let Some(command) = generator.next_command().unwrap() {
        if let VgmCommand::Ym2612Write(_, write) = command {
            match write.register {
                0x2a => values.push(write.value),
                0x2b => enables.push(write.value),
                _ => {}
            }
        }
        assert!(!generator.finished_at_loop());
    }
    assert_eq!(values.len(), 257);
    assert!(values.iter().all(|&value| value == 0x80));
    assert_eq!(enables, [0x80, 0]);
}

#[test]
fn a_loop_without_output_or_time_progress_fails() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![XgmCommand::Loop(0)]));
    let options = XgmPlaybackOptions {
        track: 0,
        loop_count: None,
        ..Default::default()
    };
    let mut generator = XgmVgmGenerator::new(builder.finalize().unwrap(), options).unwrap();
    assert!(generator.next_command().is_err());
}

#[test]
fn parse_limits_count_samples_tracks_commands_and_metadata() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.set_sample(1, &[0; 256]).unwrap();
    builder.set_sample(3, &[0; 256]).unwrap();
    for _ in 0..2 {
        builder.add_track(XgmTrack {
            commands: vec![XgmCommand::Frame, XgmCommand::End],
            gd3: Some(Gd3::default()),
        });
    }
    let bytes = builder.finalize().unwrap().to_bytes().unwrap();
    let limits = XgmParseLimits {
        max_sample_data_bytes: Some(512),
        max_sample_bytes: Some(256),
        max_samples: Some(2),
        max_tracks: Some(2),
        max_commands: Some(4),
        max_gd3_bytes: Some(34),
        max_total_gd3_bytes: Some(68),
    };
    assert!(XgmDocument::parse_with_limits(&bytes, limits).is_ok());
    for (stricter, resource) in [
        (
            XgmParseLimits {
                max_sample_data_bytes: Some(511),
                ..limits
            },
            "sample data bytes",
        ),
        (
            XgmParseLimits {
                max_sample_bytes: Some(255),
                ..limits
            },
            "sample length",
        ),
        (
            XgmParseLimits {
                max_samples: Some(1),
                ..limits
            },
            "sample count",
        ),
        (
            XgmParseLimits {
                max_tracks: Some(1),
                ..limits
            },
            "track count",
        ),
        (
            XgmParseLimits {
                max_commands: Some(3),
                ..limits
            },
            "command count",
        ),
        (
            XgmParseLimits {
                max_gd3_bytes: Some(33),
                ..limits
            },
            "GD3 bytes",
        ),
        (
            XgmParseLimits {
                max_total_gd3_bytes: Some(67),
                ..limits
            },
            "total GD3 bytes",
        ),
    ] {
        let error = XgmDocument::parse_with_limits(&bytes, stricter).unwrap_err();
        assert!(error.to_string().contains(resource), "{error}");
    }
    let mut zero_length = bytes.clone();
    for (limits, resource) in [
        (
            XgmParseLimits {
                max_sample_data_bytes: Some(511),
                ..limits
            },
            "sample data bytes",
        ),
        (
            XgmParseLimits {
                max_sample_bytes: Some(255),
                ..limits
            },
            "sample length",
        ),
        (
            XgmParseLimits {
                max_samples: Some(1),
                ..limits
            },
            "sample count",
        ),
    ] {
        let error = XgmDocument::parse_with_limits(&bytes[..0x104], limits).unwrap_err();
        assert!(error.to_string().contains(resource), "{error}");
    }
    let gd3 = 0x104 + 512 + 4 + 2;
    zero_length[gd3 + 8..gd3 + 12].fill(0);
    let second_gd3 = gd3 + 34 + 4 + 2;
    zero_length[second_gd3 + 8..second_gd3 + 12].fill(0);
    assert!(XgmDocument::parse_with_limits(&zero_length, limits).is_ok());
    assert!(
        XgmDocument::parse_with_limits(
            &zero_length,
            XgmParseLimits {
                max_gd3_bytes: Some(33),
                ..limits
            }
        )
        .is_err()
    );
    assert!(
        XgmDocument::parse_with_limits(
            &zero_length,
            XgmParseLimits {
                max_total_gd3_bytes: Some(67),
                ..limits
            }
        )
        .unwrap_err()
        .to_string()
        .contains("total GD3 bytes")
    );
    assert!(
        XgmDocument::parse_with_limits(
            &bytes,
            XgmParseLimits {
                max_sample_data_bytes: None,
                max_sample_bytes: None,
                max_samples: None,
                max_tracks: None,
                max_commands: None,
                max_gd3_bytes: None,
                max_total_gd3_bytes: None,
            }
        )
        .is_ok()
    );
}

#[test]
fn default_gd3_limits_apply_to_each_track_and_the_total() {
    let metadata = Gd3 {
        notes: Some("a".repeat((16 * 1024 - 34) / 2)),
        ..Default::default()
    };
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    for _ in 0..16 {
        builder.add_track(XgmTrack {
            commands: vec![XgmCommand::End],
            gd3: Some(metadata.clone()),
        });
    }
    let document = builder.finalize().unwrap();
    let bytes = document.to_bytes().unwrap();
    assert!(XgmDocument::parse(&bytes).is_ok());
    let gd3 = 0x104 + 4 + 1;
    let mut legacy = bytes.clone();
    legacy[gd3 + 8..gd3 + 12].fill(0);
    assert!(XgmDocument::parse(&legacy).is_ok());

    let mut too_many = document.clone();
    too_many.tracks.push(XgmTrack {
        commands: vec![XgmCommand::End],
        gd3: Some(Gd3::default()),
    });
    let bytes = too_many.to_bytes().unwrap();
    let error = XgmDocument::parse(&bytes).unwrap_err();
    assert!(error.to_string().contains("total GD3 bytes"), "{error}");
    assert!(
        XgmDocument::parse_with_limits(
            &bytes,
            XgmParseLimits {
                max_total_gd3_bytes: None,
                ..Default::default()
            }
        )
        .is_ok()
    );

    let mut too_large = document;
    too_large.tracks.truncate(1);
    too_large.tracks[0]
        .gd3
        .as_mut()
        .unwrap()
        .notes
        .as_mut()
        .unwrap()
        .push('a');
    let bytes = too_large.to_bytes().unwrap();
    let error = XgmDocument::parse(&bytes).unwrap_err();
    assert!(error.to_string().contains("GD3 bytes"), "{error}");
    let mut legacy = bytes.clone();
    legacy[gd3 + 8..gd3 + 12].fill(0);
    let error = XgmDocument::parse(&legacy).unwrap_err();
    assert!(error.to_string().contains("GD3 bytes"), "{error}");
    assert!(
        XgmDocument::parse_with_limits(
            &bytes,
            XgmParseLimits {
                max_gd3_bytes: None,
                ..Default::default()
            }
        )
        .is_ok()
    );
}

fn playback_error(error: ParseError) -> XgmPlaybackError {
    match error {
        ParseError::GeneratorError(error) => *error.downcast_ref::<XgmPlaybackError>().unwrap(),
        other => panic!("unexpected error: {other}"),
    }
}

#[test]
fn playback_limits_survive_output_and_reset_only_on_frame_waits() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![
        XgmCommand::Psg(vec![0x90]),
        XgmCommand::Loop(0),
    ]));
    let options = XgmPlaybackOptions {
        loop_count: None,
        max_commands_per_frame: Some(3),
        ..Default::default()
    };
    let mut generator = XgmVgmGenerator::new(builder.finalize().unwrap(), options).unwrap();
    assert!(matches!(
        generator.next_command().unwrap(),
        Some(VgmCommand::Sn76489Write(..))
    ));
    assert!(matches!(
        generator.next_command().unwrap(),
        Some(VgmCommand::Sn76489Write(..))
    ));
    let expected = XgmPlaybackError::CommandLimitExceeded {
        max_commands_per_frame: 3,
    };
    assert_eq!(
        playback_error(generator.next_command().unwrap_err()),
        expected
    );
    assert_eq!(
        playback_error(generator.next_command().unwrap_err()),
        expected
    );

    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![XgmCommand::Frame, XgmCommand::Loop(0)]));
    let doc = builder.finalize().unwrap();
    let options = XgmPlaybackOptions {
        loop_count: None,
        max_frames: Some(2),
        max_commands_per_frame: Some(2),
        ..Default::default()
    };
    let mut generator = XgmVgmGenerator::new(doc.clone(), options).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            generator.next_command().unwrap(),
            Some(VgmCommand::WaitSamples(_))
        ));
    }

    assert_eq!(
        playback_error(generator.next_command().unwrap_err()),
        XgmPlaybackError::FrameLimitExceeded { max_frames: 2 }
    );
    let unlimited = XgmPlaybackOptions {
        max_frames: None,
        max_commands_per_frame: None,
        ..options
    };
    let mut generator = XgmVgmGenerator::new(doc, unlimited).unwrap();
    for _ in 0..3 {
        assert!(matches!(
            generator.next_command().unwrap(),
            Some(VgmCommand::WaitSamples(_))
        ));
    }
}

#[test]
fn playback_limits_no_output_pcm_loops_and_zero_budget() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![
        XgmCommand::Pcm {
            channel: 0,
            priority: 0,
            sample_id: 0,
        },
        XgmCommand::Loop(0),
    ]));
    let doc = builder.finalize().unwrap();
    for limit in [0, 3] {
        let mut generator = XgmVgmGenerator::new(
            doc.clone(),
            XgmPlaybackOptions {
                loop_count: None,
                max_commands_per_frame: Some(limit),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            playback_error(generator.next_command().unwrap_err()),
            XgmPlaybackError::CommandLimitExceeded {
                max_commands_per_frame: limit
            }
        );
    }
    let generator = XgmVgmGenerator::new(
        doc,
        XgmPlaybackOptions {
            loop_count: None,
            max_commands_per_frame: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    let mut stream = VgmStream::from_generator(Box::new(generator));
    assert_eq!(
        playback_error(stream.next().unwrap().unwrap_err()),
        XgmPlaybackError::CommandLimitExceeded {
            max_commands_per_frame: 3
        }
    );
}

#[test]
fn default_limits_reject_excess_commands_and_frames() {
    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![XgmCommand::End]));
    let mut bytes = builder.finalize().unwrap().to_bytes().unwrap();
    bytes.truncate(0x104);
    bytes.extend_from_slice(&1_000_001u32.to_le_bytes());
    bytes.resize(0x104 + 4 + 1_000_000, 0);
    bytes.push(0x7f);
    let error = XgmDocument::parse(&bytes).unwrap_err();
    assert!(error.to_string().contains("command count"), "{error}");

    let mut builder = XgmBuilder::new(XgmTiming::Ntsc);
    builder.add_track(track(vec![XgmCommand::Frame, XgmCommand::Loop(0)]));
    let mut generator = XgmVgmGenerator::new(
        builder.finalize().unwrap(),
        XgmPlaybackOptions {
            loop_count: None,
            ..Default::default()
        },
    )
    .unwrap();
    for _ in 0..100_000 {
        assert!(matches!(
            generator.next_command().unwrap(),
            Some(VgmCommand::WaitSamples(_))
        ));
    }
    assert_eq!(
        playback_error(generator.next_command().unwrap_err()),
        XgmPlaybackError::FrameLimitExceeded {
            max_frames: 100_000
        }
    );
}

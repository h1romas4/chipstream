#![cfg(feature = "xgm")]

use soundlog::meta::Gd3;
use soundlog::vgm::command::VgmCommand;
use soundlog::vgm::stream::StreamResult;
use soundlog::vgm::{VgmCommandGenerator, VgmStream};
use soundlog::xgm::{
    XgmBuilder, XgmCommand, XgmDocument, XgmPlaybackOptions, XgmRegisterWrite, XgmTiming, XgmTrack,
    XgmVgmGenerator,
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
    assert_eq!(parsed.to_bytes().unwrap(), bytes);

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
    };
    let mut generator = XgmVgmGenerator::new(builder.finalize().unwrap(), options).unwrap();
    assert!(generator.next_command().is_err());
}

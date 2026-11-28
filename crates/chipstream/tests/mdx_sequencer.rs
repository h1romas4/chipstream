#![cfg(feature = "mdx")]

use std::error::Error;

use chipstream::sequencer::{
    Chip, ChipFactoryRegistry, Instance, MdxSequencer, MdxSequencerOptions, NativeOutput,
    PlaybackOptions, VgmSequencer,
};
use soundlog::chip::{Ym2151Spec, state::Ym2151State};
#[cfg(feature = "okim6258")]
use soundlog::mdx::command::MdxAdpcmOrNoiseFrequency;
use soundlog::mdx::command::{MdxNote, MdxPan, MdxRest, MdxVoiceOrPcmBank};
use soundlog::mdx::convert::{AdpcmMode, MdxConvertError, to_vgm_document};
use soundlog::mdx::document::MdxBuilder;
use soundlog::mdx::package::MdxPackage;
#[cfg(feature = "okim6258")]
use soundlog::mdx::pdx::PdxBuilder;
use soundlog::mdx::tone::{MdxOperator, MdxTone};

#[test]
fn mdx_execution_events_match_source_and_output_timeline() {
    use soundlog::mdx::command::MdxTempo;
    use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_stream_generator_with_observer};
    use soundlog::vgm::stream::StreamResult;
    use std::{cell::RefCell, rc::Rc};

    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(0, MdxTempo { value: 200 })
        .add_mdx_command(0, MdxRest { ticks: 2 })
        .add_mdx_command(0, MdxRest { ticks: 1 })
        .add_mdx_command(1, MdxRest { ticks: 1 });
    let package = MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: None,
    };
    let source_events = Rc::new(RefCell::new(Vec::new()));
    let observed = Rc::clone(&source_events);
    let generator = to_vgm_stream_generator_with_observer(
        package.clone(),
        MdxToVgmOptions::default(),
        move |event| observed.borrow_mut().push(event),
    )
    .unwrap();
    let mut stream = soundlog::VgmStream::from_generator(generator);
    loop {
        match stream.next().unwrap().unwrap() {
            StreamResult::EndOfStream => break,
            StreamResult::Command(_) => {}
            StreamResult::NeedsMoreData => panic!("generator unexpectedly needs data"),
        }
    }

    for (rate, chunk) in [(44_100, 31), (48_000, 128), (22_050, 17)] {
        let expected = source_events
            .borrow()
            .iter()
            .map(|event| {
                (
                    ((event.sample * u64::from(rate) + 22_050) / 44_100) as usize,
                    *event,
                )
            })
            .collect::<Vec<_>>();
        let mut player =
            MdxSequencer::new(package.clone(), rate, chunk, Default::default()).unwrap();
        let seen = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&seen);
        player
            .on_execution(move |frame, event| captured.borrow_mut().push((frame, *event)))
            .unwrap();
        let initial = player.checkpoint().unwrap();
        let audio = render_mdx(&mut player);
        assert_eq!(*seen.borrow(), expected);
        let baseline = render_mdx(
            &mut MdxSequencer::new(package.clone(), rate, chunk, Default::default()).unwrap(),
        );
        assert_eq!(audio, baseline);

        seen.borrow_mut().clear();
        let target = expected
            .iter()
            .find(|(_, event)| event.sample > 0)
            .unwrap()
            .0;
        player.seek_to_frame(&initial, target).unwrap();
        assert!(seen.borrow().is_empty());
        render_mdx(&mut player);
        assert_eq!(
            *seen.borrow(),
            expected
                .into_iter()
                .filter(|(frame, _)| *frame >= target)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn mdx_execution_events_work_with_loop_fade_and_reconstruction() {
    use soundlog::mdx::command::{MdxCommand, MdxRelativeOffset};
    use std::{cell::RefCell, rc::Rc, time::Duration};
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(0, MdxRest { ticks: 1 })
        .add_mdx_command(
            0,
            MdxCommand::EndOfTrackLoop(MdxRelativeOffset {
                opcode: 0xf1,
                offset: -4,
            }),
        );
    let package = MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: None,
    };
    let options = MdxSequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(2),
            fadeout: Some(Duration::from_millis(30)),
        },
        ..Default::default()
    };
    let mut player = MdxSequencer::new(package.clone(), 44_100, 128, options).unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&seen);
    player
        .on_execution(move |frame, event| captured.borrow_mut().push((frame, *event)))
        .unwrap();
    player.render().unwrap().unwrap();
    let checkpoint = player.checkpoint().unwrap();
    seen.borrow_mut().clear();
    let audio = render_mdx(&mut player);
    let expected = seen.borrow().clone();
    assert!(player.fade_start_frame().is_some());
    let mut baseline = MdxSequencer::new(package, 44_100, 128, options).unwrap();
    baseline.render().unwrap().unwrap();
    assert_eq!(render_mdx(&mut baseline), audio);
    assert!(
        expected
            .iter()
            .filter(|(_, event)| event.data.command_index == 0)
            .count()
            >= 2
    );
    seen.borrow_mut().clear();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert!(seen.borrow().is_empty());
    assert_eq!(render_mdx(&mut player), audio);
    assert_eq!(*seen.borrow(), expected);
}

#[test]
fn mdx_execution_event_overflow_stops_playback() {
    use chipstream::sequencer::{ExecutionEventError, MdxSequencerError, VgmSequencerError};
    use soundlog::mdx::command::MdxOpmRegisterWrite;
    let mut builder = MdxBuilder::new();
    for _ in 0..4097 {
        builder.add_mdx_command(
            0,
            MdxOpmRegisterWrite {
                register: 0x20,
                value: 0,
            },
        );
    }
    let package = MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: None,
    };
    let mut player = MdxSequencer::new(package, 44_100, 128, Default::default()).unwrap();
    player
        .on_execution(|_, _| panic!("overflowing events must not be delivered"))
        .unwrap();
    assert!(matches!(
        player.render(),
        Err(MdxSequencerError::Playback(
            VgmSequencerError::ExecutionEvent(ExecutionEventError::QueueFull { limit: 4096 })
        ))
    ));
    assert!(matches!(
        player.render(),
        Err(MdxSequencerError::Playback(VgmSequencerError::StreamFailed))
    ));
}

const MDX: &[u8] = include_bytes!("../assets/mdx/mdx_fm_loop.mdx");
#[cfg(feature = "okim6258")]
const PDX: &[u8] = include_bytes!("../assets/mdx/mdx_fm_loop.pdx");

fn render_mdx(player: &mut MdxSequencer<'_>) -> Vec<[f32; 2]> {
    let mut frames = Vec::new();
    while let Some(block) = player.render().unwrap() {
        frames.extend_from_slice(block.frames());
    }
    assert!(player.render().unwrap().is_none());
    frames
}

fn render_vgm(player: &mut VgmSequencer<'_>) -> Vec<[f32; 2]> {
    let mut frames = Vec::new();
    while let Some(block) = player.render().unwrap() {
        frames.extend_from_slice(block.frames());
    }
    frames
}

fn mdx_reference_factories() -> ChipFactoryRegistry {
    let factories = ChipFactoryRegistry::new();
    #[cfg(feature = "okim6258")]
    let factories = {
        let mut factories = factories;
        factories.register(Chip::Okim6258, |config| {
            use chipstream::chip::{ChipError, ClockDivider, Okim6258, Okim6258Stream, OutputBits};
            use chipstream::sequencer::{ChipBackendSettings, Okim6258Divider};
            let ChipBackendSettings::Okim6258(settings) = config.settings() else {
                return Err(ChipError::UnsupportedChip(config.chip().clone()));
            };
            let mut chip = Okim6258::new(config.clock())?;
            chip.set_divider(match settings.divider {
                Okim6258Divider::Div1024 => ClockDivider::Div1024,
                Okim6258Divider::Div768 => ClockDivider::Div768,
                Okim6258Divider::Div512 => ClockDivider::Div512,
            })?;
            chip.set_output_bits(if settings.output_12bit {
                OutputBits::Bits12
            } else {
                OutputBits::Bits10
            });
            Ok(Box::new(
                Okim6258Stream::new(chip).with_direct_write_queue(),
            ))
        });
        factories
    };
    factories
}

#[test]
fn current_sample_tracks_the_soundlog_command_position() {
    let mut player = MdxSequencer::new(fm_package(), 44_100, 256, Default::default()).unwrap();

    assert_eq!(player.current_sample(), 0);
    player.render().unwrap();
    assert!(player.current_sample() > 0);
}

#[test]
fn position_frames_counts_returned_pcm() {
    let mut player = MdxSequencer::new(fm_package(), 44_100, 256, Default::default()).unwrap();

    assert_eq!(player.position_frames(), 0);
    let block = player.render().unwrap().unwrap();
    assert_eq!(player.position_frames(), block.frames().len());
}

#[test]
fn mdx_native_output_gains_control_stereo_channels() {
    let baseline =
        render_mdx(&mut MdxSequencer::new(fm_package(), 48_000, 128, Default::default()).unwrap());
    let mut player = MdxSequencer::new(fm_package(), 48_000, 128, Default::default()).unwrap();
    assert_eq!(
        player
            .output_layout(Chip::Ym2151, Instance::Primary)
            .unwrap(),
        &[NativeOutput::Left, NativeOutput::Right]
    );
    player
        .set_output_gains(Chip::Ym2151, Instance::Primary, &[1.0, 0.0])
        .unwrap();
    let output = render_mdx(&mut player);
    assert_eq!(output.len(), baseline.len());
    assert!(output.iter().any(|frame| frame[0].abs() > 0.0001));
    for (actual, expected) in output.iter().zip(baseline) {
        assert_eq!(actual[0], expected[0]);
        assert_eq!(actual[1], 0.0);
    }
}

#[test]
fn mdx_stereo_group_effects_and_typed_controls() {
    use chipstream::audio::StereoBalance;
    let baseline =
        render_mdx(&mut MdxSequencer::new(fm_package(), 48_000, 128, Default::default()).unwrap());
    let mut player = MdxSequencer::new(fm_package(), 48_000, 128, Default::default()).unwrap();
    let group = player
        .create_output_group(Chip::Ym2151, Instance::Primary, &[0, 1])
        .unwrap();
    let effect = player
        .add_group_effect(&group, StereoBalance::new(0.0).unwrap())
        .unwrap();
    player
        .effect_mut(&effect)
        .unwrap()
        .set_balance(1.0)
        .unwrap();
    let output = render_mdx(&mut player);
    assert_eq!(output.len(), baseline.len());
    assert!(baseline.iter().any(|frame| frame[0].abs() > 0.0001));
    for (actual, expected) in output.iter().zip(baseline) {
        assert_eq!(actual[0], 0.0);
        assert_eq!(actual[1], expected[1]);
    }
}

fn fm_package() -> MdxPackage {
    let mut builder = MdxBuilder::new();
    builder
        .append_tone(MdxTone {
            voice_number: 0,
            con: 7,
            fl: 0,
            op: 0x78,
            operators: [MdxOperator {
                ar: 31,
                rr: 15,
                ml: 1,
                ..Default::default()
            }; 4],
        })
        .add_mdx_command(0, MdxVoiceOrPcmBank { value: 0 })
        .add_mdx_command(0, MdxPan::Center)
        .add_mdx_command(
            0,
            MdxNote {
                note: 0x98,
                length: 32,
            },
        )
        .add_mdx_command(0, MdxRest { ticks: 8 });
    MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: None,
    }
}

#[cfg(feature = "okim6258")]
fn pcm_package(extended: bool) -> MdxPackage {
    let mut builder = MdxBuilder::new();
    for mode in 0..=4 {
        builder
            .add_mdx_command(8, MdxAdpcmOrNoiseFrequency { value: mode })
            .add_mdx_command(8, MdxPan::Right)
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 8,
                },
            )
            .add_mdx_command(8, MdxPan::Left)
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 8,
                },
            );
    }
    if extended {
        builder.add_mdx_command(
            15,
            MdxNote {
                note: 0x81,
                length: 24,
            },
        );
    }
    let mut pdx = PdxBuilder::new();
    pdx.set_sample(0, 0, vec![0x71; 300]).unwrap();
    pdx.set_sample(0, 1, vec![0x37; 300]).unwrap();
    MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: Some(pdx.finalize()),
    }
}

#[test]
fn fm_and_pcm_match_eager_conversion_across_chunks() {
    for (package, mode) in [
        (fm_package(), AdpcmMode::Through),
        #[cfg(feature = "okim6258")]
        (pcm_package(false), AdpcmMode::Through),
        #[cfg(feature = "okim6258")]
        (pcm_package(false), AdpcmMode::Resample),
        #[cfg(feature = "okim6258")]
        (pcm_package(true), AdpcmMode::Through),
        #[cfg(feature = "okim6258")]
        (pcm_package(true), AdpcmMode::Lpf),
    ] {
        let options = MdxSequencerOptions {
            adpcm_mode: mode,
            ..Default::default()
        };
        let document = to_vgm_document(&package, &options.into()).unwrap();
        let mut reference = VgmSequencer::new_with_factories(
            document,
            48_000,
            512,
            Default::default(),
            mdx_reference_factories(),
        )
        .unwrap();
        let expected = render_vgm(&mut reference);
        assert!(
            expected
                .iter()
                .flatten()
                .any(|sample| sample.abs() > 0.0001)
        );
        for chunk in [31, 1024] {
            let mut mdx = MdxSequencer::new(package.clone(), 48_000, chunk, options).unwrap();
            assert_eq!(
                render_mdx(&mut mdx),
                expected,
                "chunk {chunk}, mode {mode:?}"
            );
        }
    }
}

#[test]
#[cfg(feature = "okim6258")]
fn finite_and_infinite_loops_are_driven_incrementally() {
    let package = MdxPackage::parse(MDX, Some(PDX)).unwrap();
    let options = |loop_count| MdxSequencerOptions {
        playback: PlaybackOptions {
            loop_count,
            fadeout: None,
        },
        max_ticks: None,
        ..Default::default()
    };
    let once =
        render_mdx(&mut MdxSequencer::new(package.clone(), 44_100, 256, options(Some(1))).unwrap());
    let twice =
        render_mdx(&mut MdxSequencer::new(package.clone(), 44_100, 257, options(Some(2))).unwrap());
    let mut expected = VgmSequencer::new_with_factories(
        to_vgm_document(&package, &options(Some(2)).into()).unwrap(),
        44_100,
        512,
        Default::default(),
        mdx_reference_factories(),
    )
    .unwrap();
    assert_eq!(twice, render_vgm(&mut expected));
    assert!(twice.len() > once.len());
    let mut infinite = MdxSequencer::new(package, 44_100, 256, options(None)).unwrap();
    let frames: Vec<_> = (0..(twice.len() / 256 + 2))
        .flat_map(|_| infinite.render().unwrap().unwrap().frames().to_vec())
        .collect();
    assert!(frames.len() > twice.len());
}

#[test]
fn mdx_constructors_apply_explicit_loop_options() {
    let package = MdxPackage::parse(MDX, None).unwrap();
    let once = render_mdx(
        &mut MdxSequencer::new(package.clone(), 44_100, 256, Default::default()).unwrap(),
    );
    let options = MdxSequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(2),
            fadeout: None,
        },
        max_ticks: None,
        ..Default::default()
    };
    let mut players = [
        MdxSequencer::new(package.clone(), 44_100, 256, options).unwrap(),
        MdxSequencer::new_with_factories(package, 44_100, 256, options, ChipFactoryRegistry::new())
            .unwrap(),
        MdxSequencer::from_mdx(MDX.to_vec(), None, 44_100, 256, options).unwrap(),
        MdxSequencer::from_mdx_with_factories(
            MDX.to_vec(),
            None,
            44_100,
            256,
            options,
            ChipFactoryRegistry::new(),
        )
        .unwrap(),
    ];
    let expected = render_mdx(&mut players[0]);
    assert!(expected.len() > once.len());
    for player in &mut players[1..] {
        assert_eq!(render_mdx(player), expected);
    }
}

#[test]
fn mdx_filter_write_mutes_ym2151_pan() {
    let original =
        render_mdx(&mut MdxSequencer::new(fm_package(), 48_000, 128, Default::default()).unwrap());
    assert!(
        original
            .iter()
            .flatten()
            .any(|sample| sample.abs() > 0.0001)
    );
    let mut player = MdxSequencer::new(fm_package(), 48_000, 128, Default::default()).unwrap();
    player.filter_write(|_, mut spec: Ym2151Spec, _| {
        if (0x20..=0x27).contains(&spec.register) {
            spec.value &= 0x3f;
        }
        spec
    });
    let frames = render_mdx(&mut player);
    assert_eq!(frames.len(), original.len());
    assert!(frames.iter().flatten().all(|sample| *sample == 0.0));
}

#[test]
fn events_and_gain_are_available_for_mdx() {
    let mut player = MdxSequencer::new(fm_package(), 48_000, 128, Default::default()).unwrap();
    assert!(matches!(
        player.track_state::<Ym2151State>(Instance::Secondary, 3_579_545.0),
        Err(chipstream::sequencer::MdxSequencerError::Playback(
            chipstream::sequencer::VgmSequencerError::ChipNotFound {
                chip: Chip::Ym2151,
                instance: Instance::Secondary
            }
        ))
    ));
    assert_eq!(player.track_state_with_voice_clocks::<Ym2151State>(), 1);
    player
        .track_state::<Ym2151State>(Instance::Primary, 3_579_545.0)
        .unwrap();
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let callback_events = std::rc::Rc::clone(&events);
    player.on_write(move |instance, spec: Ym2151Spec, sample, state_events| {
        if spec.register == 0x08 {
            callback_events
                .borrow_mut()
                .push((sample, instance, spec, state_events));
        }
    });
    let mut frames = Vec::new();
    while let Some(block) = player.render().unwrap() {
        frames.extend_from_slice(block.frames());
    }
    let events = events.borrow();
    assert!(
        events
            .iter()
            .any(|(_, _, spec, state)| spec.value & 0x78 != 0
                && state.as_ref().is_some_and(|s| !s.is_empty()))
    );
    assert!(
        events
            .iter()
            .all(|(_, instance, spec, _)| *instance == Instance::Primary && spec.register == 0x08)
    );
    let mut muted = MdxSequencer::new(fm_package(), 48_000, 128, Default::default()).unwrap();
    muted
        .set_chip_gain(Chip::Ym2151, Instance::Primary, 0.0)
        .unwrap();
    let muted = render_mdx(&mut muted);
    assert_eq!(muted.len(), frames.len());
    assert!(muted.iter().flatten().all(|sample| *sample == 0.0));
    assert!(frames.iter().flatten().any(|sample| sample.abs() > 0.0001));
}

#[test]
fn invalid_inputs_limits_and_playback_errors_are_reported() {
    assert!(
        MdxSequencer::from_mdx(b"bad mdx".to_vec(), None, 48_000, 128, Default::default()).is_err()
    );
    assert!(
        MdxSequencer::from_mdx(
            MDX.to_vec(),
            Some(b"bad pdx".to_vec()),
            48_000,
            128,
            Default::default()
        )
        .is_err()
    );
    assert!(MdxSequencer::new(fm_package(), 0, 128, Default::default()).is_err());
    assert!(MdxSequencer::new(fm_package(), 48_000, 0, Default::default()).is_err());
    let options = MdxSequencerOptions {
        ym2151_clock: 0,
        ..Default::default()
    };
    assert!(MdxSequencer::new(fm_package(), 48_000, 128, options).is_err());
    let options = MdxSequencerOptions {
        max_ticks: Some(1),
        ..Default::default()
    };
    let mut player = MdxSequencer::new(fm_package(), 48_000, 128, options).unwrap();
    let error = loop {
        match player.render() {
            Err(error) => break error,
            Ok(Some(_)) => {}
            Ok(None) => panic!("expected tick limit error"),
        }
    };
    let mut source: &(dyn Error + 'static) = &error;
    loop {
        if let Some(error) = source.downcast_ref::<MdxConvertError>() {
            assert_eq!(*error, MdxConvertError::TickLimitExceeded { max_ticks: 1 });
            break;
        }
        source = source.source().expect("typed MDX error in source chain");
    }
    assert!(player.render().is_err());
}

#[test]
#[cfg(feature = "okim6258")]
fn compressed_mdx_and_pdx_play_like_uncompressed_inputs() {
    let make_mdx = |compressed| {
        let mut builder = MdxBuilder::new();
        builder
            .set_lz_compressed(compressed)
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 16,
                },
            )
            .add_mdx_command(8, MdxRest { ticks: 8 });
        builder.finalize().unwrap().to_bytes().unwrap()
    };
    let make_pdx = |compressed| {
        let mut builder = PdxBuilder::new();
        builder.set_sample(0, 0, vec![0x71; 300]).unwrap();
        builder.set_lz_compressed(compressed);
        builder.finalize().to_bytes()
    };
    let plain_mdx = make_mdx(false);
    let plain_pdx = make_pdx(false);
    let compressed_mdx = make_mdx(true);
    let compressed_pdx = make_pdx(true);
    let expected = render_mdx(
        &mut MdxSequencer::from_mdx(plain_mdx, Some(plain_pdx), 48_000, 128, Default::default())
            .unwrap(),
    );
    assert!(
        expected
            .iter()
            .flatten()
            .any(|sample| sample.abs() > 0.0001)
    );
    let actual = render_mdx(
        &mut MdxSequencer::from_mdx(
            compressed_mdx,
            Some(compressed_pdx),
            48_000,
            257,
            Default::default(),
        )
        .unwrap(),
    );
    assert_eq!(actual, expected);
}

#[test]
fn owned_mdx_without_pdx_matches_parsed_package() {
    let package = fm_package();
    let mdx = package.to_mdx_bytes().unwrap();
    let actual = render_mdx(
        &mut MdxSequencer::from_mdx(mdx, None, 48_000, 128, Default::default()).unwrap(),
    );
    let expected =
        render_mdx(&mut MdxSequencer::new(package, 48_000, 128, Default::default()).unwrap());
    assert_eq!(actual, expected);
    assert!(actual.iter().flatten().any(|sample| sample.abs() > 0.0001));
}

#[test]
fn owned_mdx_normalizes_mxdrv16y_track_stubs() {
    let mut mdx = vec![0_u8; 286];
    mdx[0] = b'x';
    mdx[1..4].copy_from_slice(&[0x0d, 0x0a, 0x1a]);
    mdx[7..9].copy_from_slice(&20_u16.to_be_bytes());
    for track in 1..9 {
        let entry = 7 + track * 2;
        mdx[entry..entry + 2].copy_from_slice(&0xffff_u16.to_be_bytes());
    }
    mdx[25..28].copy_from_slice(&[0xf1, 0x01, 0x00]);
    mdx[284..286].copy_from_slice(&[0xf1, 0x00]);

    let normalized = soundlog::mdx::compat::normalize_mdx_for_playback(&mdx).unwrap();
    let package = MdxPackage::parse(&normalized, None).unwrap();
    let actual = render_mdx(
        &mut MdxSequencer::from_mdx(mdx, None, 48_000, 128, Default::default()).unwrap(),
    );
    let expected =
        render_mdx(&mut MdxSequencer::new(package, 48_000, 128, Default::default()).unwrap());
    assert_eq!(actual, expected);
}

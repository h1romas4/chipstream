use chipstream::sequencer::{
    MdxSequencer, MdxSequencerOptions, PlaybackOptions, VgmSequencer, VgmSequencerError,
    VgmSequencerOptions,
};
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::WaitSamples;
use std::time::Duration;

#[test]
fn effect_state_and_pan_restore_and_topology_changes_are_rejected() {
    use chipstream::audio::{
        AudioError, Gain, NativeEffect, StereoBalance, StereoBlock, StereoEffect,
    };
    use chipstream::chip::SampleRate;
    use chipstream::sequencer::{Chip, Instance};
    use soundlog::vgm::VgmDocument;

    #[derive(Clone)]
    struct Echo {
        previous: f32,
    }
    impl NativeEffect for Echo {
        fn process_native(&mut self, samples: &mut [f32], _: SampleRate) -> Result<(), AudioError> {
            for sample in samples {
                let current = *sample;
                *sample += self.previous * 0.25;
                self.previous = current;
            }
            Ok(())
        }
    }
    #[derive(Clone)]
    struct StereoEcho {
        previous: [f32; 2],
        scratch: Vec<[f32; 2]>,
    }
    impl StereoEffect for StereoEcho {
        fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError> {
            let mut previous = self.previous;
            block.try_transform(&mut self.scratch, |frames| {
                for frame in frames {
                    let current = *frame;
                    frame[0] += previous[0] * 0.25;
                    frame[1] += previous[1] * 0.25;
                    previous = current;
                }
                Ok(())
            })?;
            self.previous = previous;
            Ok(())
        }
    }

    let document =
        VgmDocument::try_from(&include_bytes!("../../soundlog/assets/vgm/ym2203_psg_a4.vgm")[..])
            .unwrap();
    let mut player = VgmSequencer::new(document, 48_000, 256, Default::default()).unwrap();
    let echo = player
        .add_output_effect(Chip::Ym2203, Instance::Primary, 1, Echo { previous: 0.0 })
        .unwrap();
    let group = player
        .create_output_group(Chip::Ym2203, Instance::Primary, &[1, 2, 3])
        .unwrap();
    let balance = player
        .add_group_effect(&group, StereoBalance::new(0.0).unwrap())
        .unwrap();
    let stereo_echo = player
        .add_group_effect(
            &group,
            StereoEcho {
                previous: [0.0; 2],
                scratch: Vec::new(),
            },
        )
        .unwrap();
    player
        .set_output_pan(Chip::Ym2203, Instance::Primary, 1, Some(-0.2))
        .unwrap();
    for _ in 0..4 {
        player.render().unwrap();
    }
    let checkpoint = player.checkpoint().unwrap();
    let previous = player.effect_mut(&echo).unwrap().previous;
    let stereo_previous = player.effect_mut(&stereo_echo).unwrap().previous;
    let expected = player.render().unwrap();
    player.effect_mut(&echo).unwrap().previous = 12345.0;
    player.effect_mut(&stereo_echo).unwrap().previous = [12.0, 34.0];
    player
        .effect_mut(&balance)
        .unwrap()
        .set_balance(1.0)
        .unwrap();
    player
        .set_output_pan(Chip::Ym2203, Instance::Primary, 1, Some(1.0))
        .unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.effect_mut(&echo).unwrap().previous, previous);
    assert_eq!(
        player.effect_mut(&stereo_echo).unwrap().previous,
        stereo_previous
    );
    assert_eq!(player.effect_mut(&balance).unwrap().balance(), 0.0);
    assert_eq!(player.render().unwrap(), expected);
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap(), expected);

    let extra = player
        .add_output_effect(Chip::Ym2203, Instance::Primary, 1, Gain::new(0.5).unwrap())
        .unwrap();
    let position = player.position_frames();
    assert!(matches!(
        player.restore_checkpoint(&checkpoint),
        Err(VgmSequencerError::CheckpointMismatch)
    ));
    assert_eq!(player.position_frames(), position);
    player.remove_effect(&extra).unwrap();
    assert!(player.effect_mut(&extra).is_err());
    assert!(matches!(
        player.restore_checkpoint(&checkpoint),
        Err(VgmSequencerError::StreamFailed)
    ));
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::StreamFailed)
    ));
}

#[test]
fn filter_state_is_caller_owned_and_seek_runs_filters() {
    use soundlog::chip::{Chip, Ym2151Spec};
    use soundlog::vgm::command::Instance;
    use std::{cell::Cell, rc::Rc};

    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.add_vgm_command(WaitSamples(256));
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x20,
            value: 0xc7,
        },
    );
    builder.add_vgm_command(WaitSamples(256));
    let mut player = VgmSequencer::new(builder.finalize(), 44_100, 64, Default::default()).unwrap();
    let calls = Rc::new(Cell::new(0));
    let observed = Rc::clone(&calls);
    player.filter_write(move |_, spec: Ym2151Spec, _| {
        observed.set(observed.get() + 1);
        spec
    });
    let checkpoint = player.checkpoint().unwrap();
    player.seek_to_frame(&checkpoint, 300).unwrap();
    assert_eq!(calls.get(), 1);
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(calls.get(), 1);
    player.seek_to_frame(&checkpoint, 300).unwrap();
    assert_eq!(calls.get(), 2);
}

#[test]
fn vgm_checkpoint_restores_parser_chip_and_audio_state() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(
        soundlog::chip::Chip::Ym2151,
        soundlog::vgm::command::Instance::Primary,
        4_000_000,
    );
    builder.add_vgm_command(WaitSamples(2_205));
    let bytes: Vec<u8> = builder.finalize().into();

    let mut player = VgmSequencer::from_vgm(
        bytes,
        48_000,
        128,
        VgmSequencerOptions {
            playback: PlaybackOptions {
                loop_count: Some(2),
                fadeout: Some(Duration::from_secs_f64(2_000.0 / 44_100.0)),
            },
            loop_modifier: Some(0x10),
            loop_base: Some(0),
        },
    )
    .unwrap();
    for _ in 0..3 {
        assert!(player.render().unwrap().is_some());
    }

    let checkpoint = player.checkpoint().unwrap();
    let checkpoint_sample = player.current_sample();
    let checkpoint_position = player.position_frames();
    let expected = player.render().unwrap();
    assert!(expected.is_some());
    let expected_sample = player.current_sample();
    let expected_position = player.position_frames();

    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.current_sample(), checkpoint_sample);
    assert_eq!(player.position_frames(), checkpoint_position);
    assert_eq!(player.render().unwrap(), expected);
    assert_eq!(player.current_sample(), expected_sample);
    assert_eq!(player.position_frames(), expected_position);
}

#[test]
fn checkpoint_restores_header_loop_settings() {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(400));
    builder.set_loop_index(0);
    let mut document = builder.finalize();
    document.header.loop_base = 0;
    document.header.loop_modifier = 0x20;

    for from_file in [false, true] {
        let options = VgmSequencerOptions {
            playback: PlaybackOptions {
                loop_count: Some(2),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut player = if from_file {
            VgmSequencer::from_vgm(Vec::<u8>::from(document.clone()), 44_100, 64, options).unwrap()
        } else {
            VgmSequencer::new(document.clone(), 44_100, 64, options).unwrap()
        };
        assert!(player.render().unwrap().is_some());
        let checkpoint = player.checkpoint().unwrap();
        let mut expected = Vec::new();
        while let Some(block) = player.render().unwrap() {
            expected.push(block);
        }
        player.restore_checkpoint(&checkpoint).unwrap();
        let mut actual = Vec::new();
        while let Some(block) = player.render().unwrap() {
            actual.push(block);
        }
        assert_eq!(actual, expected);
        assert!(actual.len() > 15);
    }
}

#[test]
fn vgm_dac_controls_survive_checkpoint_restore_and_seek() {
    use soundlog::chip::{Chip, Ym2612Spec};
    use soundlog::vgm::command::{
        DacStreamChipType, Instance, SetStreamData, SetStreamFrequency, SetupStreamControl,
        StartStreamFastCall, StartStreamFastCallFlags, StopStream,
    };
    use soundlog::vgm::detail::{StreamChipType, UncompressedStream};
    use soundlog::vgm::header::ChipId;

    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 1,
            register: 0xb6,
            value: 0xc0,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x2b,
            value: 0x80,
        },
    );
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Ym2612Pcm,
        data: vec![0x40, 0x80, 0xc0, 0x80],
    });
    builder.add_vgm_command(SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType::new(ChipId::Ym2612, Instance::Primary),
        write_port: 0,
        write_command: 0x2a,
    });
    builder.add_vgm_command(SetStreamData {
        stream_id: 0,
        data_bank_id: 0,
        step_size: 1,
        step_base: 0,
    });
    builder.add_vgm_command(SetStreamFrequency {
        stream_id: 0,
        frequency: 11_025,
    });
    let start = StartStreamFastCall {
        stream_id: 0,
        block_id: 0,
        flags: StartStreamFastCallFlags {
            reverse: false,
            looped: true,
        },
    };
    builder.add_vgm_command(start.clone());
    builder.add_vgm_command(WaitSamples(256));
    builder.add_vgm_command(StopStream { stream_id: 0xff });
    builder.add_vgm_command(WaitSamples(256));
    builder.add_vgm_command(start);
    builder.add_vgm_command(WaitSamples(1024));

    let document = builder.finalize();
    for from_file in [false, true] {
        let mut player = if from_file {
            VgmSequencer::from_vgm(
                Vec::<u8>::from(document.clone()),
                44_100,
                128,
                Default::default(),
            )
            .unwrap()
        } else {
            VgmSequencer::new(document.clone(), 44_100, 128, Default::default()).unwrap()
        };
        let first = player.render().unwrap().unwrap();
        assert!(first.frames().iter().flatten().any(|sample| *sample != 0.0));
        let checkpoint = player.checkpoint().unwrap();
        let expected = player.render().unwrap();
        assert!(player.render().unwrap().is_some());
        let block = player.render().unwrap().unwrap();
        let next = player.render().unwrap();

        player.restore_checkpoint(&checkpoint).unwrap();
        assert_eq!(player.render().unwrap(), expected);
        player.seek_to_frame(&checkpoint, 400).unwrap();
        assert_eq!(
            player.render().unwrap().unwrap().frames(),
            &block.frames()[16..]
        );
        assert_eq!(player.render().unwrap(), next);
    }
}

#[test]
fn checkpoint_cannot_be_restored_to_a_different_sequencer() {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(2_205));
    let bytes: Vec<u8> = builder.finalize().into();
    let mut first = VgmSequencer::from_vgm(bytes.clone(), 48_000, 128, Default::default()).unwrap();
    let checkpoint = first.checkpoint().unwrap();
    let mut second = VgmSequencer::from_vgm(bytes, 48_000, 128, Default::default()).unwrap();

    assert!(matches!(
        second.restore_checkpoint(&checkpoint),
        Err(VgmSequencerError::CheckpointMismatch)
    ));
}

#[test]
fn seek_to_frame_preserves_the_unconsumed_part_of_a_block() {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(2_205));
    let bytes: Vec<u8> = builder.finalize().into();
    let mut player = VgmSequencer::from_vgm(bytes, 48_000, 128, Default::default()).unwrap();
    assert!(player.render().unwrap().is_some());

    let checkpoint = player.checkpoint().unwrap();
    let checkpoint_position = player.position_frames();
    let expected_block = player.render().unwrap().unwrap();
    let next_expected = player.render().unwrap();
    let target = checkpoint_position + 53;
    let expected_tail = expected_block.frames()[53..].to_vec();

    player.seek_to_frame(&checkpoint, target).unwrap();
    assert_eq!(player.position_frames(), target);
    let actual_tail = player.render().unwrap().unwrap();
    assert_eq!(actual_tail.frames(), expected_tail);
    assert_eq!(player.render().unwrap(), next_expected);
}

#[test]
fn seek_to_frame_can_be_cancelled_between_render_blocks() {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(2_205));
    let bytes: Vec<u8> = builder.finalize().into();
    let mut player = VgmSequencer::from_vgm(bytes, 48_000, 128, Default::default()).unwrap();
    let checkpoint = player.checkpoint().unwrap();

    assert!(matches!(
        player.seek_to_frame_with_cancel(&checkpoint, 1_000, || true),
        Err(VgmSequencerError::SeekCancelled)
    ));
    assert_eq!(player.position_frames(), 0);
}

#[test]
fn mdx_checkpoint_restores_generator_and_audio_state() {
    let mdx = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");
    let package = MdxPackage::parse_owned(mdx.to_vec(), None).unwrap();
    let options = MdxSequencerOptions {
        playback: PlaybackOptions {
            loop_count: Some(2),
            fadeout: Some(Duration::from_secs_f64(2_000.0 / 44_100.0)),
        },
        ..MdxSequencerOptions::default()
    };
    let mut player = MdxSequencer::new(package, 48_000, 128, options).unwrap();
    for _ in 0..3 {
        assert!(player.render().unwrap().is_some());
    }

    let checkpoint = player.checkpoint().unwrap();
    let checkpoint_position = player.position_frames();
    let expected = player.render().unwrap();
    assert!(expected.is_some());
    let expected_position = player.position_frames();

    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.position_frames(), checkpoint_position);
    assert_eq!(player.render().unwrap(), expected);
    assert_eq!(player.position_frames(), expected_position);
}

#[test]
fn mdx_checkpoint_restores_fade_start_position() {
    use soundlog::mdx::command::{MdxCommand, MdxRelativeOffset, MdxRest};
    use soundlog::mdx::document::MdxBuilder;

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
            fadeout: Some(Duration::from_secs(2)),
        },
        ..Default::default()
    };
    let mut player = MdxSequencer::new(package, 44_100, 128, options).unwrap();
    assert_eq!(player.fade_start_frame(), None);
    let before_fade = player.checkpoint().unwrap();
    while player.fade_start_frame().is_none() {
        assert!(player.render().unwrap().is_some());
    }
    let start = player.fade_start_frame();
    let during_fade = player.checkpoint().unwrap();
    player.restore_checkpoint(&before_fade).unwrap();
    assert_eq!(player.fade_start_frame(), None);
    while player.fade_start_frame().is_none() {
        assert!(player.render().unwrap().is_some());
    }
    assert_eq!(player.fade_start_frame(), start);
    player.restore_checkpoint(&during_fade).unwrap();
    assert_eq!(player.fade_start_frame(), start);
}

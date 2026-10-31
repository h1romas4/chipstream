use chipstream::sequencer::{MdxSequencer, MdxSequencerOptions, VgmSequencer, VgmSequencerError};
use soundlog::mdx::package::MdxPackage;
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::WaitSamples;

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

    let mut player = VgmSequencer::from_vgm(bytes, 48_000, 128).unwrap();
    player.set_loop_count(Some(2));
    player.set_loop_modifier(0x10);
    player.set_loop_base(0);
    player.set_fadeout_samples(Some(2_000)).unwrap();
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
fn checkpoint_cannot_be_restored_to_a_different_sequencer() {
    let mut builder = VgmBuilder::new();
    builder.add_vgm_command(WaitSamples(2_205));
    let bytes: Vec<u8> = builder.finalize().into();
    let mut first = VgmSequencer::from_vgm(bytes.clone(), 48_000, 128).unwrap();
    let checkpoint = first.checkpoint().unwrap();
    let mut second = VgmSequencer::from_vgm(bytes, 48_000, 128).unwrap();

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
    let mut player = VgmSequencer::from_vgm(bytes, 48_000, 128).unwrap();
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
    let mut player = VgmSequencer::from_vgm(bytes, 48_000, 128).unwrap();
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
        loop_count: Some(2),
        ..MdxSequencerOptions::default()
    };
    let mut player =
        MdxSequencer::with_options_and_fadeout(package, 48_000, 128, options, Some(2_000)).unwrap();
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

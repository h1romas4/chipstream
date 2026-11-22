#![cfg(feature = "huc6280")]

use chipstream::chip::{ChipError, ChipRegisterWrite, Huc6280, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Huc6280State;
use soundlog::chip::{Chip, Huc6280Spec, PsgSpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Instance, WaitSamples};
use std::cell::RefCell;
use std::rc::Rc;

fn write(chip: &mut Huc6280, register: u8, value: u8) {
    chip.write(Huc6280Spec { register, value }).unwrap();
}

fn waveform(chip: &mut Huc6280, channel: u8, data: impl Fn(usize) -> u8) {
    write(chip, 0, channel);
    for i in 0..32 {
        write(chip, 6, data(i));
    }
}

#[test]
fn waveform_period_pan_and_dda_match_mame() {
    let mut chip = Huc6280::new(3_579_545).unwrap();
    assert_eq!(chip.sample_rate(), 3_579_545);
    waveform(&mut chip, 0, |i| if i == 0 { 31 } else { 0 });
    for (register, value) in [(1, 0xff), (2, 2), (5, 0xf0), (4, 0x9f)] {
        write(&mut chip, register, value);
    }
    let mut samples = [0; 6];
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [5115, 0, 5115, 0, -5456, 0]);
    write(&mut chip, 4, 0xdf);
    write(&mut chip, 6, 16);
    chip.generate(&mut samples[..2]).unwrap();
    assert_eq!(&samples[..2], &[0, 0]);
    write(&mut chip, 6, 31);
    chip.generate(&mut samples[..2]).unwrap();
    assert_eq!(&samples[..2], &[5115, 0]);
    write(&mut chip, 4, 0x9f);
    chip.generate(&mut samples[..2]).unwrap();
    assert_eq!(&samples[..2], &[5115, 0]);
}

#[test]
fn noise_lfo_and_snapshot_match_mame_counters() {
    let mut chip = Huc6280::new(3_579_545).unwrap();
    for (register, value) in [(0, 4), (1, 0xff), (5, 0xff), (7, 0x9f), (4, 0x9f)] {
        write(&mut chip, register, value);
    }
    let mut noise = [0; 6];
    chip.generate(&mut noise).unwrap();
    assert_eq!(noise, [5115, 5115, -5456, -5456, -5456, -5456]);
    let snapshot = chip.save_state().unwrap();
    let mut expected = [0; 100];
    chip.generate(&mut expected).unwrap();
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut actual = [0; 100];
    chip.generate(&mut actual).unwrap();
    assert_eq!(actual, expected);

    for (mode, period) in [(1, 17), (2, 62), (3, 242)] {
        let mut chip = Huc6280::new(3_579_545).unwrap();
        waveform(&mut chip, 1, |_| 31);
        write(&mut chip, 2, 2);
        waveform(&mut chip, 0, |i| if i & 1 == 0 { 31 } else { 0 });
        for (register, value) in [(1, 0xff), (5, 0xff), (2, 2), (8, 1), (9, mode), (4, 0x9f)] {
            write(&mut chip, register, value);
        }
        let mut frames = vec![0; (period + 3) * 2];
        chip.generate(&mut frames).unwrap();
        assert_eq!(&frames[..4], &[5115; 4]);
        assert!(
            frames[4..(period + 2) * 2]
                .iter()
                .all(|sample| *sample == -5456)
        );
        assert_eq!(&frames[(period + 2) * 2..], &[5115, 5115]);
    }
}

#[test]
fn errors_and_nonexistent_channels_are_isolated() {
    assert!(Huc6280::new(0).is_err());
    let mut chip = Huc6280::new(3_579_545).unwrap();
    for (register, value) in [(0, 6), (1, 0xff), (5, 0xff), (4, 0xdf), (6, 31)] {
        write(&mut chip, register, value);
    }
    let mut samples = [3; 4];
    chip.generate(&mut samples).unwrap();
    assert_eq!(samples, [0; 4]);
    let mut invalid = [42; 3];
    assert!(matches!(
        chip.generate(&mut invalid),
        Err(ChipError::InvalidBufferLength { .. })
    ));
    assert_eq!(invalid, [42; 3]);
    assert!(matches!(
        chip.apply_register_write(ChipRegisterWrite::Psg(PsgSpec { value: 0 })),
        Err(ChipError::ChipMismatch { .. })
    ));
    assert!(matches!(
        chip.restore_state(&42_u32),
        Err(ChipError::InvalidStateSnapshot(Chip::Huc6280))
    ));
}

#[test]
fn vgm_dac_stream_targets_dda_channel_and_restores_selection() {
    use soundlog::vgm::command::{
        DacStreamChipType, LengthMode, SetStreamData, SetStreamFrequency, SetupStreamControl,
        StartStream,
    };
    use soundlog::vgm::detail::{StreamChipType, UncompressedStream};
    use soundlog::vgm::header::ChipId;

    fn document(streamed: bool) -> soundlog::vgm::VgmDocument {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Huc6280, Instance::Primary, 3_579_545);
        for (register, value) in [
            (1, 0xff),
            (0, 0),
            (5, 0x0f),
            (4, 0xdf),
            (6, 16),
            (0, 1),
            (5, 0xf0),
            (4, 0xdf),
            (6, 16),
            (0, 0),
        ] {
            builder.add_chip_write(Instance::Primary, Huc6280Spec { register, value });
        }
        if streamed {
            builder.attach_data_block(UncompressedStream {
                chip_type: StreamChipType::Huc6280Pcm,
                data: vec![31],
            });
            builder.add_vgm_command(SetupStreamControl {
                stream_id: 0,
                chip_type: DacStreamChipType::new(ChipId::Huc6280, Instance::Primary),
                write_port: 1,
                write_command: 6,
            });
            builder.add_vgm_command(SetStreamData {
                stream_id: 0,
                data_bank_id: 5,
                step_size: 1,
                step_base: 0,
            });
            builder.add_vgm_command(SetStreamFrequency {
                stream_id: 0,
                frequency: 44_100,
            });
            builder.add_vgm_command(StartStream {
                stream_id: 0,
                data_start_offset: 0,
                length_mode: LengthMode::CommandCount {
                    reverse: false,
                    looped: false,
                },
                data_length: 1,
            });
        } else {
            for (register, value) in [(0, 1), (6, 31), (0, 0)] {
                builder.add_chip_write(Instance::Primary, Huc6280Spec { register, value });
            }
        }
        builder.add_vgm_command(WaitSamples(100));
        builder.add_chip_write(
            Instance::Primary,
            Huc6280Spec {
                register: 6,
                value: 31,
            },
        );
        builder.add_vgm_command(WaitSamples(100));
        builder.finalize()
    }

    let render = |document| {
        let mut player = VgmSequencer::new(document, 44_100, 64, Default::default()).unwrap();
        let mut frames = Vec::new();
        while let Some(block) = player.render().unwrap() {
            frames.extend_from_slice(block.frames());
        }
        frames
    };
    let actual = render(document(true));
    assert_eq!(actual, render(document(false)));
    let mut player = VgmSequencer::new(document(true), 44_100, 64, Default::default()).unwrap();
    player.render().unwrap();
    let checkpoint = player.checkpoint().unwrap();
    let next = player.render().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap(), next);
    assert!(
        actual[..90]
            .iter()
            .any(|frame| frame[0] > 0.1 && frame[1].abs() < 0.001)
    );
    assert!(
        actual[110..]
            .iter()
            .any(|frame| frame[0] > 0.1 && frame[1] > 0.1)
    );
}
#[test]
fn vgm_secondary_state_events_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Huc6280, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Huc6280, Instance::Secondary, 3_579_545);
    for (register, value) in [(0, 0), (1, 0xff), (5, 0xf0), (2, 200), (3, 0), (4, 0x9f)] {
        builder.add_chip_write(Instance::Secondary, Huc6280Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().into();
    let mut player = VgmSequencer::new(document, 44_100, 128, Default::default()).unwrap();
    player.track_state_with_voice_clocks::<Huc6280State>();
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&events);
    player.on_write(move |instance, spec: Huc6280Spec, _, changes| {
        received.borrow_mut().push((instance, spec, changes));
    });
    let first = player.render().unwrap().unwrap();
    let checkpoint = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&checkpoint).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    assert!(
        first
            .frames()
            .iter()
            .any(|frame| frame[0].abs() > 0.001 && frame[1] == 0.0)
    );
    assert!(
        events
            .borrow()
            .iter()
            .all(|(instance, _, _)| *instance == Instance::Secondary)
    );
    assert!(events.borrow().iter().any(|(_, _, changes)| matches!(
        changes.as_deref(),
        Some([StateEvent::KeyOn { channel: 0, .. }])
    )));
    let mut parsed = VgmSequencer::from_vgm(bytes, 44_100, 128, Default::default()).unwrap();
    assert_eq!(parsed.render().unwrap().unwrap(), first);
    assert_eq!(parsed.render().unwrap().unwrap(), second);
}

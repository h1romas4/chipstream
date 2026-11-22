#![cfg(feature = "mikey")]

use chipstream::chip::{Mikey, SoundChip};
use chipstream::sequencer::VgmSequencer;
use soundlog::chip::{Chip, MikeySpec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::{Instance, WaitSamples};

fn write(chip: &mut Mikey, register: u8, value: u8) {
    chip.write(MikeySpec { register, value }).unwrap();
}

#[test]
fn linked_channel_and_stereo_match_libvgm() {
    let mut chip = Mikey::new(16_000_000).unwrap();
    for (register, value) in [
        (0x20, 0x40),
        (0x21, 0x40),
        (0x23, 0),
        (0x24, 2),
        (0x26, 2),
        (0x25, 0x18),
        (0x28, 0x20),
        (0x29, 0x40),
        (0x2b, 0),
        (0x2c, 1),
        (0x2e, 1),
        (0x2d, 0x1f),
        (0x41, 0x2f),
        (0x44, 0x20),
    ] {
        write(&mut chip, register, value);
    }
    let mut samples = [0; 256];
    chip.generate(&mut samples).unwrap();
    for (frame, sample) in samples.as_chunks::<2>().0.iter().enumerate() {
        let expected = match frame {
            0..3 => [0, 0],
            3..7 => [3840, 3840],
            7..36 => [4096, 5760],
            36..69 => [-3584, -1920],
            69..73 => [4096, 5760],
            73..102 => [3584, 1920],
            _ => [-4096, -5760],
        };
        assert_eq!(*sample, expected, "frame {frame}");
    }
}

#[test]
fn vgm_mikey_playback_and_checkpoint() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Mikey, Instance::Primary, 16_000_000);
    for (register, value) in [
        (0x20, 0x40),
        (0x21, 0x40),
        (0x24, 2),
        (0x26, 2),
        (0x25, 0x18),
    ] {
        builder.add_chip_write(Instance::Primary, MikeySpec { register, value });
    }
    builder.add_vgm_command(WaitSamples(500));
    let doc = builder.finalize();
    let bytes: Vec<u8> = doc.clone().into();
    let mut player = VgmSequencer::new(doc, 44_100, 64, Default::default()).unwrap();
    let first = player.render().unwrap().unwrap();
    assert!(first.frames().iter().any(|frame| frame[0].abs() > 0.01));
    let snapshot = player.checkpoint().unwrap();
    let second = player.render().unwrap().unwrap();
    player.restore_checkpoint(&snapshot).unwrap();
    assert_eq!(player.render().unwrap().unwrap(), second);
    let mut parsed = VgmSequencer::from_vgm(bytes, 44_100, 64, Default::default()).unwrap();
    assert_eq!(parsed.render().unwrap().unwrap(), first);
}

#[test]
fn timer_wave_matches_libvgm() {
    let mut chip = Mikey::new(16_000_000).unwrap();
    for (register, value) in [
        (0x20, 0x40),
        (0x21, 0x40),
        (0x23, 0),
        (0x24, 2),
        (0x26, 2),
        (0x25, 0x18),
    ] {
        write(&mut chip, register, value);
    }
    let snapshot = chip.save_state().unwrap();
    let mut samples = [0; 256];
    chip.generate(&mut samples).unwrap();
    for (frame, sample) in samples.as_chunks::<2>().0.iter().enumerate() {
        let value = match frame {
            0..3 => 0,
            3..36 => 3840,
            36..69 => -3840,
            69..102 => 3840,
            _ => -3840,
        };
        assert_eq!(*sample, [value, value], "frame {frame}");
    }
    chip.restore_state(snapshot.as_ref()).unwrap();
    let mut split = [0; 256];
    chip.generate(&mut split[..66]).unwrap();
    chip.generate(&mut split[66..]).unwrap();
    assert_eq!(samples, split);
}

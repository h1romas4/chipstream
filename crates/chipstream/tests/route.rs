use std::error::Error;

use chipstream::audio::{ChipRoute, RouteError};
use chipstream::chip::{ChipError, ChipRegisterWrite, SoundChip, YmfmChip};
#[cfg(feature = "okim6258")]
use chipstream::chip::{ClockDivider, Okim6258, StereoMute};
use soundlog::chip::Chip;
#[cfg(feature = "okim6258")]
use soundlog::chip::Okim6258Spec;

#[test]
#[cfg(feature = "okim6258")]
fn captures_oki_rate_and_pan_changes_at_block_boundaries() {
    let mut route = ChipRoute::new(Okim6258::new(8_000_000).unwrap());
    route
        .chip_mut()
        .write(Okim6258Spec {
            register: 0,
            value: 2,
        })
        .unwrap();
    route
        .chip_mut()
        .write(Okim6258Spec {
            register: 1,
            value: 0x21,
        })
        .unwrap();
    let mut before = [0; 1];
    let block = route.capture(&mut before).unwrap();
    assert_eq!(block.chip_type(), &Chip::Okim6258);
    assert_eq!(block.sample_rate(), 7812);
    assert_eq!(block.sample_rate_ratio().numerator(), 8_000_000);
    assert_eq!(block.sample_rate_ratio().denominator().get(), 1024);
    assert_eq!(block.channels(), 1);
    assert_eq!(block.frames(), 1);
    assert_eq!(block.samples(), &[64]);
    assert_eq!(
        block.stereo_mute(),
        Some(StereoMute {
            left: false,
            right: false
        })
    );

    route.chip_mut().set_divider(ClockDivider::Div768).unwrap();
    route
        .chip_mut()
        .write(Okim6258Spec {
            register: 2,
            value: 1,
        })
        .unwrap();
    let mut after = [0; 1];
    let changed = route.capture(&mut after).unwrap();
    assert_eq!(changed.sample_rate(), 10416);
    assert_eq!(changed.sample_rate_ratio().numerator(), 8_000_000);
    assert_eq!(changed.sample_rate_ratio().denominator().get(), 768);
    assert_eq!(changed.samples(), &[224]);
    assert_eq!(
        changed.stereo_mute(),
        Some(StereoMute {
            left: true,
            right: false
        })
    );
    assert_eq!(block.sample_rate(), 7812);
    assert_eq!(block.samples(), &[64]);
    assert!(!block.stereo_mute().unwrap().left);
    assert_eq!(route.into_inner().pan_mask(), 1);
}

#[test]
fn preserves_multichannel_native_output_and_buffer_boundaries() {
    let chip = YmfmChip::new(Chip::Ym2203, 4_000_000).unwrap();
    let channels = chip.channels();
    assert_eq!(channels, 4);
    let mut route = ChipRoute::new(chip);
    let mut samples = vec![99; channels * 3 + 2];
    let block = route.capture(&mut samples[..channels * 3]).unwrap();
    assert_eq!(block.chip_type(), &Chip::Ym2203);
    assert_eq!(block.channels(), 4);
    assert_eq!(block.frames(), 3);
    assert_eq!(block.samples().len(), 12);
    assert_eq!(block.stereo_mute(), None);
    assert_eq!(&samples[channels * 3..], &[99, 99]);
    assert_eq!(route.chip().chip_type(), &Chip::Ym2203);
}

#[test]
#[cfg(feature = "okim6258")]
fn boxed_chip_and_empty_blocks_are_supported() {
    let chip: Box<dyn SoundChip> = Box::new(Okim6258::new(8_000_000).unwrap());
    let mut route = ChipRoute::new(chip);
    route
        .chip_mut()
        .write(Okim6258Spec {
            register: 2,
            value: 2,
        })
        .unwrap();
    let mut samples = [];
    let block = route.capture(&mut samples).unwrap();
    assert_eq!(block.frames(), 0);
    assert!(block.samples().is_empty());
    assert_eq!(
        block.stereo_mute(),
        Some(StereoMute {
            left: false,
            right: true,
        })
    );
}

struct TestChip {
    rate: u32,
    channels: usize,
    calls: usize,
    failure: bool,
}

impl SoundChip for TestChip {
    fn chip_type(&self) -> &Chip {
        &Chip::Ym2151
    }

    fn clock(&self) -> u32 {
        1
    }

    fn sample_rate(&self) -> u32 {
        self.rate
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn apply_register_write(&mut self, _: ChipRegisterWrite) -> Result<(), ChipError> {
        Ok(())
    }

    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        if self.failure {
            return Err(ChipError::BufferTooLarge {
                length: samples.len(),
            });
        }
        self.calls += 1;
        for (index, sample) in samples.iter_mut().enumerate() {
            *sample = index as i32;
        }
        Ok(())
    }

    fn reset(&mut self) {}
}

#[test]
fn validates_metadata_and_lengths_before_generation() {
    let mut route = ChipRoute::new(TestChip {
        rate: 0,
        channels: 0,
        calls: 0,
        failure: false,
    });
    let mut samples = [99; 5];
    assert!(matches!(
        route.capture(&mut samples),
        Err(RouteError::InvalidSampleRate)
    ));
    route.chip_mut().rate = 48_000;
    assert!(matches!(
        route.capture(&mut samples),
        Err(RouteError::InvalidChannelCount)
    ));
    route.chip_mut().channels = 3;
    assert!(matches!(
        route.capture(&mut samples),
        Err(RouteError::InvalidBufferLength {
            length: 5,
            channels: 3
        })
    ));
    assert_eq!(samples, [99; 5]);
    assert_eq!(route.chip().calls, 0);

    let mut valid = [0; 6];
    let block = route.capture(&mut valid).unwrap();
    assert_eq!(block.channels(), 3);
    assert_eq!(block.frames(), 2);
    assert_eq!(block.samples(), &[0, 1, 2, 3, 4, 5]);
    route.chip_mut().channels = 2;
    let block = route.capture(&mut valid).unwrap();
    assert_eq!(block.channels(), 2);
    assert_eq!(block.frames(), 3);
}

#[test]
fn propagates_chip_errors_with_source_and_unchanged_buffer() {
    let mut route = ChipRoute::new(TestChip {
        rate: 44_100,
        channels: 1,
        calls: 0,
        failure: true,
    });
    let mut samples = [99; 3];
    let error = route.capture(&mut samples).unwrap_err();
    assert_eq!(
        error,
        RouteError::Chip(ChipError::BufferTooLarge { length: 3 })
    );
    assert!(error.source().is_some());
    assert_eq!(samples, [99; 3]);
}

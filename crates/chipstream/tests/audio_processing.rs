use std::f32::consts::PI;
use std::num::NonZeroU32;

use chipstream::audio::{
    AudioError, ChipRoute, Gain, StereoBalance, StereoBlock, StereoEffect, StereoMatrix,
    StereoMixer, StereoResampler,
};
use chipstream::chip::{ClockDivider, Okim6258, SampleRate, SoundChip};

fn block(rate: u32, frames: Vec<[f32; 2]>) -> StereoBlock {
    StereoBlock::new(SampleRate::from_hz(rate), frames).unwrap()
}

fn convert_all(src: &StereoBlock, output: u32, chunk: usize) -> Vec<[f32; 2]> {
    let input_rate = src.sample_rate().floor_hz();
    let mut resampler = StereoResampler::new(output, input_rate, input_rate, chunk).unwrap();
    let mut output = resampler.process(src).unwrap().frames().to_vec();
    output.extend_from_slice(resampler.finish().unwrap().frames());
    output
}

#[test]
fn matrix_preserves_native_layout_and_applies_oki_pan() {
    let mut route = ChipRoute::new(Okim6258::new(8_000_000).unwrap());
    route.chip_mut().set_divider(ClockDivider::Div512).unwrap();
    route
        .chip_mut()
        .write(soundlog::chip::Okim6258Spec {
            register: 0,
            value: 2,
        })
        .unwrap();
    route
        .chip_mut()
        .write(soundlog::chip::Okim6258Spec {
            register: 1,
            value: 0x21,
        })
        .unwrap();
    route
        .chip_mut()
        .write(soundlog::chip::Okim6258Spec {
            register: 2,
            value: 1,
        })
        .unwrap();
    let mut raw = [0; 2];
    let native = route.capture(&mut raw).unwrap();
    let matrix = StereoMatrix::new(vec![[1.0 / 32768.0; 2]]).unwrap();
    let stereo = matrix.convert(&native).unwrap();
    assert_eq!(
        stereo.frames(),
        &[[0.0, 64.0 / 32768.0], [0.0, 224.0 / 32768.0]]
    );
    assert_eq!(stereo.sample_rate(), native.sample_rate_ratio());
    let wrong = StereoMatrix::new(vec![[1.0, 0.0], [0.0, 1.0]]).unwrap();
    assert!(matches!(
        wrong.convert(&native),
        Err(AudioError::ChannelMismatch {
            expected: 2,
            actual: 1
        })
    ));
    assert!(matches!(
        StereoMatrix::new(vec![]),
        Err(AudioError::InvalidMatrix)
    ));
    assert!(matches!(
        StereoMatrix::new(vec![[f32::NAN, 1.0]]),
        Err(AudioError::InvalidMatrix)
    ));
}

#[test]
fn mixer_effects_do_not_clip_and_reject_invalid_inputs() {
    let mut bus = block(48_000, vec![[0.8, -0.5], [0.2, 0.1]]);
    let input = block(48_000, vec![[0.8, 0.5], [0.4, -0.1]]);
    StereoMixer::mix(&mut bus, &input, 1.0).unwrap();
    assert_eq!(bus.frames(), &[[1.6, 0.0], [0.6, 0.0]]);
    Gain::new(0.5).unwrap().process(&mut bus).unwrap();
    StereoBalance::new(0.5).unwrap().process(&mut bus).unwrap();
    assert_eq!(bus.frames(), &[[0.4, 0.0], [0.15, 0.0]]);
    let before = bus.clone();
    assert!(matches!(
        StereoMixer::mix(&mut bus, &block(44_100, vec![[0.0; 2]; 2]), 1.0),
        Err(AudioError::RateMismatch { .. })
    ));
    assert!(matches!(
        StereoMixer::mix(&mut bus, &block(48_000, vec![[0.0; 2]; 1]), 1.0),
        Err(AudioError::FrameMismatch { .. })
    ));
    assert!(matches!(
        StereoMixer::mix(&mut bus, &input, f32::NAN),
        Err(AudioError::InvalidGain)
    ));
    assert_eq!(bus, before);
    assert!(matches!(Gain::new(-1.0), Err(AudioError::InvalidGain)));
    assert!(matches!(
        StereoBalance::new(1.1),
        Err(AudioError::InvalidBalance)
    ));
    assert!(matches!(
        StereoBlock::new(SampleRate::from_hz(1), vec![[f32::INFINITY, 0.0]]),
        Err(AudioError::NonFiniteSample)
    ));
}

#[test]
fn mixer_accepts_equivalent_fractional_rates_and_rejects_overflow() {
    let a = SampleRate::from_ratio(96_000, NonZeroU32::new(2).unwrap());
    let mut bus = StereoBlock::new(a, vec![[f32::MAX, 0.0]]).unwrap();
    let input = block(48_000, vec![[f32::MAX, 1.0]]);
    let before = bus.clone();
    assert!(matches!(
        StereoMixer::mix(&mut bus, &input, 1.0),
        Err(AudioError::NonFiniteSample)
    ));
    assert_eq!(bus, before);
    assert!(matches!(
        Gain::new(2.0).unwrap().process(&mut bus),
        Err(AudioError::NonFiniteSample)
    ));
    assert_eq!(bus, before);
    StereoMixer::mix(&mut bus, &input, 0.0).unwrap();
}

#[test]
fn equivalent_rates_at_u32_limit_work_without_cross_product_overflow() {
    let rate = SampleRate::from_ratio(u32::MAX, NonZeroU32::new(u32::MAX).unwrap());
    let mut bus = StereoBlock::new(rate, vec![[0.0; 2]; 300]).unwrap();
    let input = block(1, vec![[1.0; 2]; 300]);
    StereoMixer::mix(&mut bus, &input, 1.0).unwrap();
    let mut converter = StereoResampler::new(1, 1, 1, 64).unwrap();
    let count = converter.process(&bus).unwrap().frames().len()
        + converter.finish().unwrap().frames().len();
    assert_eq!(count, 300);
}

#[test]
fn sinc_conversion_is_continuous_across_input_chunks() {
    let frames: Vec<_> = (0..4096)
        .map(|i| {
            let value = (2.0 * PI * 1000.0 * i as f32 / 96_000.0).sin();
            [value, -value]
        })
        .collect();
    let input = block(96_000, frames.clone());
    let whole = convert_all(&input, 48_000, 256);
    let mut converter = StereoResampler::new(48_000, 96_000, 96_000, 256).unwrap();
    let mut pieces = Vec::new();
    for slice in frames.chunks(37) {
        pieces.extend_from_slice(
            converter
                .process(&block(96_000, slice.to_vec()))
                .unwrap()
                .frames(),
        );
    }
    pieces.extend_from_slice(converter.finish().unwrap().frames());
    assert_eq!(whole.len(), 2048);
    assert_eq!(whole.len(), pieces.len());
    for (index, (a, b)) in whole.iter().zip(pieces).enumerate() {
        assert!((a[0] - b[0]).abs() < 1e-5, "index {index}: {a:?} {b:?}");
        assert!((a[1] - b[1]).abs() < 1e-5);
    }
    assert!(matches!(
        converter.finish(),
        Err(AudioError::StreamFinished)
    ));
    converter.reset();
    assert!(
        converter
            .process(&block(96_000, vec![[0.0; 2]; 16]))
            .is_ok()
    );
}

fn sine(rate: u32, frequency: f32, frames: usize) -> StereoBlock {
    block(
        rate,
        (0..frames)
            .map(|i| {
                let sample = (2.0 * PI * frequency * i as f32 / rate as f32).sin();
                [sample, sample]
            })
            .collect(),
    )
}

fn rms(frames: &[[f32; 2]]) -> f32 {
    (frames.iter().map(|frame| frame[0] * frame[0]).sum::<f32>() / frames.len() as f32).sqrt()
}

#[test]
fn downsampling_attenuates_out_of_band_signal() {
    let pass = convert_all(&sine(96_000, 1000.0, 16_384), 32_000, 256);
    let reject = convert_all(&sine(96_000, 30_000.0, 16_384), 32_000, 256);
    let pass_rms = rms(&pass[256..pass.len() - 256]);
    let reject_rms = rms(&reject[256..reject.len() - 256]);
    assert!(pass_rms > 0.65 && pass_rms < 0.75, "{pass_rms}");
    assert!(reject_rms < 0.01, "{reject_rms}");
}

#[test]
fn finite_stream_preserves_final_samples_after_filter_drain() {
    let mut frames = vec![[0.0; 2]; 512];
    frames[511] = [1.0; 2];
    let output = convert_all(&block(48_000, frames), 48_000, 64);
    assert_eq!(output.len(), 512);
    assert!(output[480..].iter().any(|frame| frame[0].abs() > 0.1));
}

#[test]
fn fixed_point_duration_at_a_half_frame_boundary_is_within_one_frame() {
    let mut converter = StereoResampler::new(5, 6, 6, 8).unwrap();
    let input = block(6, vec![[1.0; 2]; 3]);
    let count = converter.process(&input).unwrap().frames().len()
        + converter.finish().unwrap().frames().len();
    assert!(count == 2 || count == 3, "{count}");
}

#[test]
fn rate_changes_preserve_history_and_duration() {
    let mut converter = StereoResampler::new(48_000, 24_000, 96_000, 128).unwrap();
    let a = converter
        .process(&block(96_000, vec![[1.0; 2]; 4096]))
        .unwrap();
    let b = converter
        .process(&block(24_000, vec![[1.0; 2]; 1024]))
        .unwrap();
    let tail = converter.finish().unwrap();
    assert_eq!(
        a.frames().len() + b.frames().len() + tail.frames().len(),
        4096
    );
    assert!(b.frames().iter().all(|frame| frame[0].is_finite()));
    assert!(
        b.frames()[300..]
            .iter()
            .all(|frame| (frame[0] - 1.0).abs() < 0.01)
    );
}

#[test]
fn rate_bounds_and_finished_stream_are_explicit_errors() {
    assert!(matches!(
        StereoResampler::new(0, 1, 2, 1),
        Err(AudioError::InvalidSampleRate)
    ));
    assert!(matches!(
        StereoResampler::new(48_000, 0, 2, 1),
        Err(AudioError::InvalidRateRange { .. })
    ));
    assert!(matches!(
        StereoResampler::new(48_000, 1, 2, 0),
        Err(AudioError::InvalidChunkSize)
    ));
    let mut converter = StereoResampler::new(48_000, 44_100, 96_000, 32).unwrap();
    assert!(matches!(
        converter.process(&block(24_000, vec![[0.0; 2]])),
        Err(AudioError::RateOutOfRange { .. })
    ));
    assert!(converter.finish().unwrap().frames().is_empty());
    assert!(matches!(
        converter.process(&block(48_000, vec![])),
        Err(AudioError::StreamFinished)
    ));
}

#[test]
fn short_and_rapidly_changing_streams_finish() {
    for (first, second) in [(96_000, 24_000), (24_000, 96_000)] {
        for length in [1, 16, 128, 512] {
            let mut converter = StereoResampler::new(48_000, 24_000, 96_000, 37).unwrap();
            let mut count = 0;
            for rate in [first, second, first, second] {
                count += converter
                    .process(&block(rate, vec![[0.5; 2]; length]))
                    .unwrap()
                    .frames()
                    .len();
            }

            count += converter.finish().unwrap().frames().len();
            let expected = (2.0 * length as f32 * 48_000.0 / first as f32
                + 2.0 * length as f32 * 48_000.0 / second as f32)
                .round() as usize;
            assert_eq!(count, expected, "{first} {second} {length}");
        }
    }
}

#[test]
fn large_rate_change_near_start_preserves_duration() {
    let mut converter = StereoResampler::new(48_000, 1_000, 192_000, 64).unwrap();
    let mut count = converter
        .process(&block(1_000, vec![[0.5; 2]]))
        .unwrap()
        .frames()
        .len();
    count += converter
        .process(&block(192_000, vec![[0.5; 2]; 1000]))
        .unwrap()
        .frames()
        .len();
    count += converter.finish().unwrap().frames().len();
    assert_eq!(count, 298);
}

#[test]
fn downsampling_to_upsampling_does_not_skip_the_new_rate() {
    let low = block(1_000, vec![[1.0; 2]; 512]);
    let convert = |prefix: bool| {
        let mut converter = StereoResampler::new(48_000, 1_000, 192_000, 64).unwrap();
        let mut output = Vec::new();
        if prefix {
            output.extend_from_slice(
                converter
                    .process(&block(192_000, vec![[0.0; 2]]))
                    .unwrap()
                    .frames(),
            );
        }
        output.extend_from_slice(converter.process(&low).unwrap().frames());
        output.extend_from_slice(converter.finish().unwrap().frames());
        output
    };
    let reference = convert(false);
    let changed = convert(true);
    assert_eq!(reference.len(), changed.len());
    for (index, (a, b)) in reference[1..200].iter().zip(&changed[1..200]).enumerate() {
        assert!((a[0] - b[0]).abs() < 0.02, "{index}: {a:?} {b:?}");
    }
}

#[test]
fn rate_changes_are_independent_of_input_block_partitioning() {
    let segments = [
        (96_000, vec![[0.25, -0.25]; 99]),
        (24_000, vec![[0.75, -0.75]; 201]),
        (72_000, vec![[-0.5, 0.5]; 277]),
    ];
    let convert = |chunk: usize| {
        let mut converter = StereoResampler::new(48_000, 24_000, 96_000, 64).unwrap();
        let mut result = Vec::new();
        for (rate, frames) in &segments {
            for part in frames.chunks(chunk) {
                result.extend_from_slice(
                    converter
                        .process(&block(*rate, part.to_vec()))
                        .unwrap()
                        .frames(),
                );
            }
        }
        result.extend_from_slice(converter.finish().unwrap().frames());
        result
    };
    let whole = convert(1000);
    let small = convert(7);
    assert_eq!(whole.len(), small.len());
    for (index, (a, b)) in whole.iter().zip(&small).enumerate() {
        assert!((a[0] - b[0]).abs() < 1e-5, "{index}: {a:?} {b:?}");
    }
}

#[test]
fn fractional_chip_rate_preserves_duration_and_reset_restarts_stream() {
    let rate = SampleRate::from_ratio(8_000_000, NonZeroU32::new(768).unwrap());
    let input = StereoBlock::new(rate, vec![[0.2; 2]; 4000]).unwrap();
    let mut converter = StereoResampler::new(48_000, 10_000, 11_000, 128).unwrap();
    let convert = |converter: &mut StereoResampler| {
        let mut frames = converter.process(&input).unwrap().frames().to_vec();
        frames.extend_from_slice(converter.finish().unwrap().frames());
        frames
    };
    let first = convert(&mut converter);
    assert_eq!(first.len(), 18_432);
    converter.reset();
    assert_eq!(first, convert(&mut converter));
}

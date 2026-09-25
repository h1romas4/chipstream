//! Software 8-channel PCM8/PCM8A mixer, ported from NanoDriveX's
//! `buildPcm8AdpcmByte()` (`okim6258.cpp`).
//!
//! MXDRV's PCM8/PCM8A mode drives a single physical OKIM6258 chip with a
//! pre-mixed mono ADPCM stream: up to 8 independently-resampled PCM/ADPCM
//! channels are summed in software, then re-encoded to 4-bit ADPCM. This
//! module reproduces that mixing + re-encoding step so it can be replayed
//! faithfully from a single OKIM6258 chip in VGM.
//!
//! The real hardware output stage is approximated with the NanoDriveX
//! 15,625 Hz `/512` 3rd-order low-pass and 183 Hz high-pass filters before
//! re-encoding.
//!
//! The mixer keeps one resampling state per logical PCM channel and emits a
//! single mono ADPCM byte stream. It does not decide when MDX notes begin or
//! end; those playback and scheduling decisions are made by `convert`.

use crate::mdx::pcm::AdpcmEncoder;

/// Fixed master output sample rate (Hz) of the re-encoded ADPCM stream.
///
/// This is the nominal "1.0x" rate used by `PCM8A_MODE_TABLE`'s F4/F5/F6
/// entries; every channel's own `rate_step` is relative to this fixed
/// master rate, exactly like NanoDriveX's `okim6258_mck_isr` driving
/// `buildPcm8AdpcmByte()` at a fixed hardware cadence while each channel
/// independently resamples its own source data against it.
pub const PCM8_MASTER_SAMPLE_RATE: u32 = 15_625;

/// VGM DAC-stream byte-write rate (Hz) for the re-encoded ADPCM stream.
///
/// Each ADPCM byte packs 2 samples, so this is `PCM8_MASTER_SAMPLE_RATE / 2`
/// rounded to the nearest Hz (15625 is odd, so this is a ~0.006% approximation).
pub const PCM8_STREAM_BYTE_RATE_HZ: u32 = PCM8_MASTER_SAMPLE_RATE.div_ceil(2);

/// VGM header `Okim6258Flags.clock_divider` selector for the OKIM6258's
/// `/512` clock divider (the fastest of the three hardware dividers),
/// needed so real hardware/emulators decode ADPCM nibbles at the same
/// `PCM8_MASTER_SAMPLE_RATE` this mixer assumes.
pub const PCM8_OKIM6258_CLOCK_DIVIDER: u8 = 2;

/// Recommended OKIM6258 master clock (Hz) for accurate PCM8 playback:
/// paired with the `/512` divider (`PCM8_OKIM6258_CLOCK_DIVIDER`), this
/// reproduces `PCM8_MASTER_SAMPLE_RATE` exactly. Leaving the VGM header's
/// clock divider at its default (`/1024`) or using a mismatched clock here
/// halves (or otherwise distorts) the real decode rate, which sounds like
/// noise even though the ADPCM data itself is correct.
pub const PCM8_RECOMMENDED_OKIM6258_CLOCK_HZ: u32 = PCM8_MASTER_SAMPLE_RATE * 512;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct PcmOutputFilter {
    enabled: bool,
    lpf_state1: f32,
    lpf_state2: f32,
    lpf_state3: f32,
    hpf_state: f32,
}

impl PcmOutputFilter {
    pub(crate) const fn new(enabled: bool) -> Self {
        Self {
            enabled,
            lpf_state1: 0.0,
            lpf_state2: 0.0,
            lpf_state3: 0.0,
            hpf_state: 0.0,
        }
    }

    // NanoDriveX's Pcm8LpfDiv512 coefficients for a 15,625 Hz OKIM6258 rate.
    const LPF_B0: f32 = 0.548918487;
    const LPF_B1: f32 = 0.564018086;
    const LPF_B2: f32 = 0.141551943;
    const LPF_B3: f32 = 0.011922191;
    const LPF_A1: f32 = 0.201042424;
    const LPF_A2: f32 = -0.019938263;
    const LPF_A3: f32 = 0.085306545;

    // NanoDriveX's Pcm8HpfDiv512 coefficients (183 Hz high-pass).
    const HPF_B0: f32 = 0.964495990;
    const HPF_A1: f32 = 0.928991979;

    fn process_sample(&mut self, sample: i16) -> i16 {
        if !self.enabled {
            return sample;
        }
        let input = f32::from(sample);
        let lpf_output = Self::LPF_B0 * input + self.lpf_state1;
        self.lpf_state1 = Self::LPF_B1 * input - Self::LPF_A1 * lpf_output + self.lpf_state2;
        self.lpf_state2 = Self::LPF_B2 * input - Self::LPF_A2 * lpf_output + self.lpf_state3;
        self.lpf_state3 = Self::LPF_B3 * input - Self::LPF_A3 * lpf_output;

        let scaled_input = Self::HPF_B0 * lpf_output;
        let hpf_output = scaled_input + self.hpf_state;
        self.hpf_state = -scaled_input + Self::HPF_A1 * hpf_output;

        let rounded = hpf_output + if hpf_output >= 0.0 { 0.5 } else { -0.5 };
        rounded.clamp(-2048.0, 2047.0) as i16
    }
}

/// Converts a raw MDX `@v` volume byte (`0x00..=0x0F` nibble form, or
/// `0x80..=0xFF` fine-grained form) into a PCM8 channel gain multiplier,
/// mirroring `getPcm8Gain()`. A gain of 16 is unity.
pub(crate) fn pcm8_gain(volume: u8) -> u8 {
    pcm8_gain_with_fadeout(volume, 0)
}

/// Converts an MDX PCM volume and global fadeout attenuation level into the
/// PCM8 mixer gain, following MXDRV's volume, fade-offset, and `PCMVolume`
/// lookup order. During an active fadeout, the table's zero-volume entries map
/// to digital silence so the mixer does not leave a quiet residual signal.
/// With no fadeout, PCM8 volume index 0 retains its normal minimum gain of 2.
pub(crate) fn pcm8_gain_with_fadeout(volume: u8, fadeout_level: u8) -> u8 {
    const VOLUME_TABLE: [u8; 16] = [
        0x2a, 0x28, 0x25, 0x22, 0x20, 0x1d, 0x1a, 0x18, 0x15, 0x12, 0x10, 0x0d, 0x0a, 0x08, 0x05,
        0x02,
    ];
    const PCM8_VOLUME_TABLE: [u8; 16] = [2, 3, 4, 5, 6, 8, 10, 12, 16, 20, 24, 32, 40, 48, 64, 80];
    const PCM8_VOLUME_BY_ATTENUATION: [u8; 43] = [
        15, 15, 15, 14, 14, 14, 13, 13, 13, 12, 12, 11, 11, 11, 10, 10, 10, 9, 9, 8, 8, 8, 7, 7, 7,
        6, 6, 5, 5, 5, 4, 4, 4, 3, 3, 2, 2, 2, 1, 1, 1, 0, 0,
    ];

    let attenuation = if volume & 0x80 != 0 {
        volume & 0x7f
    } else {
        VOLUME_TABLE[usize::from(volume & 0x0f)]
    };
    let attenuation = attenuation.saturating_add(fadeout_level);
    let pcm8_volume = PCM8_VOLUME_BY_ATTENUATION
        .get(usize::from(attenuation))
        .copied()
        .unwrap_or(0);
    if fadeout_level != 0 && pcm8_volume == 0 {
        0
    } else {
        PCM8_VOLUME_TABLE[usize::from(pcm8_volume)]
    }
}

/// One ADPCM channel's mixer state: a range in the shared decoded PCM arena
/// plus a Q16.16 playback position, mirroring `OKIM6258State`'s per-channel
/// `currentBlockId`/`posInBlock` and the track's own `pcmRateCounter`/
/// `pcmRateStep`.
#[derive(Debug, Clone, Default)]
pub(crate) struct PcmChannelState {
    pub block_start: usize,
    pub block_length: u32,
    pub block_key: Option<(usize, usize, u8)>,
    pub pos_in_block: u32,
    pub rate_counter: u32,
    pub rate_step: u32,
    pub gain: u8,
    /// Tie (`0xF7`) key-off: the note ends but the sample keeps playing to
    /// its own end instead of being cut immediately.
    pub hold: bool,
}

impl PcmChannelState {
    /// Advances this channel by one output sample slot and returns its
    /// linearly-interpolated, gain-weighted contribution to the mix (0 if
    /// stopped or silent). Mirrors one nibble's worth of the per-channel
    /// body of `buildPcm8AdpcmByte`.
    fn advance(&mut self, samples: &[i16]) -> i32 {
        if self.block_length == 0 {
            return 0;
        }
        let len = self.block_length;
        if self.pos_in_block >= len {
            self.block_length = 0;
            self.block_key = None;
            return 0;
        }

        let sample = {
            let s0 = i32::from(samples[self.block_start + self.pos_in_block as usize]);
            if self.rate_counter == 0 {
                s0
            } else {
                let next = self.pos_in_block + 1;
                if next >= len {
                    s0
                } else {
                    let s1 = i32::from(samples[self.block_start + next as usize]);
                    s0 + (((s1 - s0) * self.rate_counter as i32) >> 16)
                }
            }
        };
        let contribution = sample * i32::from(self.gain);

        let acc = self.rate_counter + self.rate_step;
        let advance = (acc >> 16) as u32;
        self.rate_counter = (acc & 0xFFFF) as u32;
        if advance != 0 {
            let new_pos = self.pos_in_block + advance;
            self.pos_in_block = new_pos.min(len);
            if self.pos_in_block >= len {
                self.block_length = 0;
                self.block_key = None;
            }
        }
        contribution
    }
}

/// Saturates a rounded mix accumulator down to the OKIM6258's 12-bit
/// signed sample range, mirroring the `(mix + 8) >> 4` bias-then-shift
/// trick used to avoid wraparound distortion when many channels sum.
fn clamp_to_12_bit(mix: i32) -> i16 {
    let biased = if mix < 0x7ff8 { mix + 8 } else { mix };
    (biased >> 4).clamp(-2048, 2047) as i16
}

/// Mixes one output byte's worth (2 samples) of all 8 ADPCM channels and
/// re-encodes the result into a single ADPCM byte via `encoder`. Channels
/// with no loaded block are skipped.
pub(crate) fn mix_and_encode_byte(
    channels: &mut [PcmChannelState; 8],
    samples: &[i16],
    encoder: &mut AdpcmEncoder,
    filter: &mut PcmOutputFilter,
) -> u8 {
    let mut mix0 = 0i32;
    let mut mix1 = 0i32;
    for channel in channels.iter_mut() {
        mix0 += channel.advance(samples);
        mix1 += channel.advance(samples);
    }
    let filtered0 = filter.process_sample(clamp_to_12_bit(mix0));
    let filtered1 = filter.process_sample(clamp_to_12_bit(mix1));
    encoder.encode_pair(filtered0, filtered1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm8_gain_maps_nibble_volume_via_vol_table() {
        assert_eq!(pcm8_gain(0), 2);
        assert_eq!(pcm8_gain(8), 16);
        assert_eq!(pcm8_gain(0x0f), 80);
    }

    #[test]
    fn pcm8_gain_maps_fine_volume_via_tl_table() {
        // 0x80 -> tl=0 -> level 15 -> highest gain in the table.
        assert_eq!(pcm8_gain(0x80), 80);
        // 0xff -> tl=0x7f, out of TL_TABLE range -> level 0 -> lowest gain.
        assert_eq!(pcm8_gain(0xff), 2);
    }

    #[test]
    fn pcm8_fadeout_adds_to_attenuation_before_volume_lookup() {
        assert_eq!(pcm8_gain_with_fadeout(8, 0), 16);
        assert_eq!(pcm8_gain_with_fadeout(8, 3), 12);
        assert_eq!(pcm8_gain_with_fadeout(0x80, 3), 64);
    }

    #[test]
    fn pcm8_fadeout_reaches_silence_at_zero_volume_table_entries() {
        assert_eq!(pcm8_gain_with_fadeout(8, 19), 3);
        assert_eq!(pcm8_gain_with_fadeout(8, 20), 0);
        assert_eq!(pcm8_gain_with_fadeout(8, 21), 0);
        assert_eq!(pcm8_gain_with_fadeout(0x80, 42), 0);
        assert_eq!(pcm8_gain_with_fadeout(0x80, 62), 0);
    }

    #[test]
    fn pcm8_without_fadeout_keeps_hardware_minimum_gain() {
        assert_eq!(pcm8_gain(0), 2);
        assert_eq!(pcm8_gain_with_fadeout(0, 0), 2);
    }

    #[test]
    fn pcm_output_filter_reduces_first_full_scale_sample() {
        let mut filter = PcmOutputFilter::new(true);

        let output = filter.process_sample(2047);

        assert!(output < 2047);
        assert!(output > 0);
    }

    #[test]
    fn silent_channel_contributes_nothing_and_does_not_advance() {
        let mut channel = PcmChannelState::default();
        assert_eq!(channel.advance(&[]), 0);
        assert_eq!(channel.pos_in_block, 0);
    }

    #[test]
    fn channel_advances_by_rate_step_and_interpolates() {
        let mut channel = PcmChannelState {
            block_start: 0,
            block_length: 4,
            block_key: None,
            pos_in_block: 0,
            rate_counter: 0,
            rate_step: 0x8000, // 0.5x: half a sample per output sample.
            gain: 16,          // unity gain.
            hold: false,
        };

        // First output sample: exactly on sample 0, no fraction yet.
        let samples = [0i16, 1000, 2000, 3000];
        assert_eq!(channel.advance(&samples), 0);
        assert_eq!(channel.pos_in_block, 0);
        assert_eq!(channel.rate_counter, 0x8000);

        // Second: halfway between sample 0 (0) and sample 1 (1000) -> ~500,
        // scaled by unity gain (16) then later normalized by the mixer's
        // final >>4, so the raw contribution here is sample * 16.
        let contribution = channel.advance(&samples);
        assert_eq!(contribution, 500 * 16);
        assert_eq!(channel.pos_in_block, 1);
    }

    #[test]
    fn channel_stops_when_it_reaches_the_end_of_its_block() {
        let mut channel = PcmChannelState {
            block_start: 0,
            block_length: 1,
            block_key: None,
            pos_in_block: 0,
            rate_counter: 0,
            rate_step: 0x10000, // 1.0x: one sample per output sample.
            gain: 16,
            hold: false,
        };

        let samples = [100i16];
        assert_eq!(channel.advance(&samples), 100 * 16);
        assert_eq!(channel.block_length, 0, "single-sample block must stop");
        assert_eq!(
            channel.advance(&samples),
            0,
            "a stopped channel stays silent"
        );
    }

    #[test]
    fn mix_and_encode_byte_skips_channels_with_no_loaded_block() {
        let mut channels: [PcmChannelState; 8] = Default::default();
        let mut encoder = AdpcmEncoder::default();
        // All channels silent -> mixed sample is 0 for both nibbles, so two
        // independent silent encoders must produce identical bytes.
        let byte = mix_and_encode_byte(
            &mut channels,
            &[],
            &mut encoder,
            &mut PcmOutputFilter::default(),
        );
        let mut reference_channels: [PcmChannelState; 8] = Default::default();
        let mut reference_encoder = AdpcmEncoder::default();
        let reference_byte = mix_and_encode_byte(
            &mut reference_channels,
            &[],
            &mut reference_encoder,
            &mut PcmOutputFilter::default(),
        );
        assert_eq!(byte, reference_byte);
    }

    #[test]
    fn mix_and_encode_byte_sums_multiple_active_channels() {
        let mut channels: [PcmChannelState; 8] = Default::default();
        let samples = [1000i16, 1000, -1000, -1000];
        channels[0] = PcmChannelState {
            block_start: 0,
            block_length: 2,
            block_key: None,
            pos_in_block: 0,
            rate_counter: 0,
            rate_step: 0x10000,
            gain: 16,
            hold: false,
        };
        channels[1] = PcmChannelState {
            block_start: 2,
            block_length: 2,
            block_key: None,
            pos_in_block: 0,
            rate_counter: 0,
            rate_step: 0x10000,
            gain: 16,
            hold: false,
        };
        let mut encoder = AdpcmEncoder::default();
        // The two channels cancel out, so the encoded byte should match an
        // encoder fed two all-silent channels.
        let byte = mix_and_encode_byte(
            &mut channels,
            &samples,
            &mut encoder,
            &mut PcmOutputFilter::default(),
        );
        let mut reference_channels: [PcmChannelState; 8] = Default::default();
        let mut reference_encoder = AdpcmEncoder::default();
        let reference_byte = mix_and_encode_byte(
            &mut reference_channels,
            &[],
            &mut reference_encoder,
            &mut PcmOutputFilter::default(),
        );
        assert_eq!(byte, reference_byte);
    }

    #[test]
    fn mix_and_encode_byte_combines_samples_from_different_blocks() {
        let samples = [1000i16, -1000, 500, 250];
        let mut channels: [PcmChannelState; 8] = Default::default();
        channels[0] = PcmChannelState {
            block_start: 0,
            block_length: 2,
            rate_step: 0x10000,
            gain: 16,
            ..Default::default()
        };
        channels[1] = PcmChannelState {
            block_start: 2,
            block_length: 2,
            rate_step: 0x10000,
            gain: 16,
            ..Default::default()
        };

        let mut encoder = AdpcmEncoder::default();
        let mixed_byte = mix_and_encode_byte(
            &mut channels,
            &samples,
            &mut encoder,
            &mut PcmOutputFilter::default(),
        );

        let combined_samples = [1500i16, -750];
        let mut reference_channels: [PcmChannelState; 8] = Default::default();
        reference_channels[0] = PcmChannelState {
            block_length: 2,
            rate_step: 0x10000,
            gain: 16,
            ..Default::default()
        };
        let mut reference_encoder = AdpcmEncoder::default();
        let reference_byte = mix_and_encode_byte(
            &mut reference_channels,
            &combined_samples,
            &mut reference_encoder,
            &mut PcmOutputFilter::default(),
        );

        assert_eq!(mixed_byte, reference_byte);
    }

    #[test]
    fn channels_can_share_one_decoded_block_range() {
        let samples = [1000i16, 1000];
        let mut channels: [PcmChannelState; 8] = Default::default();
        for channel in channels.iter_mut().take(2) {
            channel.block_start = 0;
            channel.block_length = 2;
            channel.rate_step = 0x10000;
            channel.gain = 16;
        }

        let mut encoder = AdpcmEncoder::default();
        let shared_byte = mix_and_encode_byte(
            &mut channels,
            &samples,
            &mut encoder,
            &mut PcmOutputFilter::default(),
        );

        let mut reference_channels: [PcmChannelState; 8] = Default::default();
        reference_channels[0].block_start = 0;
        reference_channels[0].block_length = 2;
        reference_channels[0].rate_step = 0x10000;
        reference_channels[0].gain = 32;
        let mut reference_encoder = AdpcmEncoder::default();
        let reference_byte = mix_and_encode_byte(
            &mut reference_channels,
            &samples,
            &mut reference_encoder,
            &mut PcmOutputFilter::default(),
        );

        assert_eq!(shared_byte, reference_byte);
    }
}

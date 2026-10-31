//! Based on MAME's OKIM6258 implementation, revision
//! `70743c6fb2602a5c2666c679b618706eabfca2ad`.
//! Copyright (c) Barry Rodewald.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.

use soundlog::chip::Chip;
use std::collections::VecDeque;
use std::num::NonZeroU32;

use super::oki_adpcm::{INDEX_SHIFT, difference_table};
use super::{ChipError, ChipRegisterWrite, SampleRate, SoundChip, StereoMute};

const COMMAND_STOP: u8 = 1 << 0;
const COMMAND_PLAY: u8 = 1 << 1;
const COMMAND_RECORD: u8 = 1 << 2;
const STATUS_PLAYING: u8 = 1 << 1;
const STATUS_RECORDING: u8 = 1 << 2;
const VGM_FILTER_RETENTION: i32 = 3;
const VGM_FILTER_DIVISOR: i32 = 4;
const VGM_UNDERRUN_RETENTION: i32 = 15;
const VGM_UNDERRUN_DIVISOR: i32 = 16;
const VGM_SIGNAL_SCALE: i32 = 256;

/// OKIM6258 clock divider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockDivider {
    Div1024,
    Div768,
    Div512,
}

impl ClockDivider {
    /// Number of input clocks per output sample.
    pub fn value(self) -> u32 {
        match self {
            Self::Div1024 => 1024,
            Self::Div768 => 768,
            Self::Div512 => 512,
        }
    }
}

/// DAC precision used to clamp the decoded ADPCM signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OutputBits {
    Bits10 = 10,
    Bits12 = 12,
}

/// OKIM6258 4-bit ADPCM decoder with mono native-rate output.
///
/// Direct instances use a one-byte data latch. VGM playback uses a byte queue,
/// smooths decoded samples, and fades toward silence when the queue runs dry.
/// Recording tracks status only. Output is mono `i32` in the signed 16-bit
/// range. The default clock divider is `/1024` and DAC precision is 10 bits.
/// VGM 3-bit selection flags use the 4-bit compatibility decoder; native
/// 3-bit ADPCM decoding is not implemented.
///
/// Register 0 is control, 1 is data, 2 is the VGM pan mask, `0x08..=0x0b`
/// are little-endian clock bytes, and `0x0c` selects the divider. Pan is
/// exposed for a stereo mixer rather than applied to mono output. Clock and
/// divider writes can change the native rate; an update producing zero rate is
/// rejected without changing configuration. Reset stops decoding but retains
/// input data, configuration, and pan mask.
#[derive(Clone)]
pub struct Okim6258 {
    clock: u32,
    divider: ClockDivider,
    output_bits: OutputBits,
    status: u8,
    pan_mask: u8,
    vgm_streaming: bool,
    vgm_data: VecDeque<u8>,
    vgm_byte_loaded: bool,
    vgm_signal_q8: i32,
    data_in: u8,
    nibble_shift: u8,
    signal: i32,
    step: usize,
    diff_lookup: [[i32; 16]; 49],
}

impl Okim6258 {
    /// Create a chip with the `/1024` divider and 10-bit DAC precision.
    pub fn new(clock: u32) -> Result<Self, ChipError> {
        let divider = ClockDivider::Div1024;
        validate_clock(clock, divider)?;
        Ok(Self {
            clock,
            divider,
            output_bits: OutputBits::Bits10,
            status: 0,
            pan_mask: 0,
            vgm_streaming: false,
            vgm_data: VecDeque::new(),
            vgm_byte_loaded: false,
            vgm_signal_q8: 0,
            data_in: 0,
            nibble_shift: 0,
            signal: -2,
            step: 0,
            diff_lookup: difference_table(),
        })
    }

    pub(crate) fn new_for_vgm(clock: u32) -> Result<Self, ChipError> {
        let mut chip = Self::new(clock)?;
        chip.vgm_streaming = true;
        Ok(chip)
    }

    /// Change the input clock without resetting the decoder.
    pub fn set_clock(&mut self, clock: u32) -> Result<(), ChipError> {
        validate_clock(clock, self.divider)?;
        self.clock = clock;
        Ok(())
    }

    /// Change the sample-rate divider without resetting the decoder.
    pub fn set_divider(&mut self, divider: ClockDivider) -> Result<(), ChipError> {
        validate_clock(self.clock, divider)?;
        self.divider = divider;
        Ok(())
    }

    /// Current sample-rate divider.
    pub fn divider(&self) -> ClockDivider {
        self.divider
    }

    /// Change the DAC precision without resetting the decoder.
    pub fn set_output_bits(&mut self, output_bits: OutputBits) {
        self.output_bits = output_bits;
    }

    /// Current DAC precision.
    pub fn output_bits(&self) -> OutputBits {
        self.output_bits
    }

    /// Return `0x00` while playing, or `0x80` while stopped.
    pub fn status(&self) -> u8 {
        if self.status & STATUS_PLAYING != 0 {
            0x00
        } else {
            0x80
        }
    }

    /// VGM pan mask: bit 0 mutes left, and bit 1 mutes right.
    ///
    /// The chip itself generates mono; a caller mixing to stereo must apply
    /// this mask. The value survives [`SoundChip::reset`].
    pub fn pan_mask(&self) -> u8 {
        self.pan_mask
    }

    fn write_control(&mut self, data: u8) {
        if data & COMMAND_STOP != 0 {
            self.status &= !(STATUS_PLAYING | STATUS_RECORDING);
            return;
        }
        if data & COMMAND_PLAY != 0 {
            if self.status & STATUS_PLAYING == 0 {
                self.status |= STATUS_PLAYING;
                self.signal = -2;
                self.step = 0;
                self.nibble_shift = 0;
                self.vgm_signal_q8 = 0;
            }
        } else {
            self.status &= !STATUS_PLAYING;
        }
        if data & COMMAND_RECORD != 0 {
            self.status |= STATUS_RECORDING;
        } else {
            self.status &= !STATUS_RECORDING;
        }
    }

    fn clock_adpcm(&mut self, nibble: u8) -> i32 {
        let limit = 1_i32 << (self.output_bits as u8 - 1);
        self.signal = (self.signal + self.diff_lookup[self.step][usize::from(nibble)])
            .clamp(-limit, limit - 1);
        self.step = (self.step as i32 + INDEX_SHIFT[usize::from(nibble & 7)]).clamp(0, 48) as usize;
        self.signal << 4
    }

    fn filter_vgm_sample(&mut self, sample: i32, retention: i32, divisor: i32) -> i32 {
        // The 12-bit DAC bounds the Q8 signal to 2^23 and retention to 15.
        self.vgm_signal_q8 = (self.vgm_signal_q8 * retention
            + sample * VGM_SIGNAL_SCALE * (divisor - retention))
            / divisor;
        let limit = 1_i32 << (self.output_bits as u8 - 1);
        self.vgm_signal_q8 = self.vgm_signal_q8.clamp(
            -limit * 16 * VGM_SIGNAL_SCALE,
            (limit - 1) * 16 * VGM_SIGNAL_SCALE,
        );
        self.vgm_signal_q8 / VGM_SIGNAL_SCALE
    }

    fn clock_vgm_stream(&mut self) -> i32 {
        if !self.vgm_byte_loaded
            && self.nibble_shift == 0
            && let Some(data) = self.vgm_data.pop_front()
        {
            self.data_in = data;
            self.vgm_byte_loaded = true;
        }

        if self.vgm_byte_loaded {
            let nibble = (self.data_in >> self.nibble_shift) & 0x0f;
            let sample = self.clock_adpcm(nibble);
            self.nibble_shift ^= 4;
            if self.nibble_shift == 0 {
                self.vgm_byte_loaded = false;
            }
            self.filter_vgm_sample(sample, VGM_FILTER_RETENTION, VGM_FILTER_DIVISOR)
        } else {
            self.nibble_shift = 0;
            self.filter_vgm_sample(0, VGM_UNDERRUN_RETENTION, VGM_UNDERRUN_DIVISOR)
        }
    }
}

impl SoundChip for Okim6258 {
    fn chip_type(&self) -> &Chip {
        &Chip::Okim6258
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.clock / self.divider.value()
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(
            self.clock,
            NonZeroU32::new(self.divider.value()).expect("clock dividers are nonzero"),
        )
    }

    fn channels(&self) -> usize {
        1
    }

    fn stereo_mute(&self) -> Option<StereoMute> {
        Some(StereoMute {
            left: self.pan_mask & 1 != 0,
            right: self.pan_mask & 2 != 0,
        })
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let (bank, register, value) = write.byte_parts_for(self.chip_type())?;
        if bank != 0 {
            return Err(ChipError::InvalidRegisterBank {
                chip: Chip::Okim6258,
                bank,
            });
        }
        match register {
            0x00 => self.write_control(value),
            0x01 => {
                if self.vgm_streaming {
                    self.vgm_data.push_back(value);
                } else {
                    self.data_in = value;
                    self.nibble_shift = 0;
                }
            }
            0x02 => self.pan_mask = value & 3,
            0x08..=0x0b => {
                let shift = u32::from(register - 0x08) * 8;
                self.set_clock((self.clock & !(0xff << shift)) | (u32::from(value) << shift))?;
            }
            0x0c => {
                let divider = match value & 3 {
                    0 => ClockDivider::Div1024,
                    1 => ClockDivider::Div768,
                    _ => ClockDivider::Div512,
                };
                self.set_divider(divider)?;
            }
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: Chip::Okim6258,
                    register,
                });
            }
        }
        Ok(())
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if self.status & STATUS_PLAYING == 0 {
            buffer.fill(0);
            return Ok(());
        }
        if self.vgm_streaming {
            for sample in buffer {
                *sample = self.clock_vgm_stream();
            }
        } else {
            for sample in buffer {
                let nibble = (self.data_in >> self.nibble_shift) & 0x0f;
                *sample = self.clock_adpcm(nibble);
                self.nibble_shift ^= 4;
            }
        }
        Ok(())
    }

    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, super::ChipError> {
        Ok(super::save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), super::ChipError> {
        let chip = self.chip_type().clone();
        super::restore_cloned_state(self, state, chip)
    }

    fn reset(&mut self) {
        self.signal = -2;
        self.step = 0;
        self.status = 0;
        self.vgm_signal_q8 = 0;
    }
}

fn validate_clock(clock: u32, divider: ClockDivider) -> Result<(), ChipError> {
    let minimum = divider.value();
    if clock < minimum {
        Err(ChipError::InvalidClock { clock, minimum })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::chip::Okim6258Spec;

    fn write(chip: &mut Okim6258, register: u8, value: u8) {
        chip.apply_register_write(ChipRegisterWrite::Okim6258(Okim6258Spec {
            register,
            value,
        }))
        .unwrap();
    }

    #[test]
    fn vgm_filter_matches_i64_reference_at_bounds_and_precision_changes() {
        for bits in [OutputBits::Bits10, OutputBits::Bits12] {
            let limit = 1_i64 << (bits as u8 - 1);
            for initial in [-8_388_608_i32, -1, 0, 1, 8_384_512] {
                for sample in [-32_768_i32, -16, 0, 16, 32_752] {
                    for (retention, divisor) in [
                        (VGM_FILTER_RETENTION, VGM_FILTER_DIVISOR),
                        (VGM_UNDERRUN_RETENTION, VGM_UNDERRUN_DIVISOR),
                    ] {
                        let mut chip = Okim6258::new_for_vgm(8_000_000).unwrap();
                        chip.set_output_bits(bits);
                        chip.vgm_signal_q8 = initial;
                        let expected = ((i64::from(initial) * i64::from(retention)
                            + i64::from(sample)
                                * i64::from(VGM_SIGNAL_SCALE)
                                * i64::from(divisor - retention))
                            / i64::from(divisor))
                        .clamp(-limit * 16 * 256, (limit - 1) * 16 * 256);
                        let output = chip.filter_vgm_sample(sample, retention, divisor);
                        assert_eq!(i64::from(chip.vgm_signal_q8), expected);
                        assert_eq!(i64::from(output), expected / 256);
                    }
                }
            }
        }
    }

    #[test]
    fn vgm_filter_matches_i64_reference_over_long_stream() {
        let mut chip = Okim6258::new_for_vgm(8_000_000).unwrap();
        let mut reference = 0_i64;
        let mut random = 0x4a32_f19b_u32;
        for index in 0..50_000 {
            random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let bits = if (index / 113) % 2 == 0 {
                OutputBits::Bits12
            } else {
                OutputBits::Bits10
            };
            chip.set_output_bits(bits);
            let limit = 1_i32 << (bits as u8 - 1);
            let (sample, retention, divisor) = if index % 7 == 0 {
                (0, VGM_UNDERRUN_RETENTION, VGM_UNDERRUN_DIVISOR)
            } else {
                (
                    ((random % (2 * limit) as u32) as i32 - limit) * 16,
                    VGM_FILTER_RETENTION,
                    VGM_FILTER_DIVISOR,
                )
            };
            reference = ((reference * i64::from(retention)
                + i64::from(sample) * 256 * i64::from(divisor - retention))
                / i64::from(divisor))
            .clamp(
                -i64::from(limit) * 16 * 256,
                i64::from(limit - 1) * 16 * 256,
            );
            assert_eq!(
                i64::from(chip.filter_vgm_sample(sample, retention, divisor)),
                reference / 256,
                "sample {index}"
            );
            assert_eq!(i64::from(chip.vgm_signal_q8), reference, "sample {index}");
        }
    }

    #[test]
    fn vgm_stream_consumes_queued_bytes_in_order() {
        let mut queued = Okim6258::new_for_vgm(8_000_000).unwrap();
        write(&mut queued, 0, COMMAND_PLAY);
        write(&mut queued, 1, 0x21);
        write(&mut queued, 1, 0x43);
        let mut queued_output = [0; 4];
        queued.generate(&mut queued_output).unwrap();

        let mut paced = Okim6258::new_for_vgm(8_000_000).unwrap();
        write(&mut paced, 0, COMMAND_PLAY);
        write(&mut paced, 1, 0x21);
        let mut first_paced_output = [0; 2];
        paced.generate(&mut first_paced_output).unwrap();
        write(&mut paced, 1, 0x43);
        let mut second_paced_output = [0; 2];
        paced.generate(&mut second_paced_output).unwrap();

        assert_eq!(
            queued_output,
            [
                first_paced_output[0],
                first_paced_output[1],
                second_paced_output[0],
                second_paced_output[1]
            ]
        );
    }

    #[test]
    fn vgm_stream_smooths_samples_without_attenuating_sustained_levels() {
        let mut vgm = Okim6258::new_for_vgm(8_000_000).unwrap();
        let mut direct = Okim6258::new(8_000_000).unwrap();
        write(&mut vgm, 0, COMMAND_PLAY);
        write(&mut direct, 0, COMMAND_PLAY);
        write(&mut direct, 1, 0x11);
        for _ in 0..64 {
            write(&mut vgm, 1, 0x11);
        }

        let mut averaged = [0; 128];
        let mut accumulated = [0; 128];
        vgm.generate(&mut averaged).unwrap();
        direct.generate(&mut accumulated).unwrap();

        assert!(averaged[3] < accumulated[3]);
        assert!(averaged[127] > 4_000);
        assert!(accumulated[127] > 4_000);
    }

    #[test]
    fn vgm_stream_fades_when_input_runs_dry() {
        let mut chip = Okim6258::new_for_vgm(8_000_000).unwrap();
        write(&mut chip, 0, COMMAND_PLAY);
        write(&mut chip, 1, 0x77);
        let mut active = [0; 2];
        chip.generate(&mut active).unwrap();
        assert!(active.iter().any(|sample| *sample != 0));

        let mut tail = [0; 128];
        chip.generate(&mut tail).unwrap();
        assert_eq!(tail[127], 0);
        assert!(tail.windows(2).all(|pair| pair[1].abs() <= pair[0].abs()));
    }
}

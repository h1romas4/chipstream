//! Based on MAME's OKIM6258 implementation, revision
//! `70743c6fb2602a5c2666c679b618706eabfca2ad`.
//! Copyright (c) Barry Rodewald.
//! See `THIRD_PARTY_NOTICES.md` for the license terms.
//!
//! The 12-bit predictor and DAC output precision are modeled separately;
//! unlike the referenced MAME revision, 10-bit output does not clamp the
//! predictor to 10 bits.

use soundlog::chip::Chip;
use std::num::NonZeroU32;

use super::{ChipError, ChipRegisterWrite, Okim6258Adpcm, SampleRate, SoundChip, StereoMute};

const COMMAND_STOP: u8 = 1 << 0;
const COMMAND_PLAY: u8 = 1 << 1;
const COMMAND_RECORD: u8 = 1 << 2;
const STATUS_PLAYING: u8 = 1 << 1;
const STATUS_RECORDING: u8 = 1 << 2;

/// OKIM6258 clock divider.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockDivider {
    /// Divide the input clock by 1024.
    Div1024,
    /// Divide the input clock by 768.
    Div768,
    /// Divide the input clock by 512.
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

/// DAC precision applied to the decoded 12-bit ADPCM signal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum OutputBits {
    /// Output the upper 10 bits of the 12-bit decoded signal.
    Bits10 = 10,
    /// Output the full 12-bit decoded signal.
    Bits12 = 12,
}

/// OKIM6258 4-bit ADPCM decoder with mono native-rate output.
///
/// Data writes replace a one-byte latch and restart decoding from its low
/// nibble. Playback alternates its low and high nibbles until the next write.
/// Recording tracks status only. Output is mono `i32` in the signed 16-bit
/// range. The predictor uses 12-bit precision regardless of DAC precision;
/// the 10-bit DAC discards the two least significant bits. The default clock
/// divider is `/1024` and DAC precision is 10 bits.
/// VGM 3-bit selection flags use the 4-bit compatibility decoder; native
/// 3-bit ADPCM decoding is not implemented.
///
/// Register 0 is control, 1 is data, 2 is the VGM pan mask, `0x08..=0x0b`
/// are little-endian clock bytes, and `0x0c` selects the divider. Writes to
/// `0x10..=0x17` are ignored for VGMMAME compatibility. Pan is
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
    data_in: u8,
    nibble_shift: u8,
    adpcm: Okim6258Adpcm,
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
            data_in: 0,
            nibble_shift: 0,
            adpcm: Okim6258Adpcm::new(),
        })
    }

    /// Create an OKIM6258 backend for VGM.
    ///
    /// This is equivalent to [`Self::new`].
    pub fn new_for_vgm(clock: u32) -> Result<Self, ChipError> {
        Self::new(clock)
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

    /// Apply playback and recording control bits.
    fn write_control(&mut self, data: u8) {
        if data & COMMAND_STOP != 0 {
            self.status &= !(STATUS_PLAYING | STATUS_RECORDING);
            return;
        }
        if data & COMMAND_PLAY != 0 {
            if self.status & STATUS_PLAYING == 0 {
                self.status |= STATUS_PLAYING;
                self.adpcm.reset();
                self.nibble_shift = 0;
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

    /// Decode one ADPCM nibble and advance the predictor.
    fn clock_adpcm(&mut self, nibble: u8) -> i32 {
        let signal = self.adpcm.decode_low_nibble(nibble);
        let output = match self.output_bits {
            OutputBits::Bits10 => signal & !3,
            OutputBits::Bits12 => signal,
        };
        output << 4
    }
}

impl SoundChip for Okim6258 {
    /// Report the emulated chip type.
    fn chip_type(&self) -> &Chip {
        &Chip::Okim6258
    }

    /// Return the input clock in hertz.
    fn clock(&self) -> u32 {
        self.clock
    }

    /// Return the native sample rate in hertz.
    fn sample_rate(&self) -> u32 {
        self.clock / self.divider.value()
    }

    /// Return the exact native sample-rate ratio.
    fn sample_rate_ratio(&self) -> SampleRate {
        SampleRate::from_ratio(
            self.clock,
            NonZeroU32::new(self.divider.value()).expect("clock dividers are nonzero"),
        )
    }

    /// Return the mono channel count.
    fn channels(&self) -> usize {
        1
    }

    /// Report which stereo outputs are enabled by the pan register.
    fn stereo_mute(&self) -> Option<StereoMute> {
        Some(StereoMute {
            left: self.pan_mask & 1 != 0,
            right: self.pan_mask & 2 != 0,
        })
    }

    /// Apply an OKIM6258 register write and update its decoder state.
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
                self.data_in = value;
                self.nibble_shift = 0;
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
            0x10..=0x17 => {}
            _ => {
                return Err(ChipError::InvalidRegister {
                    chip: Chip::Okim6258,
                    register,
                });
            }
        }
        Ok(())
    }

    /// Decode samples into the caller's mono buffer.
    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        if self.status & STATUS_PLAYING == 0 {
            buffer.fill(0);
            return Ok(());
        }
        for sample in buffer {
            let nibble = (self.data_in >> self.nibble_shift) & 0x0f;
            *sample = self.clock_adpcm(nibble);
            self.nibble_shift ^= 4;
        }
        Ok(())
    }

    /// Capture a clone of the chip state.
    fn save_state(&mut self) -> Result<Box<dyn super::SoundChipState>, super::ChipError> {
        Ok(super::save_cloned_state(self))
    }

    /// Restore a state snapshot after validating its concrete type.
    fn restore_state(&mut self, state: &dyn super::SoundChipState) -> Result<(), super::ChipError> {
        let chip = self.chip_type().clone();
        super::restore_cloned_state(self, state, chip)
    }

    /// Reset the decoder and stop playback.
    fn reset(&mut self) {
        self.adpcm.reset();
        self.status = 0;
    }
}

/// Ensure the clock is high enough for the selected divider.
fn validate_clock(clock: u32, divider: ClockDivider) -> Result<(), ChipError> {
    let minimum = divider.value();
    if clock < minimum {
        Err(ChipError::InvalidClock { clock, minimum })
    } else {
        Ok(())
    }
}

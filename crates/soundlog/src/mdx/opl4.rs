//! YMF278B (OPL4) PCM output for MDX conversion and standalone PDX playback.
//!
//! # MDX output
//!
//! Select [`super::convert::PcmOutput::Opl4`] with
//! [`super::convert::MdxToVgmOptions::pcm_output`] to convert MDX PCM tracks to
//! independent OPL4 slots. The default remains
//! [`super::convert::PcmOutput::Okim6258`]; FM continues to use YM2151.
//!
//! OPL4 output supports legacy ADPCM and PCM8A F0-F12 formats/rates, up to eight
//! PCM tracks, gate/key-off, ties, live rate/volume/pan changes, embedded
//! fadeout, finite repeats and native VGM loops. Decoded 12-bit samples are
//! stored once per source range/format/scaling in type `0x84` ROM blocks, with
//! per-slot `0xD0` register writes instead of a flattened PCM stream.
//!
//! Eager conversion moves ROM blocks to the front; lazy conversion emits them
//! before their first use. Register timestamps and ROM contents agree. Lazy
//! streams do not provide a VGM header; register [`CLOCK_HZ`] (33,868,800 Hz)
//! with the playback device. A finite conversion ends with a 256-sample release
//! wait; native loops do not stop the PCM chip at the loop boundary.
//!
//! # Processing and limits
//!
//! OPL4 requires [`super::convert::AdpcmMode::Through`].
//! [`super::convert::AdpcmMode::Resample`] and
//! [`super::convert::AdpcmMode::Lpf`] are OKIM6258-only and are rejected rather
//! than silently ignored. OPL4 still decodes legacy ADPCM in `Through` mode.
//! It bypasses the mono software mixer, ADPCM re-encoding, and analog-output
//! filter. Per-track stereo is retained. PCM8 gains are mapped to OPL4's
//! 0.375 dB total-level steps relative to the PCM8 maximum gain (80); this
//! provides headroom but changes PCM/FM balance compared with the OKIM6258 mixer.
//!
//! MDX output is limited to 65,532 decoded samples per wave, 512 decoded sample
//! variants, and 4 MiB of ROM (including the reserved wave-header region).
//! Exceeding these limits reports an error; long-sample segmentation is not
//! implemented. The existing decoded-PCM, tick, per-tick command and
//! eager-output limits also apply. Missing/malformed PCM follows the existing
//! MDX silence policy. Original ADPCM bytes and PDX bank/note reconstruction
//! metadata are not embedded; wave headers retain converted sample boundaries.
//!
//! # Standalone playback
//!
//! [`pdx_sample_to_vgm`] and [`pdx_voices_to_vgm`] generate single samples or
//! chords without MDX sequencing, using the same PCM packing and pitch mapping.

use crate::ParseError;
use crate::chip::{Chip, Ymf278bSpec};
use crate::mdx::convert::MdxConvertError;
use crate::mdx::pcm::{PCM8A_MODE_TABLE, Pcm8aFormat, decode_pcm8a_with_pcm16_15khz};
use crate::mdx::pcm_mixer::PCM8_MASTER_SAMPLE_RATE;
use crate::mdx::pdx::PdxDocument;
use crate::vgm::command::{Instance, WaitSamples};
use crate::vgm::detail::{RomRamChipType, RomRamDump};
use crate::vgm::{VGM_SAMPLE_RATE, VgmBuilder, VgmDocument};

/// Standard OPL4 master clock used by MDX conversion and the experiments.
pub const CLOCK_HZ: u32 = 33_868_800;
const HEADER_SIZE: usize = 12;
pub(super) const MAX_SAMPLE_COUNT: usize = 65_532;
const LOAD_WAIT: u16 = 14;
const RELEASE_WAIT: u16 = 256;

/// One independently controlled voice in the experimental OPL4 chord generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opl4Voice {
    /// Zero-based PDX bank.
    pub bank: usize,
    /// Zero-based PDX note, not an MDX note byte.
    pub note: usize,
    /// PCM8A F0-F12 selector (`0..=12`), selecting both format and sample rate.
    /// F5 uses the existing PCM8A signed 12-bit-in-16-bit-container scaling.
    pub mode: u8,
    /// Native OPL4 attenuation (`0..=127`), in 0.375 dB steps.
    /// This is not the MDX volume byte. Leave headroom when mixing many voices.
    pub total_level: u8,
    /// Native signed OPL4 pan: -7=left, 0=center, 7=right, -8=mute.
    pub pan: i8,
}

impl Default for Opl4Voice {
    fn default() -> Self {
        Self {
            bank: 0,
            note: 0,
            mode: 4,
            total_level: 0,
            pan: 0,
        }
    }
}

/// Builds a standalone VGM playing one PDX entry as ADPCM decoded to 12-bit PCM.
///
/// `bank` and `note` are zero-based PDX table coordinates, not MDX note bytes.
/// The entry is always interpreted as OKIM6258 ADPCM; PCM8A PCM8/PCM16 input is
/// not supported by this PoC. No source sample values are lost during packing.
///
/// Playback uses slot/wave 0, centered pan, constant volume, and the standard
/// 33.8688 MHz clock. Octave -1 and F-number 427 approximate the MDX F4 rate
/// (15,625 Hz) at 15,622.3388671875 Hz (about -0.017%). Duration is calculated
/// from that actual rate. Register writes are separated by one VGM sample,
/// with an additional 14-sample header-load wait before key-on.
///
/// A two-sample silent loop prevents the waveform from repeating before the
/// explicit mute/key-off. The ROM contains a 12-byte wave header followed by
/// the packed source samples and three bytes of silence. It does not retain
/// the original ADPCM bytes or PDX bank/note metadata. After key-off, a
/// 256-sample (about 5.8 ms) wait lets the release envelope settle.
///
/// # Errors
///
/// Returns [`ParseError::DataInconsistency`] for missing/empty/invalid entries
/// or entries exceeding 32,766 ADPCM bytes (65,532 decoded samples). Longer
/// samples require segmentation, which is outside this PoC.
pub fn pdx_sample_to_vgm(
    pdx: &PdxDocument,
    bank: usize,
    note: usize,
) -> Result<VgmDocument, ParseError> {
    pdx_voices_to_vgm(
        pdx,
        &[Opl4Voice {
            bank,
            note,
            ..Opl4Voice::default()
        }],
    )
}

/// Builds a standalone VGM playing one to eight PCM8A voices concurrently.
///
/// Each slice entry controls a separate OPL4 slot, including when several
/// voices select the same sample. Identical source ranges with the same
/// decoding semantics share one ROM wave; rate, volume and pan remain per-slot.
/// ADPCM, PCM8 and PCM16 use the same decoders/scaling as MDX playback.
///
/// All decoded samples are retained as packed 12-bit PCM. Odd sample counts
/// are zero-padded for packing, without extending the source playback duration.
/// Every wave ends in a silent loop, allowing different source lengths/rates
/// to finish independently. All slots are muted/released after the last source
/// finishes. No software mixing or ADPCM re-encoding is performed.
///
/// The standard 33.8688 MHz clock is used. Each F0-F12 rate is approximated
/// by the nearest OPL4 octave/F-number (less than 0.03% nominal rate error);
/// duration uses the actual hardware step. Register writes take one VGM sample
/// each, so key-ons are staggered
/// by one sample (at most seven samples, about 0.159 ms, across eight voices).
/// Levels are native OPL4 attenuation, not PCM8A gain; no automatic mixer
/// normalization or clipping prevention is applied.
///
/// This is not an MDX sequencer: gates, ties, live controls, repeats and
/// reconstruction metadata are outside this experiment.
///
/// # Errors
///
/// Returns [`ParseError::DataInconsistency`] for an empty or oversized voice
/// list, invalid controls, missing samples, malformed PCM16 data, or samples
/// longer than 65,532 decoded values. Length limits are checked before decoding.
pub fn pdx_voices_to_vgm(
    pdx: &PdxDocument,
    voices: &[Opl4Voice],
) -> Result<VgmDocument, ParseError> {
    if voices.is_empty() || voices.len() > 8 {
        return Err(invalid("expected one to eight voices"));
    }
    let mut waves: Vec<Wave> = Vec::new();
    let mut slots = Vec::with_capacity(voices.len());
    for voice in voices {
        let Some(&(rate, format)) = PCM8A_MODE_TABLE.get(usize::from(voice.mode)) else {
            return Err(invalid("PCM8A mode must be in 0..=12"));
        };
        if voice.total_level > 127 || !(-8..=7).contains(&voice.pan) {
            return Err(invalid("total level must be in 0..=127 and pan in -8..=7"));
        }
        let bytes = pdx
            .sample_bytes(voice.bank, voice.note)
            .filter(|bytes| !bytes.is_empty())
            .ok_or_else(|| {
                invalid(format!(
                    "missing or invalid PDX sample at bank {}, note {}",
                    voice.bank, voice.note
                ))
            })?;
        let sample_count = match format {
            Pcm8aFormat::Adpcm => bytes.len().checked_mul(2),
            Pcm8aFormat::Pcm8 => Some(bytes.len()),
            Pcm8aFormat::Pcm16 if bytes.len() % 2 == 0 => Some(bytes.len() / 2),
            Pcm8aFormat::Pcm16 => return Err(invalid("PCM16 sample has an odd byte count")),
        }.filter(|&count| count <= MAX_SAMPLE_COUNT).ok_or_else(|| invalid(format!(
            "PDX sample at bank {}, note {} exceeds the maximum of {MAX_SAMPLE_COUNT} decoded samples (maximum is 32766 ADPCM bytes)",
            voice.bank, voice.note
        )))?;
        let entry = pdx
            .entry(voice.bank, voice.note)
            .ok_or_else(|| invalid("invalid PDX entry"))?;
        let pcm16_is_15khz = voice.mode == 5;
        let key = (entry.start, entry.size, format, pcm16_is_15khz);
        let wave = if let Some(index) = waves.iter().position(|wave| wave.key == key) {
            index
        } else {
            let samples = decode_pcm8a_with_pcm16_15khz(format, bytes, pcm16_is_15khz)
                .map_err(|error| invalid(format!("PCM decode failed: {error:?}")))?;
            waves.push(Wave { key, samples });
            waves.len() - 1
        };
        let (octave, fnumber, step) = pitch(rate);
        slots.push((
            wave as u8,
            octave,
            fnumber,
            (sample_count as u32 * 65536).div_ceil(step),
        ));
    }

    let mut rom = vec![0; waves.len() * HEADER_SIZE];
    for (index, wave) in waves.iter().enumerate() {
        let start = rom.len() as u32;
        rom[index * HEADER_SIZE..(index + 1) * HEADER_SIZE]
            .copy_from_slice(&wave_header(start, wave.samples.len()));
        pack_pcm12(&wave.samples, &mut rom);
        rom.extend_from_slice(&[0; 3]);
    }
    let mut builder = VgmBuilder::new();
    builder.set_sample_rate(VGM_SAMPLE_RATE);
    builder.register_chip(Chip::Ymf278b, Instance::Primary, CLOCK_HZ);
    builder.attach_data_block(RomRamDump {
        chip_type: RomRamChipType::Ymf278bRom,
        rom_size: rom.len() as u32,
        start_address: 0,
        data: rom,
    });
    write(&mut builder, 1, 0x05, 0x03); // Enable NEW and NEW2 before PCM writes.
    write(&mut builder, 2, 0x02, 0);
    write(&mut builder, 2, 0xf8, 0x3f); // Mute FM in the mixed output.
    write(&mut builder, 2, 0xf9, 0);
    for (slot, (voice, &(wave, octave, fnumber, _))) in voices.iter().zip(&slots).enumerate() {
        let slot = slot as u8;
        write(&mut builder, 2, 0x20 + slot, (fnumber as u8) << 1);
        write(&mut builder, 2, 0x08 + slot, wave);
        builder.add_vgm_command(WaitSamples(LOAD_WAIT));
        write(
            &mut builder,
            2,
            0x38 + slot,
            (octave << 4) | (fnumber >> 7) as u8,
        );
        write(&mut builder, 2, 0x50 + slot, (voice.total_level << 1) | 1);
    }
    let mut finish = 0;
    for (slot, voice) in voices.iter().enumerate() {
        write(
            &mut builder,
            2,
            0x68 + slot as u8,
            0x80 | (voice.pan as u8 & 0x0f),
        );
        finish = finish.max(slot as u32 + 1 + slots[slot].3);
    }
    let mut remaining = finish - voices.len() as u32;
    while remaining > 0 {
        let wait = remaining.min(u32::from(u16::MAX)) as u16;
        builder.add_vgm_command(WaitSamples(wait));
        remaining -= u32::from(wait);
    }
    for slot in 0..voices.len() as u8 {
        write(&mut builder, 2, 0x50 + slot, 0xff);
        write(&mut builder, 2, 0x68 + slot, 0);
    }
    builder.add_vgm_command(WaitSamples(RELEASE_WAIT));
    Ok(builder.finalize())
}

struct Wave {
    key: (u32, u32, Pcm8aFormat, bool),
    samples: Vec<i16>,
}

fn invalid(message: impl std::fmt::Display) -> ParseError {
    ParseError::DataInconsistency(format!("OPL4 PoC: {message}"))
}

fn pitch(rate: u32) -> (u8, u16, u32) {
    let target_step =
        f64::from(rate) * f64::from(PCM8_MASTER_SAMPLE_RATE) / f64::from(VGM_SAMPLE_RATE);
    let mut best = (0, 0, 0);
    let mut error = f64::INFINITY;
    for octave in -7i8..=7 {
        let scale = 2f64.powi(i32::from(octave) + 5);
        let fnumber = (target_step / scale - 1024.0).round().clamp(0.0, 1023.0) as u16;
        let step = ((1024 + u32::from(fnumber)) << (octave + 7)) >> 2;
        let candidate_error = (f64::from(step) - target_step).abs();
        if candidate_error < error {
            best = (octave as u8 & 0x0f, fnumber, step);
            error = candidate_error;
        }
    }
    best
}

fn write(builder: &mut VgmBuilder, port: u8, register: u8, value: u8) {
    builder.add_chip_write(
        Instance::Primary,
        Ymf278bSpec {
            port,
            register,
            value,
        },
    );
    builder.add_vgm_command(WaitSamples(1));
}

fn pack_pcm12(samples: &[i16], output: &mut Vec<u8>) {
    for pair in samples.chunks(2) {
        let first = pair[0] as u16 & 0x0fff;
        let second = pair.get(1).copied().unwrap_or(0) as u16 & 0x0fff;
        output.extend_from_slice(&[
            (first >> 4) as u8,
            ((first & 0x0f) | ((second & 0x0f) << 4)) as u8,
            (second >> 4) as u8,
        ]);
    }
}

fn wave_header(start: u32, sample_count: usize) -> [u8; HEADER_SIZE] {
    let count = (sample_count as u16 + 1) & !1;
    let end = 0u16.wrapping_sub(count + 2).to_be_bytes();
    [
        0x40 | (start >> 16) as u8,
        (start >> 8) as u8,
        start as u8,
        (count >> 8) as u8,
        count as u8,
        end[0],
        end[1],
        0,
        0xf0,
        0,
        0xff,
        0,
    ]
}

/// ROM is populated incrementally, before the first use of each decoded range.
#[derive(Clone)]
pub(super) struct Opl4Output {
    waves: std::collections::HashMap<(usize, usize), u16>,
    next_address: u32,
    pub(super) steps: [u32; 8],
    wave_high: [u8; 8],
    pan: [u8; 8],
    gain: [u8; 8],
    active: [bool; 8],
    pending: Vec<crate::vgm::command::VgmCommand>,
}

impl Opl4Output {
    pub(super) fn new() -> Self {
        Self {
            waves: Default::default(),
            next_address: 0x2000,
            steps: [0; 8],
            wave_high: [0; 8],
            pan: [0; 8],
            gain: [16; 8],
            active: [false; 8],
            pending: Vec::new(),
        }
    }

    pub(super) fn initialize(&mut self) {
        self.write(1, 0x05, 0x03);
        self.write(2, 0x02, 0);
        self.write(2, 0xf8, 0x3f);
        self.write(2, 0xf9, 0);
        for channel in 0..8 {
            self.stop(channel);
        }
    }

    pub(super) fn start(
        &mut self,
        channel: usize,
        range: (usize, usize),
        samples: &[i16],
        rate: u32,
        gain: u8,
    ) -> Result<(), MdxConvertError> {
        let wave = if let Some(&wave) = self.waves.get(&range) {
            wave
        } else {
            if range.1 == 0 || range.1 > MAX_SAMPLE_COUNT {
                return Err(MdxConvertError::InvalidDocument(format!(
                    "OPL4 sample has {} decoded values; maximum is {MAX_SAMPLE_COUNT}; segmentation is not supported",
                    range.1
                )));
            }
            if self.waves.len() >= 512 {
                return Err(MdxConvertError::InvalidDocument(
                    "OPL4 wave table exceeds 512 decoded sample variants".into(),
                ));
            }
            let mut data = Vec::with_capacity(range.1.div_ceil(2) * 3 + 3);
            pack_pcm12(&samples[range.0..range.0 + range.1], &mut data);
            data.extend_from_slice(&[0; 3]);
            let end_address = self.next_address + data.len() as u32;
            if end_address > 0x40_0000 {
                return Err(MdxConvertError::InvalidDocument(
                    "OPL4 decoded PCM ROM exceeds 4 MiB".into(),
                ));
            }
            let wave = self.waves.len() as u16;
            let start = self.next_address;
            self.rom(u32::from(wave) * 12, wave_header(start, range.1).to_vec());
            self.rom(start, data);
            self.next_address = end_address;
            self.waves.insert(range, wave);
            wave
        };
        self.stop(channel);
        let (octave, fnumber, step) = pitch(rate);
        self.steps[channel] = step;
        self.wave_high[channel] = (wave >> 8) as u8;
        self.write(
            2,
            0x20 + channel as u8,
            ((fnumber as u8) << 1) | (wave >> 8) as u8,
        );
        self.write(2, 0x08 + channel as u8, wave as u8);
        self.write(
            2,
            0x38 + channel as u8,
            (octave << 4) | (fnumber >> 7) as u8,
        );
        self.set_gain(channel, gain);
        self.active[channel] = true;
        self.emit_key(channel);
        Ok(())
    }

    pub(super) fn stop(&mut self, channel: usize) {
        self.active[channel] = false;
        self.write(2, 0x50 + channel as u8, 0xff);
        self.emit_key(channel);
    }

    pub(super) fn set_gain(&mut self, channel: usize, gain: u8) {
        self.gain[channel] = gain;
        // Normalize PCM8's maximum gain (80) to the OPL4's maximum level.
        let level = if gain == 0 {
            127
        } else {
            (-20.0 * (f64::from(gain) / 80.0).log10() / 0.375)
                .round()
                .clamp(0.0, 127.0) as u8
        };
        self.write(2, 0x50 + channel as u8, (level << 1) | 1);
        if self.active[channel] {
            self.emit_key(channel);
        }
    }

    pub(super) fn set_pan(&mut self, channel: usize, raw: u8) {
        self.pan[channel] = match raw & 3 {
            0 => 8,
            1 => 9,
            2 => 7,
            _ => 0,
        };
        self.emit_key(channel);
    }

    pub(super) fn set_rate(&mut self, channel: usize, rate: u32) {
        let (octave, fnumber, step) = pitch(rate);
        self.steps[channel] = step;
        // Preserve the currently selected wave's ninth index bit.
        let wave_high = self.wave_high[channel];
        self.write(2, 0x20 + channel as u8, ((fnumber as u8) << 1) | wave_high);
        self.write(
            2,
            0x38 + channel as u8,
            (octave << 4) | (fnumber >> 7) as u8,
        );
    }

    fn emit_key(&mut self, channel: usize) {
        let pan = if !self.active[channel] || self.gain[channel] == 0 {
            8
        } else {
            self.pan[channel]
        };
        self.write(
            2,
            0x68 + channel as u8,
            (u8::from(self.active[channel]) << 7) | pan,
        );
    }

    fn write(&mut self, port: u8, register: u8, value: u8) {
        self.pending.push(
            (
                Instance::Primary,
                Ymf278bSpec {
                    port,
                    register,
                    value,
                },
            )
                .into(),
        );
    }

    fn rom(&mut self, start_address: u32, data: Vec<u8>) {
        let block = crate::vgm::detail::build_data_block(
            &RomRamDump {
                chip_type: RomRamChipType::Ymf278bRom,
                rom_size: 0x40_0000,
                start_address,
                data,
            }
            .into(),
        );
        self.pending
            .push(crate::vgm::command::VgmCommand::DataBlock(Box::new(block)));
    }

    pub(super) fn flush(&mut self, builder: &mut VgmBuilder) {
        for command in self.pending.drain(..) {
            builder.add_vgm_command(command);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_pcm_preserves_every_signed_12_bit_value() {
        let samples: Vec<i16> = (-2048..=2047).collect();
        let mut packed = Vec::new();
        pack_pcm12(&samples, &mut packed);
        assert_eq!(packed.len(), samples.len() * 3 / 2);
        let decoded: Vec<i16> = packed
            .chunks_exact(3)
            .flat_map(|bytes| {
                [
                    i16::from_be_bytes([bytes[0], (bytes[1] & 0x0f) << 4]) >> 4,
                    i16::from_be_bytes([bytes[2], bytes[1] & 0xf0]) >> 4,
                ]
            })
            .collect();
        assert_eq!(decoded, samples);
    }
}

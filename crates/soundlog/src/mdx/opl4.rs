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
//! with the playback device and apply [`VGM_VOLUME`] / 256 as the PCM output
//! gain, since lazy streams cannot carry the extra-header volume setting.
//! A finite conversion ends with a 256-sample release
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
//! preserves headroom and relative channel gains. MDX documents also specify
//! a relative OPL4 chip volume of 20 in the VGM extra header: a factor of five
//! compensates for normalization against 80 instead of PCM8's unity gain of 16,
//! and a factor of four compensates for the lower OPL4 output level relative
//! to OKIM6258 in libvgm. This approximates the original PCM/FM balance without
//! modifying sample values or saturating louder channel gains. Players must
//! honor extra-header chip volumes; exact balance remains renderer-dependent.
//! The main header also sets [`VGM_VOLUME_MODIFIER`] to compensate for libvgm's
//! overall attenuation when the larger PCM chip volume is included in its
//! normalization budget. This brings overall loudness close to default
//! OKIM6258 output while retaining the corrected PCM/FM ratio. This is a fixed
//! playback calibration, not per-song loudness normalization or a limiter;
//! other renderers and unusually loud mixes may need playback-level adjustment.
//!
//! MDX accepts up to 131,064 decoded samples per source, splitting samples
//! longer than 65,532 into two waves. Each source is decoded continuously
//! before splitting, preserving ADPCM decoder state. The second wave reuses
//! the slot's current rate, volume and pan; key-off cancels a pending switch.
//! Switches occur at VGM output-sample boundaries and reset the chip's
//! fractional source phase, introducing less than one output sample of delay
//! per split, rather than rounding to an MDX tick. The first wave includes
//! source lookahead for interpolation; only the final wave loops in silence.
//!
//! Up to 512 wave headers (including segments) and 4 MiB of ROM (including
//! the reserved wave-header region) are supported. Exceeding these limits
//! reports an error. The existing decoded-PCM, tick, per-tick command and
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
use crate::mdx::pcm_mixer::PcmChannelState;
use crate::mdx::pdx::PdxDocument;
use crate::vgm::command::{Instance, WaitSamples};
use crate::vgm::detail::{RomRamChipType, RomRamDump};
use crate::vgm::{VGM_SAMPLE_RATE, VgmBuilder, VgmDocument};

/// Standard OPL4 master clock used by MDX conversion and standalone PDX playback.
pub const CLOCK_HZ: u32 = 33_868_800;
/// Relative PCM chip volume for MDX playback in VGM Q8.8 units (20 times).
///
/// Eager MDX conversion stores this in the VGM extra header. Headerless playback
/// should multiply the OPL4 PCM output by `f32::from(VGM_VOLUME) / 256.0`.
/// Standalone PDX generators use native OPL4 levels and do not apply this gain.
pub const VGM_VOLUME: u16 = 20 * 0x100;
/// Global VGM volume modifier for MDX OPL4 documents (+15.05 dB).
///
/// VGM encodes the gain as `2^(modifier / 32)`, so `0x50` multiplies the whole
/// mix by approximately 5.657. This compensates for libvgm's overall volume
/// normalization after applying [`VGM_VOLUME`], without changing PCM/FM balance.
/// It is a document-level setting, not an additional OPL4 channel gain.
pub const VGM_VOLUME_MODIFIER: u8 = 0x50;
const HEADER_SIZE: usize = 12;
pub(super) const MAX_SAMPLE_COUNT: usize = 65_532;
pub(super) const MAX_SOURCE_SAMPLE_COUNT: usize = 2 * MAX_SAMPLE_COUNT;
const LOAD_WAIT: u16 = 14;
const RELEASE_WAIT: u16 = 256;

/// One independently controlled voice in standalone OPL4 playback.
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
/// not supported by this function. No source sample values are lost during packing.
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
/// samples require segmentation, which is only supported by MDX conversion.
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
/// reconstruction metadata are not supported by this function.
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

/// Creates a validation error with OPL4 context.
fn invalid(message: impl std::fmt::Display) -> ParseError {
    ParseError::DataInconsistency(format!("OPL4: {message}"))
}

/// Maps a PCM8A rate to octave bits, F-number and the actual Q16 playback step.
fn pitch(rate: u32) -> (u8, u16, u32) {
    let target_step = rate as f32 * PCM8_MASTER_SAMPLE_RATE as f32 / VGM_SAMPLE_RATE as f32;
    let mut best = (0, 0, 0);
    let mut error = f32::INFINITY;
    for octave in -7i8..=7 {
        let scale = 2f32.powi(i32::from(octave) + 5);
        let fnumber = (target_step / scale - 1024.0).round().clamp(0.0, 1023.0) as u16;
        let step = ((1024 + u32::from(fnumber)) << (octave + 7)) >> 2;
        let candidate_error = (step as f32 - target_step).abs();
        if candidate_error < error {
            best = (octave as u8 & 0x0f, fnumber, step);
            error = candidate_error;
        }
    }
    best
}

/// Emits a standalone playback register write followed by a one-sample wait.
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

/// Appends packed signed 12-bit sample pairs, zero-padding an odd final sample.
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

/// Builds a PCM12 header with an instant attack and a two-sample tail loop.
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
    waves: std::collections::HashMap<(usize, usize), WaveSequence>,
    wave_count: u16,
    next_address: u32,
    pub(super) steps: [u32; 8],
    rates: [u32; 8],
    continuation: [Option<u16>; 8],
    wave_high: [u8; 8],
    pan: [u8; 8],
    gain: [u8; 8],
    active: [bool; 8],
    pending: Vec<crate::vgm::command::VgmCommand>,
}

#[derive(Clone, Copy)]
struct WaveSequence {
    first: u16,
    second: Option<u16>,
}

impl Opl4Output {
    /// Creates an empty ROM cache and eight inactive PCM slots.
    pub(super) fn new() -> Self {
        Self {
            waves: Default::default(),
            wave_count: 0,
            next_address: 0x2000,
            steps: [0; 8],
            rates: [0; 8],
            continuation: [None; 8],
            wave_high: [0; 8],
            pan: [0; 8],
            gain: [16; 8],
            active: [false; 8],
            pending: Vec::new(),
        }
    }

    /// Enables PCM access, configures the output mixer and mutes all used slots.
    pub(super) fn initialize(&mut self) {
        self.write(1, 0x05, 0x03);
        self.write(2, 0x02, 0);
        self.write(2, 0xf8, 0x3f);
        self.write(2, 0xf9, 0);
        for channel in 0..8 {
            self.stop(channel);
        }
    }

    /// Caches a decoded source as one or two waves and starts its first wave.
    pub(super) fn start(
        &mut self,
        channel: usize,
        range: (usize, usize),
        samples: &[i16],
        rate: u32,
        gain: u8,
    ) -> Result<(), MdxConvertError> {
        let sequence = if let Some(&sequence) = self.waves.get(&range) {
            sequence
        } else {
            if range.1 == 0 || range.1 > MAX_SOURCE_SAMPLE_COUNT {
                return Err(MdxConvertError::InvalidDocument(format!(
                    "OPL4 sample has {} decoded values; maximum is {MAX_SOURCE_SAMPLE_COUNT} across two waves",
                    range.1
                )));
            }
            let segment_count = range.1.div_ceil(MAX_SAMPLE_COUNT);
            if usize::from(self.wave_count) + segment_count > 512 {
                return Err(MdxConvertError::InvalidDocument(
                    "OPL4 wave table exceeds 512 waves (including sample segments)".into(),
                ));
            }
            let data_size = range.1.div_ceil(2) * 3 + segment_count * 3;
            let end_address = self.next_address + data_size as u32;
            if end_address > 0x40_0000 {
                return Err(MdxConvertError::InvalidDocument(
                    "OPL4 decoded PCM ROM exceeds 4 MiB".into(),
                ));
            }
            let sequence = WaveSequence {
                first: self.wave_count,
                second: (segment_count == 2).then_some(self.wave_count + 1),
            };
            let source = &samples[range.0..range.0 + range.1];
            for (index, segment) in source.chunks(MAX_SAMPLE_COUNT).enumerate() {
                let mut data = Vec::with_capacity(segment.len().div_ceil(2) * 3 + 3);
                pack_pcm12(segment, &mut data);
                if segment_count == 2 && index == 0 {
                    // Keep interpolation at the seam on source data until the wave switch.
                    pack_pcm12(
                        &source[MAX_SAMPLE_COUNT..source.len().min(MAX_SAMPLE_COUNT + 2)],
                        &mut data,
                    );
                } else {
                    data.extend_from_slice(&[0; 3]);
                }
                let start = self.next_address;
                self.rom(
                    u32::from(self.wave_count) * 12,
                    wave_header(start, segment.len()).to_vec(),
                );
                self.next_address += data.len() as u32;
                self.rom(start, data);
                self.wave_count += 1;
            }
            self.waves.insert(range, sequence);
            sequence
        };
        self.play_wave(channel, sequence.first, rate, gain);
        self.continuation[channel] = sequence.second;
        Ok(())
    }

    /// Restarts a slot on a cached wave, retaining its current pan.
    fn play_wave(&mut self, channel: usize, wave: u16, rate: u32, gain: u8) {
        self.stop(channel);
        self.rates[channel] = rate;
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
    }

    /// Mutes and releases a slot, cancelling any pending second-wave switch.
    pub(super) fn stop(&mut self, channel: usize) {
        self.continuation[channel] = None;
        self.active[channel] = false;
        self.write(2, 0x50 + channel as u8, 0xff);
        self.emit_key(channel);
    }

    /// Converts PCM8 gain to total-level attenuation and applies zero-gain mute.
    pub(super) fn set_gain(&mut self, channel: usize, gain: u8) {
        self.gain[channel] = gain;
        // Normalize PCM8's maximum gain (80) to the OPL4's maximum level.
        let level = if gain == 0 {
            127
        } else {
            (-20.0 * (f32::from(gain) / 80.0).log10() / 0.375)
                .round()
                .clamp(0.0, 127.0) as u8
        };
        self.write(2, 0x50 + channel as u8, (level << 1) | 1);
        if self.active[channel] {
            self.emit_key(channel);
        }
    }

    /// Maps an MDX PCM pan value to the slot's native pan or mute setting.
    pub(super) fn set_pan(&mut self, channel: usize, raw: u8) {
        self.pan[channel] = match raw & 3 {
            0 => 8,
            1 => 9,
            2 => 7,
            _ => 0,
        };
        self.emit_key(channel);
    }

    /// Updates pitch without retriggering or changing the selected wave.
    pub(super) fn set_rate(&mut self, channel: usize, rate: u32) {
        self.rates[channel] = rate;
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

    /// Time to the earliest second-wave switch, in VGM output samples.
    pub(super) fn samples_until_continuation(
        &self,
        channels: &[PcmChannelState; 8],
    ) -> Option<u32> {
        channels
            .iter()
            .enumerate()
            .filter_map(|(index, state)| {
                if self.continuation[index].is_none() || state.block_length == 0 {
                    return None;
                }
                let remaining =
                    ((MAX_SAMPLE_COUNT as u32 - state.pos_in_block) << 16) - state.rate_counter;
                Some(remaining.div_ceil(self.steps[index]))
            })
            .min()
    }

    /// Advance channel positions and restart a second wave at the wait boundary.
    pub(super) fn advance(&mut self, channels: &mut [PcmChannelState; 8], samples: u32) {
        for (index, state) in channels.iter_mut().enumerate() {
            if state.block_length == 0 {
                continue;
            }
            // Split the product before multiplying: all PCM8A steps are below 65536.
            // This retains the exact Q16 phase even for waits up to u32::MAX.
            let fraction = (samples & 0xffff) * self.steps[index] + state.rate_counter;
            let advance = (samples >> 16) * self.steps[index] + (fraction >> 16);
            let position = state.pos_in_block + advance;
            state.rate_counter = fraction & 0xffff;
            state.pos_in_block = position.min(state.block_length);
            if position >= MAX_SAMPLE_COUNT as u32
                && let Some(wave) = self.continuation[index].take()
            {
                // A wave load resets the chip's fractional source position.
                state.pos_in_block = MAX_SAMPLE_COUNT as u32;
                state.rate_counter = 0;
                self.play_wave(index, wave, self.rates[index], self.gain[index]);
            } else if position >= state.block_length {
                state.block_length = 0;
                if !state.hold {
                    state.block_key = None;
                }
            }
        }
    }

    /// Queues the slot's key state and pan, muting inactive or zero-gain slots.
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

    /// Queues a register write at the current MDX playback timestamp.
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

    /// Queues a ROM data block at the given address within the 4 MiB image.
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

    /// Transfers queued ROM blocks and register writes to the VGM builder.
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
    fn single_precision_preserves_pcm8a_pitch_and_gain_registers() {
        for (rate, expected) in [
            (0x4000, (13, 427, 5804)),
            (0x5555, (13, 911, 7740)),
            (0x8000, (14, 427, 11608)),
            (0xaaaa, (14, 911, 15480)),
            (0x10000, (15, 427, 23216)),
            (0x15555, (15, 911, 30960)),
            (0x20000, (0, 427, 46432)),
        ] {
            assert_eq!(pitch(rate), expected);
        }
        let levels = [
            127, 101, 85, 76, 69, 64, 60, 56, 53, 51, 48, 46, 44, 42, 40, 39, 37, 36, 35, 33, 32,
            31, 30, 29, 28, 27, 26, 25, 24, 24, 23, 22, 21, 21, 20, 19, 18, 18, 17, 17, 16, 15, 15,
            14, 14, 13, 13, 12, 12, 11, 11, 10, 10, 10, 9, 9, 8, 8, 7, 7, 7, 6, 6, 6, 5, 5, 4, 4,
            4, 3, 3, 3, 2, 2, 2, 1, 1, 1, 1, 0, 0,
        ];
        let mut output = Opl4Output::new();
        for (gain, level) in levels.into_iter().enumerate() {
            output.set_gain(0, gain as u8);
            assert!(matches!(output.pending.pop().unwrap(),
                crate::vgm::command::VgmCommand::Ymf278bWrite(_, spec)
                if spec.register == 0x50 && spec.value == (level << 1) | 1
            ));
        }
    }

    #[test]
    fn position_advance_matches_samplewise_phase_with_32_bit_products() {
        for (rate, _) in PCM8A_MODE_TABLE {
            let mut output = Opl4Output::new();
            output.steps[0] = pitch(rate).2;
            for wait in [0, 1, 2891, 65_535, 65_536, 184_990] {
                for phase in [0, 65_535] {
                    let mut channels: [PcmChannelState; 8] = Default::default();
                    channels[0].block_length = MAX_SOURCE_SAMPLE_COUNT as u32;
                    channels[0].rate_counter = phase;
                    let mut expected_position = 0;
                    let mut expected_phase = phase;
                    for _ in 0..wait {
                        expected_phase += output.steps[0];
                        expected_position += expected_phase >> 16;
                        expected_phase &= 0xffff;
                    }
                    output.advance(&mut channels, wait);
                    assert_eq!(
                        channels[0].pos_in_block,
                        expected_position.min(MAX_SOURCE_SAMPLE_COUNT as u32)
                    );
                    assert_eq!(channels[0].rate_counter, expected_phase);
                }
            }
            let mut channels: [PcmChannelState; 8] = Default::default();
            channels[0].block_length = MAX_SOURCE_SAMPLE_COUNT as u32;
            channels[0].pos_in_block = MAX_SAMPLE_COUNT as u32;
            channels[0].rate_counter = 65_535;
            output.advance(&mut channels, u32::MAX);
            assert_eq!(channels[0].block_length, 0);
            assert_eq!(channels[0].pos_in_block, MAX_SOURCE_SAMPLE_COUNT as u32);
            assert_eq!(channels[0].rate_counter, 65_535 - output.steps[0]);
        }
    }

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

    #[test]
    fn segmented_rom_preserves_source_values_and_valid_wave_headers() {
        for count in [65_532, 65_533, 101_276, 131_063, 131_064] {
            let samples: Vec<i16> = (0..count).map(|i| (i % 4096) as i16 - 2048).collect();
            let mut output = Opl4Output::new();
            output.start(0, (0, count), &samples, 0x10000, 16).unwrap();
            let blocks: Vec<_> = output
                .pending
                .iter()
                .filter_map(|command| match command {
                    crate::vgm::command::VgmCommand::DataBlock(block) => Some(&block.data[8..]),
                    _ => None,
                })
                .collect();
            assert_eq!(blocks.len(), count.div_ceil(MAX_SAMPLE_COUNT) * 2);
            for (index, source) in samples.chunks(MAX_SAMPLE_COUNT).enumerate() {
                let header = blocks[index * 2];
                let data = blocks[index * 2 + 1];
                let padded = source.len().next_multiple_of(2);
                assert_eq!(u16::from_be_bytes([header[3], header[4]]) as usize, padded);
                assert_eq!(
                    0u16.wrapping_sub(u16::from_be_bytes([header[5], header[6]])) as usize,
                    padded + 2
                );
                let decoded: Vec<i16> = data[..padded * 3 / 2]
                    .chunks_exact(3)
                    .flat_map(|bytes| {
                        [
                            i16::from_be_bytes([bytes[0], bytes[1] << 4]) >> 4,
                            i16::from_be_bytes([bytes[2], bytes[1] & 0xf0]) >> 4,
                        ]
                    })
                    .collect();
                assert!(decoded[..source.len()] == *source);
                if padded != source.len() {
                    assert_eq!(decoded[source.len()], 0);
                }
            }
            let mut expected_tail = Vec::new();
            if count > MAX_SAMPLE_COUNT {
                pack_pcm12(
                    &samples[MAX_SAMPLE_COUNT..count.min(MAX_SAMPLE_COUNT + 2)],
                    &mut expected_tail,
                );
            } else {
                expected_tail.extend_from_slice(&[0; 3]);
            }
            assert_eq!(&blocks[1][blocks[1].len() - 3..], expected_tail);
        }
    }

    #[test]
    fn continuation_uses_current_rate_and_controls_then_clears_on_stop() {
        let samples = vec![1024; MAX_SOURCE_SAMPLE_COUNT];
        for (rate, _) in PCM8A_MODE_TABLE {
            let mut output = Opl4Output::new();
            output
                .start(0, (0, samples.len()), &samples, rate, 16)
                .unwrap();
            let mut channels: [PcmChannelState; 8] = Default::default();
            channels[0].block_length = samples.len() as u32;
            channels[0].hold = true;
            let wait = output.samples_until_continuation(&channels).unwrap();
            assert_eq!(
                wait,
                ((MAX_SAMPLE_COUNT as u32) << 16).div_ceil(output.steps[0])
            );
            output.advance(&mut channels, wait - 1);
            assert_eq!(output.samples_until_continuation(&channels), Some(1));
            output.set_rate(0, 0x8000);
            output.set_gain(0, 80);
            output.set_pan(0, 1);
            let wait = output.samples_until_continuation(&channels).unwrap();
            output.pending.clear();
            output.advance(&mut channels, wait);
            assert_eq!(channels[0].pos_in_block, MAX_SAMPLE_COUNT as u32);
            assert_eq!(channels[0].rate_counter, 0);
            assert!(channels[0].hold);
            assert_eq!(output.samples_until_continuation(&channels), None);
            let mut builder = VgmBuilder::new();
            output.flush(&mut builder);
            assert!(builder.drain_commands().any(|command| matches!(command,
                crate::vgm::command::VgmCommand::Ymf278bWrite(_, spec)
                if spec.register == 0x68 && spec.value == 0x89
            )));
            output
                .start(0, (0, samples.len()), &samples, rate, 16)
                .unwrap();
            output.stop(0);
            assert_eq!(output.samples_until_continuation(&channels), None);
        }
    }

    #[test]
    fn two_wave_samples_consume_two_entries_and_preserve_high_wave_bit() {
        let samples = vec![0; MAX_SOURCE_SAMPLE_COUNT];
        let mut output = Opl4Output::new();
        for index in 0..510 {
            output.start(0, (index, 1), &samples, 0x10000, 16).unwrap();
        }
        output
            .start(0, (0, samples.len()), &samples, 0x10000, 16)
            .unwrap();
        assert_eq!(output.wave_count, 512);
        let mut channels: [PcmChannelState; 8] = Default::default();
        channels[0].block_length = samples.len() as u32;
        let wait = output.samples_until_continuation(&channels).unwrap();
        output.advance(&mut channels, wait);
        assert_eq!(output.wave_high[0], 1);
        output.set_rate(0, 0x8000);
        assert_eq!(output.wave_high[0], 1);
        assert!(
            output
                .start(0, (511, 1), &samples, 0x10000, 16)
                .unwrap_err()
                .to_string()
                .contains("512")
        );
    }
}

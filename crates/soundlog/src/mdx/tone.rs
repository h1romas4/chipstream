//! MDX OPM tone data.
//!
//! An MDX tone is a 27-byte OPM voice definition containing global algorithm
//! fields and four operator records. This module provides typed tone and
//! tone-bank representations plus their byte-level conversion helpers.
//!
//! Tone parsing is independent of track playback, so conversion code can
//! reuse the same definitions for eager and lazy VGM paths.

use std::array;

/// One 27-byte OPM tone record used by MXDRV.
///
/// The serialized layout is one voice number byte, one byte containing the
/// connection and feedback fields, one operator-count/algorithm byte, and 24
/// bytes containing six groups of four operator fields. Operators are kept in
/// MDX order and can be serialized with [`Self::to_bytes`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdxTone {
    /// Voice number associated with this tone record.
    pub voice_number: u8,
    /// OPM connection/algorithm value from the low three bits of byte 1.
    pub con: u8,
    /// OPM feedback value from bits 3..=5 of byte 1.
    pub fl: u8,
    /// OPM operator/algorithm byte stored at byte 2 of the record.
    pub op: u8,
    /// Operators are ordered M1, M2, C1, C2, as in the MDX MML definition.
    pub operators: [MdxOperator; 4],
}

/// One operator in an MDX MML OPM tone definition.
///
/// The fields correspond to the OPM operator registers and are stored in the
/// same M1, M2, C1, C2 order as [`MdxTone::operators`]. Values are represented
/// without normalization; callers constructing a value directly should keep
/// each field within the bit width used by the MDX tone format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MdxOperator {
    /// Attack rate, a five-bit value.
    pub ar: u8,
    /// Decay 1 rate, a five-bit value.
    pub dr: u8,
    /// Sustain rate, a five-bit value.
    pub sr: u8,
    /// Release rate, a four-bit value.
    pub rr: u8,
    /// Decay 1 level, a four-bit value stored in the high half of its byte.
    pub sl: u8,
    /// Total level, stored as one complete byte in the tone record.
    pub ol: u8,
    /// Key-scale rate, a two-bit value.
    pub ks: u8,
    /// Multiple, a four-bit value.
    pub ml: u8,
    /// Detune 1, a four-bit value.
    pub dt1: u8,
    /// Detune 2, a two-bit value.
    pub dt2: u8,
    /// Amplitude modulation enable flag, stored in the high bit of its byte.
    pub ame: u8,
}

impl MdxTone {
    /// Serialized size of one MDX tone record in bytes.
    pub const BYTE_LENGTH: usize = 27;

    /// Parse one MDX tone record from the beginning of `bytes`.
    ///
    /// Returns `None` when fewer than [`Self::BYTE_LENGTH`] bytes are
    /// available. Additional bytes are ignored. The packed connection,
    /// feedback, and operator fields are split into their typed components;
    /// values that occupy unused high bits are masked during parsing.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < Self::BYTE_LENGTH {
            return None;
        }
        Some(Self {
            voice_number: bytes[0],
            con: bytes[1] & 0x07,
            fl: (bytes[1] >> 3) & 0x07,
            op: bytes[2],
            operators: operators_from_bytes(&bytes[3..]),
        })
    }

    /// Serialize this tone into the canonical 27-byte MDX layout.
    ///
    /// The returned array contains the voice header followed by the six
    /// operator-field groups. The caller is responsible for supplying field
    /// values within the bit widths described by [`MdxOperator`].
    pub fn to_bytes(&self) -> [u8; Self::BYTE_LENGTH] {
        let mut bytes = [0; Self::BYTE_LENGTH];
        bytes[0] = self.voice_number;
        bytes[1] = self.con | (self.fl << 3);
        bytes[2] = self.op;
        bytes[3..].copy_from_slice(&operators_to_bytes(self.operators));
        bytes
    }
}

fn operators_from_bytes(bytes: &[u8]) -> [MdxOperator; 4] {
    let group = |index: usize| {
        let start = index * 4;
        let mut values = [0; 4];
        values.copy_from_slice(&bytes[start..start + 4]);
        values
    };
    let dt1_mul = group(0);
    let tl = group(1);
    let ks_ar = group(2);
    let ame_d1r = group(3);
    let dt2_d2r = group(4);
    let d1l_rr = group(5);
    array::from_fn(|index| MdxOperator {
        ar: ks_ar[index] & 0x1f,
        dr: ame_d1r[index] & 0x1f,
        sr: dt2_d2r[index] & 0x1f,
        rr: d1l_rr[index] & 0x0f,
        sl: d1l_rr[index] >> 4,
        ol: tl[index],
        ks: ks_ar[index] >> 6,
        ml: dt1_mul[index] & 0x0f,
        dt1: dt1_mul[index] >> 4,
        dt2: dt2_d2r[index] >> 6,
        ame: ame_d1r[index] >> 7,
    })
}

fn operators_to_bytes(operators: [MdxOperator; 4]) -> [u8; 24] {
    let mut bytes = [0; 24];
    let mut groups = [[0; 4]; 6];
    for (index, operator) in operators.iter().enumerate() {
        groups[0][index] = operator.dt1 << 4 | operator.ml;
        groups[1][index] = operator.ol;
        groups[2][index] = operator.ks << 6 | operator.ar;
        groups[3][index] = operator.ame << 7 | operator.dr;
        groups[4][index] = operator.dt2 << 6 | operator.sr;
        groups[5][index] = operator.sl << 4 | operator.rr;
    }
    for (group, values) in groups.iter().enumerate() {
        let start = group * 4;
        bytes[start..start + 4].copy_from_slice(values);
    }
    bytes
}

/// The tone area associated with an MDX document.
///
/// Complete 27-byte records are parsed into [`MdxTone`] values. Any final
/// incomplete record is retained verbatim in [`Self::trailing_bytes`], so
/// parsing and serializing a tone area preserves bytes that do not form a
/// complete tone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MdxToneBank {
    /// Complete tone records in their original order.
    pub tones: Vec<MdxTone>,
    /// Bytes after the final complete 27-byte tone record.
    pub trailing_bytes: Vec<u8>,
}

impl MdxToneBank {
    /// Parse all complete tone records in `bytes`.
    ///
    /// The input is divided into [`MdxTone::BYTE_LENGTH`]-byte records. Any
    /// remainder shorter than one record is copied into `trailing_bytes`.
    /// This function does not reject or otherwise interpret the remainder.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let tone_length = bytes.len() / MdxTone::BYTE_LENGTH * MdxTone::BYTE_LENGTH;
        let tones = bytes[..tone_length]
            .chunks_exact(MdxTone::BYTE_LENGTH)
            .filter_map(MdxTone::from_bytes)
            .collect();
        Self {
            tones,
            trailing_bytes: bytes[tone_length..].to_vec(),
        }
    }

    /// Serialize all tones followed by the preserved trailing bytes.
    ///
    /// The result is canonical for the typed tone records while retaining the
    /// original incomplete suffix unchanged.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes =
            Vec::with_capacity(self.tones.len() * MdxTone::BYTE_LENGTH + self.trailing_bytes.len());
        for tone in &self.tones {
            bytes.extend_from_slice(&tone.to_bytes());
        }
        bytes.extend_from_slice(&self.trailing_bytes);
        bytes
    }
}

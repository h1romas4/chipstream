//! Text encoding helpers used by MDX metadata.
//!
//! MDX metadata uses the CP932-compatible Shift JIS encoding used by MXDRV,
//! with additional X68000 gaiji mappings. This module decodes those bytes for
//! public header strings and applies the reference player's fallback behavior
//! for unknown or truncated sequences.
//!
//! Encoding is intentionally kept private to the MDX header layer; callers
//! work with decoded Rust strings while raw bytes remain available there for
//! serialization.

use crate::encoding::{cp932_to_unicode, cp932_unicode_to_code};

/// Decode MDX text using the CP932-compatible Shift JIS mapping used by MXDRV.
///
/// ASCII and half-width katakana are handled directly. Unknown or truncated
/// sequences are represented by `?`, matching NanoDrive8's display behavior.
pub(crate) fn decode_shift_jis(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len());
    let mut offset = 0;
    while offset < bytes.len() {
        let first = bytes[offset];
        offset += 1;
        if first < 0x80 {
            result.push(first as char);
            continue;
        }
        if (0xa1..=0xdf).contains(&first) {
            push_codepoint(&mut result, 0xff61 + u32::from(first - 0xa1));
            continue;
        }
        let Some(&second) = bytes.get(offset) else {
            result.push('?');
            break;
        };
        offset += 1;
        let code = u16::from_be_bytes([first, second]);
        let unicode = x68k_extended_to_unicode(code).or_else(|| cp932_to_unicode(code));
        match unicode {
            Some(unicode) => push_codepoint(&mut result, unicode),
            None => result.push('?'),
        }
    }
    result
}

/// Encode MDX text using the CP932-compatible Shift JIS mapping used by MXDRV.
///
/// X68000 gaiji mappings are preferred when a character has both a gaiji and a
/// standard CP932 representation. Characters without a supported mapping are
/// replaced with ASCII `?`, matching [`decode_shift_jis`] fallback behavior.
pub(crate) fn encode_shift_jis(text: &str) -> Vec<u8> {
    let mut result = Vec::with_capacity(text.len());
    for character in text.chars() {
        let code = match character as u32 {
            0x00..=0x7f => character as u16,
            0xff61..=0xff9f => 0xa1 + (character as u16 - 0xff61),
            codepoint => x68k_unicode_to_code(codepoint)
                .or_else(|| cp932_unicode_to_code(codepoint))
                .unwrap_or(u16::from(b'?')),
        };
        if code <= 0xff {
            result.push(code as u8);
        } else {
            result.extend_from_slice(&code.to_be_bytes());
        }
    }
    result
}

// X68000-specific gaiji mappings derived from NanoDrive8's sjis.cpp.
fn x68k_extended_to_unicode(code: u16) -> Option<u32> {
    match code {
        0xec4c => Some(0x20),
        0xec71 => Some(0x2163),
        0xec7b => Some(0x2161),
        0xf030..=0xf039 | 0xf130..=0xf139 => Some(
            [
                0x2070, 0x00b9, 0x00b2, 0x00b3, 0x2074, 0x2075, 0x2076, 0x2077, 0x2078, 0x2079,
            ][(code & 0x000f) as usize],
        ),
        0xf041..=0xf05a | 0xf141..=0xf15a => Some(0xe020 + u32::from((code as u8) - 0x41)),
        0xf230..=0xf239 | 0xf330..=0xf339 => Some(0x2080 + u32::from(code & 0x000f)),
        0xf241..=0xf25a | 0xf341..=0xf35a => Some(0xe000 + u32::from((code as u8) - 0x41)),
        _ => None,
    }
}

fn x68k_unicode_to_code(codepoint: u32) -> Option<u16> {
    match codepoint {
        0x2163 => Some(0xec71),
        0x2161 => Some(0xec7b),
        0x2070 => Some(0xf030),
        0x00b9 => Some(0xf031),
        0x00b2 => Some(0xf032),
        0x00b3 => Some(0xf033),
        0x2074..=0x2079 => Some(0xf030 + (codepoint as u16 - 0x2070)),
        0xe020..=0xe039 => Some(0xf041 + (codepoint as u16 - 0xe020)),
        0x2080..=0x2089 => Some(0xf230 + (codepoint as u16 - 0x2080)),
        0xe000..=0xe019 => Some(0xf241 + (codepoint as u16 - 0xe000)),
        _ => None,
    }
}

fn push_codepoint(result: &mut String, codepoint: u32) {
    if let Some(character) = char::from_u32(codepoint) {
        result.push(character);
    } else {
        result.push('?');
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_shift_jis, encode_shift_jis};

    #[test]
    fn decodes_cp932_and_x68000_extensions() {
        assert_eq!(decode_shift_jis(b"ASCII"), "ASCII");
        assert_eq!(
            decode_shift_jis(&[0x83, 0x65, 0x83, 0x58, 0x83, 0x67]),
            "テスト"
        );
        assert_eq!(decode_shift_jis(&[0xa6, 0xdf]), "ｦﾟ");
        assert_eq!(decode_shift_jis(&[0xec, 0x71, 0xf0, 0x31]), "Ⅳ¹");
    }

    #[test]
    fn replaces_unknown_and_truncated_sequences() {
        assert_eq!(decode_shift_jis(&[0x80, 0x40]), "?");
        assert_eq!(decode_shift_jis(&[0x82]), "?");
    }

    #[test]
    fn encodes_cp932_and_x68000_extensions() {
        assert_eq!(encode_shift_jis("ASCII"), b"ASCII");
        assert_eq!(
            encode_shift_jis("テスト"),
            [0x83, 0x65, 0x83, 0x58, 0x83, 0x67]
        );
        assert_eq!(encode_shift_jis("ｦﾟ"), [0xa6, 0xdf]);
        assert_eq!(encode_shift_jis("Ⅳ¹"), [0xec, 0x71, 0xf0, 0x31]);
        assert_eq!(encode_shift_jis("").as_slice(), [0xf0, 0x41]);
        assert_eq!(encode_shift_jis("😀"), b"?");
    }
}

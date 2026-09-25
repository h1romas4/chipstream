//! Compatibility helpers for non-standard MDX layouts.
//!
//! Some non-standard MDX files use command stubs or alternate track layouts
//! that a regular MDX parser cannot interpret directly. Compatibility helpers
//! can rewrite the minimum necessary metadata so the existing parser starts at
//! the actual command streams. Format-specific normalizers can live here
//! without changing the regular MDX parser.

use std::borrow::Cow;

use super::command::MdxCommand;
use super::header::MdxHeader;
use super::parser::parse_mdx_command;

/// Normalize an MXDRV16y track table before normal MDX parsing.
///
/// Files that do not contain positive `0xf1` track stubs are returned
/// byte-for-byte unchanged. For an MXDRV16y file, each positive `0xf1` at a
/// track-table entry is followed and its target is written back as that
/// track's offset. The command data itself is not modified.
///
/// This is deliberately a narrow normalization step; it does not attempt to
/// emulate MXDRV16y playback or reinterpret arbitrary MDX commands.
///
/// # Examples
///
/// ```
/// let mut builder = soundlog::mdx::document::MdxBuilder::new();
/// builder.add_mdx_command(
///     0,
///     soundlog::mdx::command::MdxRest::new(1).expect("valid rest length"),
/// );
/// let standard_mdx_bytes = builder.finalize().expect("valid MDX").to_bytes();
/// let normalized = soundlog::mdx::compat::normalize_mxdrv16y_tracks(&standard_mdx_bytes)
///     .expect("valid MDX");
///
/// assert_eq!(normalized.as_ref(), standard_mdx_bytes.as_slice());
/// ```
pub fn normalize_mxdrv16y_tracks(bytes: &[u8]) -> Result<Cow<'_, [u8]>, String> {
    let (header, _) = MdxHeader::parse(bytes).map_err(|error| error.to_string())?;
    let mut targets = Vec::with_capacity(header.track_count());

    for track in 0..header.track_count() {
        let Some(mut position) = header.track_position(track) else {
            targets.push(None);
            continue;
        };
        let mut target = None;
        let mut followed_jump = false;
        for _ in 0..64 {
            if followed_jump
                && bytes
                    .get(position..position.saturating_add(6))
                    .is_some_and(|bytes| bytes[..4] == [0xf6, 0x00, 0x00, 0xf5])
            {
                // MXDRV16y uses an empty infinite repeat as a dispatch trap;
                // follow the F5 offset to the next initialization stream.
                let offset = i16::from_be_bytes([bytes[position + 4], bytes[position + 5]]);
                position = position
                    .checked_add(6)
                    .and_then(|position| position.checked_add_signed(offset as isize))
                    .ok_or_else(|| format!("track {track} empty-loop target overflow"))?;
                if position >= bytes.len() {
                    return Err(format!(
                        "track {track} empty-loop target 0x{position:06x} is outside the MDX"
                    ));
                }
                continue;
            }
            let (command, length) = parse_mdx_command(bytes, position)
                .map_err(|error| format!("track {track} at 0x{position:06x}: {error}"))?;
            let MdxCommand::EndOfTrackLoop(command) = command else {
                if followed_jump {
                    target = Some(position);
                }
                break;
            };
            if command.offset <= 0 {
                if followed_jump {
                    target = Some(position);
                }
                break;
            }
            followed_jump = true;
            let command_end = position
                .checked_add(length)
                .ok_or_else(|| format!("track {track} offset overflow"))?;
            position = command_end
                .checked_add(command.offset as usize)
                .ok_or_else(|| format!("track {track} jump target overflow"))?;
            if position >= bytes.len() {
                return Err(format!(
                    "track {track} jump target 0x{position:06x} is outside the MDX"
                ));
            }
        }
        if target.is_none() && followed_jump {
            return Err(format!("track {track} has too many MXDRV16y F1 jumps"));
        }
        targets.push(target);
    }

    if !targets.iter().any(Option::is_some) {
        return Ok(Cow::Borrowed(bytes));
    }

    let mut normalized = bytes.to_vec();
    let table_start = header
        .base_offset
        .checked_add(2)
        .ok_or_else(|| "MDX track table offset overflow".to_owned())?;
    for (track, target) in targets.into_iter().enumerate() {
        let Some(target) = target else {
            continue;
        };
        let relative = target
            .checked_sub(header.base_offset)
            .and_then(|offset| u16::try_from(offset).ok())
            .ok_or_else(|| format!("track {track} target cannot be represented"))?;
        let entry = table_start
            .checked_add(track * 2)
            .ok_or_else(|| format!("track {track} table offset overflow"))?;
        let end = entry
            .checked_add(2)
            .ok_or_else(|| format!("track {track} table entry overflow"))?;
        if end > normalized.len() {
            return Err(format!("track {track} table entry is outside the MDX"));
        }
        normalized[entry..end].copy_from_slice(&relative.to_be_bytes());
    }

    Ok(Cow::Owned(normalized))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::normalize_mxdrv16y_tracks;

    #[test]
    fn rejects_non_mdx_bytes() {
        let bytes = b"not an mdx";
        assert!(normalize_mxdrv16y_tracks(bytes).is_err());
    }

    #[test]
    fn rewrites_positive_f1_track_stub_to_target() {
        let mut bytes = vec![0_u8; 286];
        bytes[0] = b'x';
        bytes[1..4].copy_from_slice(&[0x0d, 0x0a, 0x1a]);
        bytes[4] = 0;
        bytes[5..7].copy_from_slice(&0_u16.to_be_bytes());
        bytes[7..9].copy_from_slice(&20_u16.to_be_bytes());
        for track in 1..9 {
            let entry = 7 + track * 2;
            bytes[entry..entry + 2].copy_from_slice(&0xffff_u16.to_be_bytes());
        }
        bytes[25..28].copy_from_slice(&[0xf1, 0x01, 0x00]);
        bytes[284..286].copy_from_slice(&[0xf1, 0x00]);

        let normalized = normalize_mxdrv16y_tracks(&bytes).unwrap();

        assert!(matches!(normalized, Cow::Owned(_)));
        assert_eq!(&normalized[7..9], &279_u16.to_be_bytes());
    }

    #[test]
    fn skips_mxdrv16y_empty_repeat_trap() {
        let mut bytes = vec![0_u8; 293];
        bytes[0] = b'x';
        bytes[1..4].copy_from_slice(&[0x0d, 0x0a, 0x1a]);
        bytes[7..9].copy_from_slice(&20_u16.to_be_bytes());
        for track in 1..9 {
            let entry = 7 + track * 2;
            bytes[entry..entry + 2].copy_from_slice(&0xffff_u16.to_be_bytes());
        }
        bytes[25..28].copy_from_slice(&[0xf1, 0x01, 0x00]);
        bytes[284..290].copy_from_slice(&[0xf6, 0x00, 0x00, 0xf5, 0x00, 0x01]);
        bytes[291..293].copy_from_slice(&[0xf1, 0x00]);

        let normalized = normalize_mxdrv16y_tracks(&bytes).unwrap();

        assert_eq!(&normalized[7..9], &286_u16.to_be_bytes());
    }

    #[test]
    fn borrows_standard_mdx_bytes_without_rebuilding() {
        let mut builder = super::super::document::MdxBuilder::new();
        builder.add_mdx_command(0, super::super::command::MdxRest::new(1).unwrap());
        let bytes = builder.finalize().unwrap().to_bytes();
        let normalized = normalize_mxdrv16y_tracks(&bytes).unwrap();

        assert!(matches!(normalized, Cow::Borrowed(_)));
    }
}

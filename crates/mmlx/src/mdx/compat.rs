//! Compatibility helpers for non-standard MDX layouts.
//!
//! Some non-standard MDX files use command stubs or alternate track layouts
//! that a regular MDX parser cannot interpret directly. Compatibility helpers
//! can rewrite the minimum necessary metadata so the existing parser starts at
//! the actual command streams. Format-specific normalizers can live here
//! without changing the regular MDX parser.

use std::borrow::Cow;

use soundlog::mdx::command::MdxCommand;
use soundlog::mdx::header::MdxHeader;
use soundlog::mdx::parser::parse_mdx_command;

/// Normalize an MXDRV16y track table before normal MDX parsing.
///
/// Files that do not contain positive `0xf1` track stubs are returned
/// byte-for-byte unchanged. For an MXDRV16y file, each positive `0xf1` at a
/// track-table entry is followed and its target is written back as that
/// track's offset. The command data itself is not modified.
///
/// This is deliberately a narrow normalization step; it does not attempt to
/// emulate MXDRV16y playback or reinterpret arbitrary MDX commands.
pub fn normalize_mxdrv16y_tracks(bytes: &[u8]) -> Result<Cow<'_, [u8]>, String> {
    let (header, _) = MdxHeader::parse(bytes).map_err(|error| error.to_string())?;
    let mut targets = Vec::with_capacity(header.track_count());

    for track in 0..header.track_count() {
        let Some(position) = header.track_position(track) else {
            targets.push(None);
            continue;
        };
        let (command, length) = parse_mdx_command(bytes, position)
            .map_err(|error| format!("track {track} at 0x{position:06x}: {error}"))?;
        let target = match command {
            MdxCommand::EndOfTrackLoop(command) if command.offset > 0 => {
                let command_end = position
                    .checked_add(length)
                    .ok_or_else(|| format!("track {track} offset overflow"))?;
                let target = command_end
                    .checked_add(command.offset as usize)
                    .ok_or_else(|| format!("track {track} jump target overflow"))?;
                if target >= bytes.len() {
                    return Err(format!(
                        "track {track} jump target 0x{target:06x} is outside the MDX"
                    ));
                }
                Some(target)
            }
            _ => None,
        };
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
    fn borrows_standard_mdx_bytes_without_rebuilding() {
        let mut bytes = [0_u8; 27];
        bytes[0] = b'x';
        bytes[1..4].copy_from_slice(&[0x0d, 0x0a, 0x1a]);
        bytes[5..7].copy_from_slice(&0_u16.to_be_bytes());
        bytes[7..9].copy_from_slice(&20_u16.to_be_bytes());
        for track in 1..9 {
            let entry = 7 + track * 2;
            bytes[entry..entry + 2].copy_from_slice(&0xffff_u16.to_be_bytes());
        }
        bytes[25..27].copy_from_slice(&[0xf1, 0x00]);

        let normalized = normalize_mxdrv16y_tracks(&bytes).unwrap();

        assert!(matches!(normalized, Cow::Borrowed(_)));
    }
}

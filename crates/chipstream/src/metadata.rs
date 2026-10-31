//! Metadata probing for VGM, VGZ, and MDX files.

use std::error::Error;
use std::fmt;

use soundlog::mdx::compat::normalize_mdx_for_playback;
use soundlog::mdx::header::MdxHeader;
use soundlog::meta::Gd3;
use soundlog::vgm::header::VgmHeader;

const MAX_INPUT_BYTES: u64 = 64 * 1024 * 1024;

/// Standard metadata tags available from a VGM/VGZ or MDX file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackMetadata {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub comment: Option<String>,
}

/// Errors encountered while reading or parsing metadata.
#[derive(Debug)]
pub enum MetadataError {
    InvalidData(String),
    InvalidGd3(String),
    InputTooLarge { limit: u64 },
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidData(message) => f.write_str(message),
            Self::InvalidGd3(message) => write!(f, "invalid VGM GD3 metadata: {message}"),
            Self::InputTooLarge { limit } => {
                write!(f, "metadata input exceeds the {limit}-byte limit")
            }
        }
    }
}

impl Error for MetadataError {}

/// Parse standard GD3 metadata from uncompressed VGM bytes.
///
/// Callers handling VGZ input should decompress it before calling this function.
pub fn probe_vgm_bytes(bytes: &[u8]) -> Result<TrackMetadata, MetadataError> {
    check_size(bytes, MAX_INPUT_BYTES)?;
    let header = VgmHeader::from_bytes(bytes)
        .map_err(|error| MetadataError::InvalidData(format!("invalid VGM header: {error}")))?;
    if header.gd3_offset == 0 {
        return Ok(TrackMetadata::default());
    }

    let gd3_start = header
        .gd3_offset
        .checked_add(0x14)
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or_else(|| MetadataError::InvalidGd3("offset is out of range".to_owned()))?;
    let gd3_bytes = bytes
        .get(gd3_start..)
        .ok_or_else(|| MetadataError::InvalidGd3("offset exceeds the input".to_owned()))?;
    let gd3 =
        Gd3::try_from(gd3_bytes).map_err(|error| MetadataError::InvalidGd3(error.to_string()))?;

    Ok(TrackMetadata {
        title: first_nonempty(gd3.track_name_en, gd3.track_name_origin),
        artist: first_nonempty(gd3.author_name_en, gd3.author_name_origin),
        album: first_nonempty(gd3.game_name_en, gd3.game_name_origin),
        comment: nonempty(gd3.notes.unwrap_or_default()),
    })
}

/// Parse the title metadata from uncompressed MDX bytes.
pub fn probe_mdx_bytes(bytes: &[u8]) -> Result<TrackMetadata, MetadataError> {
    check_size(bytes, MAX_INPUT_BYTES)?;
    let normalized = normalize_mdx_for_playback(bytes)
        .map_err(|error| MetadataError::InvalidData(format!("invalid MDX input: {error}")))?;
    let (header, _) = MdxHeader::parse(normalized.as_ref())
        .map_err(|error| MetadataError::InvalidData(format!("invalid MDX header: {error}")))?;

    Ok(TrackMetadata {
        title: nonempty(header.title),
        ..TrackMetadata::default()
    })
}

fn check_size(bytes: &[u8], limit: u64) -> Result<(), MetadataError> {
    if bytes.len() as u64 > limit {
        return Err(MetadataError::InputTooLarge { limit });
    }
    Ok(())
}

fn first_nonempty(primary: Option<String>, fallback: Option<String>) -> Option<String> {
    primary
        .and_then(nonempty)
        .or_else(|| fallback.and_then(nonempty))
}

fn nonempty(value: String) -> Option<String> {
    let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use soundlog::mdx::document::MdxBuilder;
    use soundlog::meta::Gd3;
    use soundlog::vgm::VgmBuilder;

    use super::{TrackMetadata, probe_mdx_bytes, probe_vgm_bytes};

    #[test]
    fn vgm_probe_prefers_english_tags_and_falls_back_to_original() {
        let mut builder = VgmBuilder::new();
        builder.set_gd3(Gd3 {
            track_name_en: Some(" English Title ".to_owned()),
            track_name_origin: Some("Original Title".to_owned()),
            game_name_origin: Some("Game".to_owned()),
            author_name_origin: Some("Composer".to_owned()),
            notes: Some(" Notes ".to_owned()),
            ..Gd3::default()
        });
        let bytes: Vec<u8> = builder.finalize().into();

        assert_eq!(
            probe_vgm_bytes(&bytes).unwrap(),
            TrackMetadata {
                title: Some("English Title".to_owned()),
                artist: Some("Composer".to_owned()),
                album: Some("Game".to_owned()),
                comment: Some("Notes".to_owned()),
            }
        );
    }

    #[test]
    fn mdx_probe_reads_title() {
        let mut builder = MdxBuilder::new();
        builder.set_title("MDX Song");
        let bytes = builder.finalize().unwrap().to_bytes().unwrap();

        assert_eq!(
            probe_mdx_bytes(&bytes).unwrap().title.as_deref(),
            Some("MDX Song")
        );
    }
}

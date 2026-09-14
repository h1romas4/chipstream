//! Parsed MDX documents and source maps.
//!
//! This module assembles an MDX header, tone data, and typed per-track command
//! streams into an in-memory `MdxDocument`. It also provides the builder and
//! parsing/serialization entry points used by the MDX package layer.
//!
//! Responsibilities:
//! - Parse standard and extended MDX files, including their optional LZ body.
//! - Preserve enough source information to serialize parsed documents again.
//! - Build documents from typed commands without exposing byte-level offsets.

use crate::ParseError;
use crate::mdx::command::MdxCommand;
use crate::mdx::header::MdxHeader;
use crate::mdx::lz::{self, Result as LzResult};
use crate::mdx::parser::parse_mdx_command;
use crate::mdx::tone::{MdxTone, MdxToneBank};

/// Number of tracks allocated by [`MdxBuilder::new`].
const DEFAULT_TRACK_COUNT: usize = 9;
/// Minimum number of tracks allocated when an extended track is requested.
const EXTENDED_TRACK_COUNT: usize = 16;
/// Marker that identifies the NanoDriveX-compatible compressed MDX body.
const LZ_STREAM_MARKER: [u8; 4] = [0x7f, 0xff, 0xff, 0x4c];
/// Maximum decoded MDX size accepted by the LZ parser.
const MAX_DECODED_MDX_SIZE: usize = 64 * 1024 * 1024;

/// An in-memory MDX document consisting of a header, tone definitions, and
/// typed per-track command streams.
///
/// The document owns the parsed representation. It does not retain the input
/// byte slice after [`parse`][Self::parse] returns; call [`to_bytes`][Self::to_bytes]
/// to serialize the current typed state again. The public fields can be
/// inspected or changed directly, but callers should use
/// [`MdxBuilder`] when constructing a new document because the builder keeps
/// track offsets and header metadata consistent.
///
/// `tracks` uses zero-based track indices. Standard MDX files normally contain
/// nine tracks, while extended MDX files can contain sixteen. An empty vector
/// represents a track with no command stream.
///
/// Parsing an LZ-compressed MDX first decodes its body and parses that decoded
/// representation. Serialization produces the current document state and does
/// not preserve the original compressed byte stream unless compression is
/// explicitly enabled through [`MdxBuilder::set_lz_compressed`].
///
/// # Examples
///
/// ```
/// use soundlog::mdx::command::MdxRest;
/// use soundlog::mdx::document::{MdxBuilder, MdxDocument};
///
/// let mut builder = MdxBuilder::new();
/// builder.add_mdx_command(0, MdxRest::new(12).unwrap());
/// let bytes = builder.finalize().to_bytes();
/// let document = MdxDocument::parse(&bytes).unwrap();
///
/// assert_eq!(document.tracks.len(), 9);
/// assert!(!document.tracks[0].is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdxDocument {
    pub header: MdxHeader,
    pub tone_bank: MdxToneBank,
    pub tracks: Vec<Vec<MdxCommand>>,
    lz_compressed: bool,
}

/// Builder for constructing an [`MdxDocument`] from typed track commands.
///
/// A new builder starts with nine empty tracks, which is the standard MDX
/// layout. Adding or replacing track 9 or any later track automatically grows
/// the document to the extended 16-track layout. Tracks are zero-based, so
/// track `0` is the first track in the serialized MDX file.
///
/// The builder stores title and PDX name values as encoded MDX byte strings.
/// Use [`set_title`][Self::set_title] and [`set_pdx_name`][Self::set_pdx_name]
/// for Rust strings, or the corresponding byte methods when the source bytes
/// are already encoded in the MDX character set.
///
/// Call [`finalize`][Self::finalize] after adding commands. Finalization adds
/// an [`MdxEndOfTrack`][crate::mdx::command::MdxEndOfTrack] command to every non-terminated track, updates the
/// header track offsets, and returns the resulting document.
///
/// # Examples
///
/// ```
/// use soundlog::mdx::command::{MdxEndOfTrack, MdxRest};
/// use soundlog::mdx::document::MdxBuilder;
///
/// let mut builder = MdxBuilder::new();
/// builder.add_mdx_command(0, MdxRest::new(24).unwrap());
/// builder.add_mdx_command(0, MdxEndOfTrack);
/// let document = builder.finalize();
///
/// assert_eq!(document.tracks.len(), 9);
/// assert_eq!(document.tracks[0].len(), 2);
/// ```
pub struct MdxBuilder {
    document: MdxDocument,
}

impl MdxBuilder {
    /// Creates a builder with an empty nine-track MDX document.
    ///
    /// The returned builder has no title, PDX filename, tone definitions, or
    /// track commands. Track offsets are calculated when [`finalize`][Self::finalize]
    /// is called.
    pub fn new() -> Self {
        Self {
            document: MdxDocument {
                header: MdxHeader {
                    title: String::new(),
                    title_raw_bytes: Vec::new(),
                    pdx_name: None,
                    pdx_name_raw_bytes: None,
                    base_offset: 0,
                    tone_data_offset: 0,
                    track_offsets: vec![None; DEFAULT_TRACK_COUNT],
                },
                tone_bank: MdxToneBank::default(),
                tracks: vec![Vec::new(); DEFAULT_TRACK_COUNT],
                lz_compressed: false,
            },
        }
    }

    /// Sets the title using already encoded MDX text bytes.
    ///
    /// The bytes are retained for serialization and decoded into the
    /// human-readable [`MdxHeader::title`] field. The input should contain the
    /// title bytes without the terminating NUL byte used in an MDX header.
    ///
    /// [`MdxHeader::title`]: crate::mdx::header::MdxHeader::title
    pub fn set_title_bytes(&mut self, bytes: Vec<u8>) -> &mut Self {
        self.document.header.title = crate::mdx::encoding::decode_shift_jis(&bytes);
        self.document.header.title_raw_bytes = bytes;
        self
    }

    /// Sets the title from a Rust string, encoding it as MDX text.
    pub fn set_title(&mut self, title: &str) -> &mut Self {
        self.set_title_bytes(crate::mdx::encoding::encode_shift_jis(title))
    }

    /// Sets or clears the PDX filename using already encoded MDX text bytes.
    ///
    /// Pass `Some(bytes)` to set a filename or `None` to remove it. As with
    /// [`set_title_bytes`][Self::set_title_bytes], the bytes are retained in
    /// their encoded form for serialization and are also decoded for access
    /// through the header.
    pub fn set_pdx_name_bytes(&mut self, bytes: Option<Vec<u8>>) -> &mut Self {
        self.document.header.pdx_name =
            bytes.as_deref().map(crate::mdx::encoding::decode_shift_jis);
        self.document.header.pdx_name_raw_bytes = bytes;
        self
    }

    /// Sets or clears the PDX filename from a Rust string.
    pub fn set_pdx_name(&mut self, name: Option<&str>) -> &mut Self {
        self.set_pdx_name_bytes(name.map(crate::mdx::encoding::encode_shift_jis))
    }

    /// Appends a value convertible into an OPM tone to the generated tone bank.
    ///
    /// Tone definitions are serialized before the track command streams. The
    /// tone table offset is populated during [`finalize`][Self::finalize].
    pub fn append_tone<T>(&mut self, tone: T) -> &mut Self
    where
        T: Into<MdxTone>,
    {
        self.document.tone_bank.tones.push(tone.into());
        self
    }

    /// Enables or disables NanoDriveX-compatible LZ compression on serialization.
    ///
    /// Compression is applied by [`MdxDocument::to_bytes`] after the document
    /// has been finalized. This setting does not change the typed commands
    /// held by the builder.
    ///
    /// [`MdxDocument::to_bytes`]: crate::mdx::document::MdxDocument::to_bytes
    pub fn set_lz_compressed(&mut self, compressed: bool) -> &mut Self {
        self.document.lz_compressed = compressed;
        self
    }

    /// Replaces one track with the supplied command sequence.
    ///
    /// Track indices are zero-based. Setting track 9 or above grows the
    /// document to at least the extended 16-track form, while preserving all
    /// existing tracks. An empty replacement track is allowed; it remains
    /// absent from the serialized track table until commands are added.
    pub fn set_track(&mut self, track: usize, commands: Vec<MdxCommand>) -> &mut Self {
        if track >= self.document.tracks.len() {
            self.document
                .tracks
                .resize_with(EXTENDED_TRACK_COUNT.max(track + 1), Vec::new);
            self.document
                .header
                .track_offsets
                .resize(self.document.tracks.len(), None);
        }
        self.document.tracks[track] = commands;
        self
    }

    /// Appends a command convertible into an [`MdxCommand`] to a track.
    ///
    /// Track indices are zero-based. Referencing a track beyond the current
    /// range grows the document in the same way as [`set_track`][Self::set_track]
    /// and creates empty intermediate tracks.
    pub fn add_mdx_command<C>(&mut self, track: usize, command: C) -> &mut Self
    where
        C: Into<MdxCommand>,
    {
        if track >= self.document.tracks.len() {
            self.set_track(track, Vec::new());
        }
        self.document.tracks[track].push(command.into());
        self
    }

    /// Finalizes the document and recalculates its header offsets.
    ///
    /// Every track that does not already end with
    /// [`MdxEndOfTrack`][crate::mdx::command::MdxEndOfTrack] receives
    /// one. The returned [`MdxDocument`] is ready for [`to_bytes`][MdxDocument::to_bytes]
    /// or for conversion to a playback stream. Calling this method consumes
    /// the builder.
    pub fn finalize(mut self) -> MdxDocument {
        for track in &mut self.document.tracks {
            if !matches!(track.last(), Some(MdxCommand::EndOfTrack(_))) {
                track.push(MdxCommand::EndOfTrack(crate::mdx::command::MdxEndOfTrack));
            }
        }
        if !self.document.tone_bank.to_bytes().is_empty()
            && self.document.header.tone_data_offset == 0
        {
            self.document.header.tone_data_offset =
                u16::try_from(2 + self.document.tracks.len() * 2).unwrap_or(u16::MAX);
        }
        self.document.recalculate_offsets();
        self.document
    }
}

impl Default for MdxBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl MdxDocument {
    /// Parses an MDX byte stream and all present tracks.
    ///
    /// Both standard and NanoDriveX-compatible LZ-compressed MDX bodies are
    /// accepted. Parsing validates the header offsets and command boundaries,
    /// decodes the tone table, and converts each command into its typed
    /// [`MdxCommand`] representation.
    ///
    /// The input bytes are borrowed only during parsing. The returned document
    /// owns its header, tones, and commands, so it remains valid after the
    /// input buffer is released. For compressed input, the returned document
    /// contains the decoded representation and does not retain the compressed
    /// source bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, ParseError> {
        let decoded = decode_mdx_body(bytes)?;
        let parse_bytes = decoded.as_deref().unwrap_or(bytes);
        let (header, _) = MdxHeader::parse(parse_bytes)?;
        let tone_start = header
            .tone_data_position()
            .ok_or_else(|| ParseError::DataInconsistency("MDX tone data offset overflow".into()))?;
        let tone_end = (0..header.track_count())
            .filter_map(|track| header.track_position(track))
            .filter(|&position| position > tone_start)
            .min()
            .unwrap_or(parse_bytes.len());
        if tone_end > parse_bytes.len() || tone_start > tone_end {
            return Err(ParseError::DataInconsistency(
                "MDX tone data range is outside the file".into(),
            ));
        }
        let mut tracks = Vec::with_capacity(header.track_count());

        for track in 0..header.track_count() {
            let Some(position) = header.track_position(track) else {
                tracks.push(Vec::new());
                continue;
            };
            let track_end = (0..header.track_count())
                .filter_map(|next_track| header.track_position(next_track))
                .filter(|&next_position| next_position > position)
                .chain((tone_start > position).then_some(tone_start))
                .min()
                .unwrap_or(parse_bytes.len());
            tracks.push(parse_track(parse_bytes, position, track_end, track)?);
        }

        Ok(Self {
            header,
            tone_bank: MdxToneBank::from_bytes(&parse_bytes[tone_start..tone_end]),
            tracks,
            lz_compressed: false,
        })
    }

    /// Returns the serialized byte range of every command in every track.
    ///
    /// Each range is `(offset, length)`, where `offset` points to the command
    /// opcode in the document's uncompressed MDX representation. The offsets
    /// are recalculated from the current typed commands, so they reflect
    /// direct field edits and may differ from the original input after a
    /// document has been modified.
    ///
    /// Unknown commands whose serialized form is not available produce a zero
    /// length range.
    pub fn sourcemap(&self) -> Vec<Vec<(usize, usize)>> {
        self.tracks
            .iter()
            .enumerate()
            .map(|(track, commands)| {
                let mut offset = self.header.track_position(track).unwrap_or(0);
                commands
                    .iter()
                    .map(|command| {
                        let length = command.to_mdx_bytes().map_or(0, |bytes| bytes.len());
                        let range = (offset, length);
                        offset = offset.saturating_add(length);
                        range
                    })
                    .collect()
            })
            .collect()
    }

    /// Serializes the current document to a complete MDX byte stream.
    ///
    /// Header offsets are recalculated from the current header, tone bank, and
    /// track contents before serialization. The returned bytes are therefore a
    /// canonical representation of the typed document, not necessarily a
    /// byte-for-byte copy of the input passed to [`parse`][Self::parse].
    ///
    /// If LZ compression was enabled on the builder, the serialized body is
    /// encoded using the NanoDriveX-compatible format. Documents parsed from
    /// compressed input are not automatically marked for compressed output.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut document = self.clone();
        document.recalculate_offsets();
        let mut bytes = document.header.to_bytes();
        let tone_position = document.header.tone_data_position().unwrap_or(bytes.len());
        let tone_bytes = document.tone_bank.to_bytes();
        let mut track_bytes = Vec::with_capacity(document.tracks.len());
        for track in &document.tracks {
            track_bytes.push(
                track
                    .iter()
                    .filter_map(|command| command.to_mdx_bytes())
                    .flatten()
                    .collect::<Vec<_>>(),
            );
        }
        let body_end = document
            .header
            .track_offsets
            .iter()
            .enumerate()
            .filter_map(|(track, offset)| {
                offset.and_then(|_| {
                    document
                        .header
                        .track_position(track)
                        .map(|position| position.saturating_add(track_bytes[track].len()))
                })
            })
            .chain(Some(tone_position.saturating_add(tone_bytes.len())))
            .max()
            .unwrap_or(bytes.len());
        bytes.resize(body_end, 0);
        if let Some(end) = tone_position.checked_add(tone_bytes.len())
            && end <= bytes.len()
        {
            bytes[tone_position..end].copy_from_slice(&tone_bytes);
        }
        for (track, data) in track_bytes.iter().enumerate() {
            let Some(position) = document.header.track_position(track) else {
                continue;
            };
            let Some(end) = position.checked_add(data.len()) else {
                continue;
            };
            if end <= bytes.len() {
                bytes[position..end].copy_from_slice(data);
            }
        }
        if !document.lz_compressed {
            return bytes;
        }
        let body_start = document.header.base_offset;
        let mut compressed = bytes[..body_start].to_vec();
        compressed.extend_from_slice(&LZ_STREAM_MARKER);
        compressed.extend_from_slice(&lz::encode(&bytes[body_start..]));
        compressed
    }

    /// Recalculates the track offsets based on the current state of the document.
    ///
    /// This should be called whenever the tracks or tone bank are modified to ensure
    /// that the track offsets in the header are consistent with the actual positions
    /// of the track data in the MDX document.
    fn recalculate_offsets(&mut self) {
        self.header.track_offsets.resize(self.tracks.len(), None);
        self.header.base_offset = self.header.title_raw_bytes.len()
            + 3
            + self.header.pdx_name_raw_bytes.as_ref().map_or(0, Vec::len)
            + 1;
        let header_length = self.header.base_offset + 2 + self.tracks.len() * 2;
        let tone_table_end = self.header.base_offset + 2 + self.tracks.len() * 2;
        let tone_position = self.header.tone_data_position().unwrap_or(header_length);
        let tone_length = self.tone_bank.to_bytes().len();
        let mut position = if tone_position == tone_table_end {
            tone_position.saturating_add(tone_length)
        } else {
            header_length
        };
        for (track, commands) in self.tracks.iter().enumerate() {
            if commands.is_empty() {
                self.header.track_offsets[track] = None;
                continue;
            }
            let relative = position
                .checked_sub(self.header.base_offset)
                .and_then(|offset| u16::try_from(offset).ok());
            self.header.track_offsets[track] = relative;
            position = position.saturating_add(
                commands
                    .iter()
                    .filter_map(|command| command.to_mdx_bytes())
                    .map(|bytes| bytes.len())
                    .sum::<usize>(),
            );
        }
    }
}

impl TryFrom<&[u8]> for MdxDocument {
    /// The parse error returned when the input is not a valid MDX document.
    type Error = ParseError;

    /// Parses an MDX byte slice using [`MdxDocument::parse`].
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Self::parse(bytes)
    }
}

/// Decodes the body of an MDX document, handling LZ-compressed streams if present.
///
/// Returns `Ok(Some(decoded_bytes))` if the body was successfully decoded,
/// `Ok(None)` if no LZ stream marker was found, or an error if decoding failed.
fn decode_mdx_body(bytes: &[u8]) -> Result<Option<Vec<u8>>, ParseError> {
    let (_, body_start) = MdxHeader::parse(bytes)?;
    let Some(marker_offset) = bytes[body_start..]
        .windows(LZ_STREAM_MARKER.len())
        .position(|window| window == LZ_STREAM_MARKER)
    else {
        return Ok(None);
    };
    let stream_start = body_start
        .checked_add(marker_offset)
        .and_then(|offset| offset.checked_add(LZ_STREAM_MARKER.len()))
        .ok_or_else(|| ParseError::DataInconsistency("MDX LZ stream offset overflow".into()))?;
    let compressed = bytes.get(stream_start..).ok_or_else(|| {
        ParseError::DataInconsistency("MDX LZ stream has no compressed data".into())
    })?;
    if compressed.is_empty() {
        return Err(ParseError::DataInconsistency(
            "MDX LZ stream has no compressed data".into(),
        ));
    }

    let mut capacity = bytes.len().clamp(4096, MAX_DECODED_MDX_SIZE);
    loop {
        let mut decoded_body = vec![0; capacity.saturating_sub(body_start)];
        let result = lz::decode(compressed, &mut decoded_body);
        if result.result == LzResult::Ok {
            decoded_body.truncate(result.bytes_written);
            let mut decoded = bytes[..body_start].to_vec();
            decoded.extend_from_slice(&decoded_body);
            return Ok(Some(decoded));
        }
        if result.result != LzResult::OutputOverrun || capacity == MAX_DECODED_MDX_SIZE {
            return Err(ParseError::DataInconsistency(format!(
                "failed to decode MDX LZ stream: {:?}",
                result.result
            )));
        }
        capacity = capacity.saturating_mul(2).min(MAX_DECODED_MDX_SIZE);
    }
}

/// Parses a single track from an MDX document.
///
/// `bytes` is the full MDX document data.
/// `offset` is the starting offset of the track within `bytes`.
/// `end` is the ending offset of the track within `bytes`.
/// `track` is the track index (for error reporting purposes).
///
/// Returns a vector of parsed `MdxCommand`s if successful, or a `ParseError` if parsing fails.
fn parse_track(
    bytes: &[u8],
    mut offset: usize,
    end: usize,
    track: usize,
) -> Result<Vec<MdxCommand>, ParseError> {
    let mut commands = Vec::new();

    while offset < end {
        let (command, length) = parse_mdx_command(bytes, offset)?;
        if length == 0 {
            return Err(ParseError::DataInconsistency(format!(
                "MDX track {track} parser returned a zero-length command"
            )));
        }
        offset = offset.checked_add(length).ok_or_else(|| {
            ParseError::DataInconsistency(format!("MDX track {track} offset overflow"))
        })?;
        let end_of_track = matches!(command, MdxCommand::EndOfTrack(_));
        commands.push(command);
        if end_of_track {
            return Ok(commands);
        }
    }

    if offset == end {
        Ok(commands)
    } else {
        Err(ParseError::DataInconsistency(format!(
            "MDX track {track} extends past its offset boundary"
        )))
    }
}

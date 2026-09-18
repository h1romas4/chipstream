//! MDX documents together with their optional external PDX data.
//!
//! An `MdxPackage` is the format-level input to playback and VGM conversion:
//! it keeps parsed MDX command tracks beside the PDX sample banks named by
//! the MDX header.
//!
//! Responsibilities:
//! - Parse and serialize the MDX member and the optional PDX member.
//! - Resolve PCM note references against PDX banks without doing filesystem
//!   name resolution, which remains the caller's responsibility.
//! - Report whether the loaded package should engage the OKIM6258 path.

use crate::ParseError;
use crate::mdx::command::MdxCommand;
use crate::mdx::document::MdxDocument;
use crate::mdx::pcm::{Pcm8aFormat, PcmDecodeError, decode_pcm8a};
use crate::mdx::pdx::{PdxDocument, PdxSample};

/// A PCM note in an MDX track and its corresponding PDX table entry.
///
/// The track and bank values identify the lookup location in the package. The
/// note is the PCM note index within that bank, after subtracting the MDX PCM
/// note base (`0x80`). `sample` contains the resolved PDX table entry when a
/// PDX is loaded and the table entry is non-empty; otherwise it is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MdxPcmReference {
    /// Zero-based MDX track containing the PCM note.
    pub track: usize,
    /// PDX bank selected by the most recent voice/PCM-bank command in the track.
    pub bank: usize,
    /// Zero-based note index within the selected PDX bank.
    pub note: usize,
    /// Resolved PDX table entry, if the package contains a matching sample.
    pub sample: Option<PdxSample>,
}

/// An MDX document and the optional PDX sample data supplied by its caller.
///
/// `MdxPackage` is the format-level input used by playback and VGM
/// conversion. It keeps the parsed MDX command tracks together with the PDX
/// banks referenced by the MDX header, but it does not resolve a PDX filename
/// through the filesystem. Callers are responsible for locating the PDX bytes
/// and passing them to [`Self::parse`] or [`Self::parse_owned`].
///
/// A package can be constructed directly when the caller already has parsed
/// documents. For normal byte input, prefer the parsing constructors so that
/// header, command, and PDX table validation is applied consistently.
///
/// # Examples
///
/// ```
/// use soundlog::mdx::document::MdxBuilder;
/// use soundlog::mdx::package::MdxPackage;
///
/// let package = MdxPackage {
///     mdx: MdxBuilder::new().finalize().unwrap(),
///     pdx: None,
/// };
///
/// assert!(!package.drives_okim6258());
/// assert!(package.pdx_name().is_none());
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MdxPackage {
    pub mdx: MdxDocument,
    pub pdx: Option<PdxDocument>,
}

impl MdxPackage {
    /// Parses an MDX file and, when supplied, its referenced PDX file.
    ///
    /// The PDX filename is read from the MDX header, but filename resolution is
    /// intentionally left to the caller because it is filesystem- and
    /// case-sensitivity-dependent. The input slices are borrowed only while
    /// parsing; the returned package owns its parsed MDX and PDX documents.
    pub fn parse(mdx_bytes: &[u8], pdx_bytes: Option<&[u8]>) -> Result<Self, ParseError> {
        Ok(Self {
            mdx: MdxDocument::parse(mdx_bytes)?,
            pdx: pdx_bytes.map(PdxDocument::parse).transpose()?,
        })
    }

    /// Parses an MDX package from owned input buffers.
    ///
    /// This constructor is useful when the caller already owns file buffers
    /// and wants the package API to consume them. The resulting package owns
    /// parsed documents rather than the original input buffers. The PDX owned
    /// parser retains decoded sample data only, not the original compressed
    /// input representation.
    pub fn parse_owned(mdx_bytes: Vec<u8>, pdx_bytes: Option<Vec<u8>>) -> Result<Self, ParseError> {
        Ok(Self {
            mdx: MdxDocument::parse(&mdx_bytes)?,
            pdx: pdx_bytes.map(PdxDocument::parse_owned).transpose()?,
        })
    }

    /// Serializes the MDX member using [`MdxDocument::to_bytes`].
    ///
    /// The result is the current canonical MDX representation. It is not
    /// guaranteed to be byte-for-byte identical to the input used by
    /// [`Self::parse`], especially after typed fields have been changed.
    pub fn to_mdx_bytes(&self) -> Vec<u8> {
        self.mdx.to_bytes()
    }

    /// Returns whether conversion should drive OKIM6258 ADPCM/PCM8 playback.
    ///
    /// Mirrors NanoDriveX's real MDX player (`src/mdx.cpp`), which gates
    /// *all* OKIM6258 engagement — the initial `0x02` (ADPCM on) register
    /// write and every subsequent per-tick data byte — purely on whether a
    /// PDX file was actually loaded (`pdxLoaded`), not on whether any track
    /// happens to reference a PCM note. Every standard (non-extended) MDX
    /// file's header always declares 9 tracks regardless of whether the
    /// song uses the ninth (PCM) one, so `mdx.tracks.len() > 8` was never a
    /// valid "does this song use PCM8" check either.
    pub fn drives_okim6258(&self) -> bool {
        self.pdx.is_some()
    }

    /// Serializes the PDX member if one was supplied.
    ///
    /// Returns `None` when this package has no PDX document. The serialized
    /// bytes are reconstructed from the parsed PDX representation.
    pub fn to_pdx_bytes(&self) -> Option<Vec<u8>> {
        self.pdx.as_ref().map(PdxDocument::to_bytes)
    }

    /// Returns the PDX filename requested by the MDX header.
    ///
    /// The returned string is borrowed from the parsed MDX header and does not
    /// perform filesystem lookup or verify that a matching PDX was supplied.
    pub fn pdx_name(&self) -> Option<&str> {
        self.mdx.header.pdx_name.as_deref()
    }

    /// Resolves PCM notes in tracks 8 and above against the optional PDX bank.
    ///
    /// Track 8 is the ninth MDX track and is the first track considered for
    /// PCM references. The selected bank is tracked independently for each
    /// track and is updated by `VoiceOrPcmBank` commands before subsequent
    /// notes are resolved.
    ///
    /// A missing PDX or an empty table entry is represented by `sample: None`
    /// rather than an error. The returned references contain table metadata;
    /// use [`pcm_sample_bytes`][Self::pcm_sample_bytes] to access the encoded
    /// sample payload.
    pub fn pcm_references(&self) -> Vec<MdxPcmReference> {
        let Some(pdx) = self.pdx.as_ref() else {
            return self.pcm_references_without_pdx();
        };
        self.mdx
            .tracks
            .iter()
            .enumerate()
            .filter(|(track, _)| *track >= 8)
            .flat_map(|(track, commands)| {
                let mut bank = 0usize;
                commands.iter().filter_map(move |command| match command {
                    MdxCommand::VoiceOrPcmBank(command) => {
                        bank = usize::from(command.value);
                        None
                    }
                    MdxCommand::Note(command) if command.note >= 0x80 => {
                        let note = usize::from(command.note - 0x80);
                        Some(MdxPcmReference {
                            track,
                            bank,
                            note,
                            sample: pdx.entry(bank, note),
                        })
                    }
                    _ => None,
                })
            })
            .collect()
    }

    /// Returns the encoded payload for a resolved PCM reference.
    ///
    /// The returned slice is borrowed from the package's PDX document. Returns
    /// `None` if no PDX is loaded or if the bank/note does not identify a
    /// non-empty sample entry.
    pub fn pcm_sample_bytes(&self, reference: &MdxPcmReference) -> Option<&[u8]> {
        self.pdx
            .as_ref()?
            .sample_bytes(reference.bank, reference.note)
    }

    /// Decodes the payload for a resolved PCM reference.
    ///
    /// A missing PDX or an empty table entry returns `Ok(None)`. When a payload
    /// exists, it is decoded according to `format` and returned as signed
    /// 16-bit samples. Decode errors from fixed-width PCM formats are returned
    /// unchanged.
    pub fn decode_pcm_reference(
        &self,
        reference: &MdxPcmReference,
        format: Pcm8aFormat,
    ) -> Result<Option<Vec<i16>>, PcmDecodeError> {
        self.pcm_sample_bytes(reference)
            .map(|bytes| decode_pcm8a(format, bytes))
            .transpose()
    }

    /// Returns PCM references for tracks without an associated PDX.
    fn pcm_references_without_pdx(&self) -> Vec<MdxPcmReference> {
        self.mdx
            .tracks
            .iter()
            .enumerate()
            .filter(|(track, _)| *track >= 8)
            .flat_map(|(track, commands)| {
                let mut bank = 0usize;
                commands.iter().filter_map(move |command| match command {
                    MdxCommand::VoiceOrPcmBank(command) => {
                        bank = usize::from(command.value);
                        None
                    }
                    MdxCommand::Note(command) if command.note >= 0x80 => Some(MdxPcmReference {
                        track,
                        bank,
                        note: usize::from(command.note - 0x80),
                        sample: None,
                    }),
                    _ => None,
                })
            })
            .collect()
    }
}

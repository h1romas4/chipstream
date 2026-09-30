use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use soundlog::mdx::command::MdxCommand;
use soundlog::mdx::convert::{MdxPlaybackCheckLimits, MdxToVgmOptions, check_playback};
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::pdx::{PdxBuilder, PdxDocument};

use super::mml::{parse_input, parse_reader};

/// Options for parsing and bounded lazy playback validation.
#[derive(Debug, Clone, Copy)]
pub struct CheckOptions {
    /// Only parse MML, skipping compilation and playback.
    pub parse_only: bool,
    /// Number of whole-song playthroughs to check; must be positive.
    pub loop_count: u32,
    /// Execution budgets; exhausting them reports an incomplete check.
    pub limits: MdxPlaybackCheckLimits,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            parse_only: false,
            loop_count: 1,
            limits: MdxPlaybackCheckLimits::default(),
        }
    }
}

/// Check an MML file with default playback budgets and optionally print its syntax tree.
pub fn check(input: &Path, verbose: bool) -> Result<()> {
    check_with_stdin(input, verbose, false)
}

/// Check an MML file or stdin with default playback budgets.
pub fn check_with_stdin(input: &Path, verbose: bool, stdin: bool) -> Result<()> {
    check_with_options(input, verbose, stdin, CheckOptions::default())
}

/// Parse, compile, and check finite lazy playback without loading the referenced PDX.
///
/// Synthetic PCM payloads exercise playback but do not validate actual PDX files,
/// source-sample formats, audio, or sample-length-dependent behavior. Runtime
/// coordinates refer to zero-based MDX commands, not MML line and column numbers.
pub fn check_with_options(
    input: &Path,
    verbose: bool,
    stdin: bool,
    options: CheckOptions,
) -> Result<()> {
    let document = if stdin || input == Path::new("-") {
        parse_reader(input, io::stdin().lock())?
    } else {
        parse_input(input)?
    };
    check_document(input, &document, options)?;
    if verbose {
        let tree = mmlx::mdx::format_tree(&document);
        let mut stdout = io::BufWriter::new(io::stdout().lock());
        match stdout
            .write_all(tree.as_bytes())
            .and_then(|()| stdout.write_all(b"\n"))
            .and_then(|()| stdout.flush())
        {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}
            Err(error) => return Err(error).context("failed to write verbose output"),
        }
    }
    Ok(())
}

/// Validate a parsed document using the same diagnostics for files and stdin.
fn check_document(
    input: &Path,
    document: &mmlx::mdx::MmlDocument,
    options: CheckOptions,
) -> Result<()> {
    if options.parse_only {
        return Ok(());
    }
    if options.loop_count == 0 {
        return Err(anyhow!(
            "{}: error: loop count must be positive",
            input.display()
        ));
    }
    let mdx = mmlx::mdx::compile(document)
        .map_err(|error| anyhow!("{}: error: compile error: {error}", input.display()))?;
    check_compiled_document(input, mdx, options)
}

pub(super) fn check_compiled_document(
    input: &Path,
    mdx: MdxDocument,
    options: CheckOptions,
) -> Result<()> {
    let pdx = dummy_pdx(&mdx)
        .with_context(|| format!("{}: error: failed to prepare dummy PDX", input.display()))?;
    let package = MdxPackage { mdx, pdx };
    check_playback(
        &package,
        MdxToVgmOptions {
            loop_count: Some(options.loop_count),
            ..Default::default()
        },
        options.limits,
    )
    .map_err(|error| anyhow!("{}: error: {error}", input.display()))
}

/// Populate possible PCM bank/note combinations with short, even-length silence.
fn dummy_pdx(mdx: &MdxDocument) -> Result<Option<PdxDocument>> {
    let mut banks = BTreeSet::from([0]);
    let mut notes = BTreeSet::new();
    for command in mdx.tracks.iter().skip(8).flatten() {
        match command {
            MdxCommand::VoiceOrPcmBank(command) => {
                banks.insert(usize::from(command.value));
            }
            MdxCommand::Note(command) => {
                if let Some(note) = command.note.checked_sub(0x80) {
                    notes.insert(usize::from(note));
                }
            }
            _ => {}
        }
    }
    if notes.is_empty() && mdx.header.pdx_name.is_none() {
        return Ok(None);
    }
    let mut builder = PdxBuilder::new();
    for bank in banks {
        for &note in &notes {
            builder.set_sample(bank, note, vec![0; 4])?;
        }
    }
    Ok(Some(builder.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Checks defaults, compile failures, runtime failures, and parse-only compatibility.
    #[test]
    fn checks_compilation_and_runtime_beyond_parsing() {
        let input = Path::new("songs/check.mml");
        for source in ["A r4", "#pcmfile \"does-not-exist.pdx\"\nP c4"] {
            let document = super::super::mml::parse_source(input, source).unwrap();
            check_document(input, &document, CheckOptions::default()).unwrap();
        }
        for (source, message) in [
            ("A c193", "compile error"),
            ("A @42 c4", "missing tone for voice 42"),
        ] {
            let document = super::super::mml::parse_source(input, source).unwrap();
            let error = check_document(input, &document, CheckOptions::default())
                .unwrap_err()
                .to_string();
            assert!(error.starts_with("songs/check.mml: error:"));
            assert!(error.contains(message), "{error}");
            if source.contains("@42") {
                assert!(error.contains("track 0, MDX command 1"), "{error}");
            }
            check_document(
                input,
                &document,
                CheckOptions {
                    parse_only: true,
                    ..Default::default()
                },
            )
            .unwrap();
        }
    }

    /// Checks finite song loops and path-preserving diagnostics for reader input.
    #[test]
    fn checks_requested_loop_count_and_reader_input() {
        let input = Path::new("songs/unsaved.mml");
        let document = parse_reader(input, Cursor::new("P L c4")).unwrap();
        let options = CheckOptions {
            limits: MdxPlaybackCheckLimits {
                max_ticks: 64,
                max_commands: 1000,
            },
            ..Default::default()
        };
        check_document(input, &document, options).unwrap();
        let error = check_document(
            input,
            &document,
            CheckOptions {
                loop_count: 3,
                ..options
            },
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.starts_with(
                "songs/unsaved.mml: error: playback check incomplete: tick limit exceeded"
            ),
            "{error}"
        );
        assert!(
            check_document(
                input,
                &document,
                CheckOptions {
                    loop_count: 0,
                    ..options
                }
            )
            .is_err()
        );
        let document = parse_reader(input, Cursor::new("A @42 c4")).unwrap();
        let error = check_document(input, &document, CheckOptions::default())
            .unwrap_err()
            .to_string();
        assert_eq!(
            error,
            "songs/unsaved.mml: error: missing tone for voice 42 (track 0, MDX command 1)"
        );
    }

    /// Checks synthetic samples for bank/note combinations carried across loops.
    #[test]
    fn dummy_pdx_covers_possible_banks_and_notes() {
        let document =
            super::super::mml::parse_source(Path::new("test.mml"), "P @2 c4\nQ @4 d4").unwrap();
        let mdx = mmlx::mdx::compile(&document).unwrap();
        let pdx = dummy_pdx(&mdx).unwrap().unwrap();
        let package = MdxPackage { mdx, pdx: None };
        for reference in package.pcm_references() {
            for bank in [0, 2, 4] {
                assert_eq!(pdx.sample_bytes(bank, reference.note), Some(&[0; 4][..]));
            }
        }
    }
}

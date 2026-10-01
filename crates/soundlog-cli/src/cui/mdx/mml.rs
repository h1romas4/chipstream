use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use mmlx::diagnostic::Diagnostic;
use mmlx::frontend::{SourceFile, SourceMap};
use mmlx::mdx::frontend::{self, CompiledMdx, MdxLocation, ParsedMml};

pub(super) fn read_input_source(input: &Path) -> Result<String> {
    fs::read_to_string(input)
        .with_context(|| format!("failed to read MML input: {}", input.display()))
}

pub(super) fn read_reader_source<R: Read>(input: &Path, mut reader: R) -> Result<String> {
    let mut source = String::new();
    reader.read_to_string(&mut source).with_context(|| {
        format!(
            "failed to read MML input from stdin for {}",
            input.display()
        )
    })?;
    Ok(source)
}

pub(super) fn compile_document<'source>(
    input: &Path,
    document: &ParsedMml<'source>,
) -> Result<CompiledMdx<'source>> {
    frontend::compile(document).map_err(|error| {
        let position = error
            .span
            .and_then(|span| document.source().position(span.start()));
        match position {
            Some(position) => anyhow!(
                "{}:{}:{}: error: compile error: {error}",
                input.display(),
                position.line_number,
                position.column
            ),
            None => anyhow!("{}: error: compile error: {error}", input.display()),
        }
    })
}

pub(super) fn playback_error(
    input: &Path,
    source: SourceFile<'_>,
    source_map: &SourceMap<MdxLocation>,
    error: soundlog::mdx::convert::MdxPlaybackCheckError,
) -> anyhow::Error {
    use soundlog::mdx::convert::MdxPlaybackCheckError;
    let (track, command_index) = match &error {
        MdxPlaybackCheckError::Conversion {
            track,
            command_index,
            ..
        }
        | MdxPlaybackCheckError::LimitExceeded {
            track,
            command_index,
            ..
        } => (*track, *command_index),
    };
    let position = track
        .zip(command_index)
        .and_then(|(track, index)| source_map.get(&MdxLocation::TrackCommand { track, index }))
        .and_then(|span| source.position(span.start()));
    match position {
        Some(position) => anyhow!(
            "{}:{}:{}: error: {error}",
            input.display(),
            position.line_number,
            position.column
        ),
        None => anyhow!("{}: error: {error}", input.display()),
    }
}

pub(super) fn parse_source<'source>(
    input: &Path,
    source: &'source str,
) -> Result<ParsedMml<'source>> {
    frontend::parse(source).map_err(|error| parse_error(input, source, error))
}

fn parse_error(input: &Path, source: &str, error: Diagnostic) -> anyhow::Error {
    let located = SourceFile::new(source).and_then(|source| {
        let span = error.span?;
        let position = source.position(span.start())?;
        let width = source
            .slice(span)?
            .split('\n')
            .next()?
            .chars()
            .count()
            .max(1);
        Some((position, width))
    });
    let Some((position, width)) = located else {
        return anyhow!("{}: error: {error}", input.display());
    };
    let category = match error.code {
        "mmlx.invalid-value" | "mmlx.integer-overflow" => "value",
        _ => "syntax",
    };
    let line = position.line_number;
    let column = position.column;
    let end_column = column + width;
    let line_text = source
        .split('\n')
        .nth(line - 1)
        .unwrap_or("")
        .trim_end_matches('\r');
    anyhow!(
        "{}:{line}:{column}: error: MML {category} error at line {line}, columns {column}-{end_column}\n  {line_text}\n  {}{}\n  reason: {error}",
        input.display(),
        " ".repeat(column - 1),
        "^".repeat(width),
    )
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use super::*;

    #[test]
    fn playback_diagnostics_use_finalized_map_after_dropping_the_ast() {
        use soundlog::mdx::command::MdxCommand;
        use soundlog::mdx::convert::{MdxConvertError, MdxPlaybackCheckError};
        let input = Path::new("songs/unsaved.mml");
        let source = read_reader_source(input, Cursor::new("AP /* \u{65e5} */ @42 c%600")).unwrap();
        let parsed = parse_source(input, &source).unwrap();
        let compiled = compile_document(input, &parsed).unwrap();
        drop(parsed);
        assert_eq!(compiled.source().text().as_ptr(), source.as_ptr());
        assert!(matches!(
            compiled.document().tracks[0][0],
            MdxCommand::PcmMode(_)
        ));
        for (track, commands) in compiled.document().tracks.iter().enumerate() {
            for (index, command) in commands.iter().enumerate() {
                if matches!(command, MdxCommand::Note(_)) {
                    let error = MdxPlaybackCheckError::Conversion {
                        error: MdxConvertError::MissingTone { voice: 42 },
                        track: Some(track),
                        command_index: Some(index),
                    };
                    let message =
                        playback_error(input, compiled.source(), compiled.source_map(), error)
                            .to_string();
                    assert!(
                        message.starts_with(
                            "songs/unsaved.mml:1:16: error: missing tone for voice 42"
                        ),
                        "{message}"
                    );
                }
            }
        }
    }

    #[test]
    fn structured_parse_diagnostics_render_unicode_crlf_and_eof() {
        let input = Path::new("songs/broken.mml");
        let error = parse_source(input, "A r4\r\nB /* \u{65e5} */ t5000\r\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("songs/broken.mml:2:12: error: MML value error"),
            "{error}"
        );
        assert!(error.contains("  B /* \u{65e5} */ t5000\n"));
        assert!(!error.contains('\r'));
        let source = "A r4\n";
        let error = parse_error(
            input,
            source,
            Diagnostic::error(
                "mmlx.syntax",
                "unexpected end of input",
                mmlx::source::Span::new(source.len(), source.len()),
            ),
        )
        .to_string();
        assert!(
            error.starts_with("songs/broken.mml:2:1: error: MML syntax error"),
            "{error}"
        );
        assert!(error.contains("\n  \n  ^\n"));
    }

    #[test]
    fn compile_diagnostics_locate_original_reader_source_and_preserve_fallback() {
        let input = Path::new("songs/unsaved.mml");
        for (source, line, column, message) in [
            ("A a>>>>>>>>>a", 1, 13, "note value 162"),
            ("AB r4\nA [r4 [>>>>>>>>a]2]3", 2, 16, "note value 150"),
            ("A c193", 1, 3, "note length value 0"),
            ("A c_>>>>>>>>>a", 1, 14, "note value 162"),
            ("A /* \u{65e5} */ o0 c", 1, 14, "note value -3"),
        ] {
            let original = read_reader_source(input, Cursor::new(source)).unwrap();
            let document = parse_source(input, &original).unwrap();
            let error = compile_document(input, &document).unwrap_err().to_string();
            assert!(
                error.starts_with(&format!(
                    "songs/unsaved.mml:{line}:{column}: error: compile error: {message}"
                )),
                "{source}: {error}"
            );
        }
        let source = format!("A {}", "r4 ".repeat(65_536));
        let document = parse_source(input, &source).unwrap();
        let error = compile_document(input, &document).unwrap_err().to_string();
        assert!(error.starts_with("songs/unsaved.mml: error: compile error:"));
    }

    #[test]
    fn playback_diagnostics_fall_back_without_guessing_source_locations() {
        use soundlog::mdx::convert::{MdxConvertError, MdxPlaybackCheckError};
        let input = Path::new("songs/check.mml");
        for (source, track, command_index) in [
            ("A @42 c4", None, None),
            ("A r4", Some(0), Some(1)),
            ("AP r4", Some(0), Some(0)),
            ("A r4", Some(16), Some(0)),
        ] {
            let parsed = parse_source(input, source).unwrap();
            let compiled = compile_document(input, &parsed).unwrap();
            let error = MdxPlaybackCheckError::Conversion {
                error: MdxConvertError::MissingTone { voice: 42 },
                track,
                command_index,
            };
            let expected = format!("songs/check.mml: error: {error}");
            assert_eq!(
                playback_error(input, compiled.source(), compiled.source_map(), error).to_string(),
                expected
            );
        }
        let error = MdxPlaybackCheckError::LimitExceeded {
            resource: "tick",
            track: None,
            command_index: None,
        };
        let parsed = parse_source(input, "A @42 c4").unwrap();
        let compiled = compile_document(input, &parsed).unwrap();
        assert_eq!(
            playback_error(input, compiled.source(), compiled.source_map(), error).to_string(),
            "songs/check.mml: error: playback check incomplete: tick limit exceeded"
        );
    }

    #[test]
    fn formats_parse_errors_as_editor_diagnostics() {
        let input = Path::new("songs/broken.mml");

        let syntax_error = parse_source(input, "A c4\nB ]").unwrap_err();
        assert!(
            syntax_error.to_string().starts_with(
                "songs/broken.mml:2:3: error: MML syntax error at line 2, columns 3-4"
            )
        );
        assert!(syntax_error.to_string().contains("  B ]\n    ^"));

        let grammar_error = parse_source(input, "A z").unwrap_err();
        assert!(
            grammar_error.to_string().starts_with(
                "songs/broken.mml:1:3: error: MML syntax error at line 1, columns 3-4"
            )
        );

        let value_error = parse_source(input, "A c4\nB t5000").unwrap_err();
        assert!(
            value_error
                .to_string()
                .starts_with("songs/broken.mml:2:4: error: MML value error at line 2, columns 4-8")
        );
    }

    #[test]
    fn parses_stdin_and_keeps_the_input_path_in_diagnostics() {
        let input = Path::new("songs/unsaved.mml");

        assert!(
            parse_source(
                input,
                &read_reader_source(input, Cursor::new("A c4\nB t5000")).unwrap()
            )
            .is_err_and(|error| {
                error
                    .to_string()
                    .starts_with("songs/unsaved.mml:2:4: error: MML value error")
            })
        );
        let source = read_reader_source(input, Cursor::new("A c4")).unwrap();
        assert!(parse_source(input, &source).is_ok());
    }
}

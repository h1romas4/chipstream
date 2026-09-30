use std::fs;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

pub(super) fn parse_input(input: &Path) -> Result<mmlx::mdx::MmlDocument> {
    parse_input_with_source(input).map(|(document, _)| document)
}

pub(super) fn parse_input_with_source(input: &Path) -> Result<(mmlx::mdx::MmlDocument, String)> {
    let source = fs::read_to_string(input)
        .with_context(|| format!("failed to read MML input: {}", input.display()))?;
    let document = parse_source(input, &source)?;
    Ok((document, source))
}

pub(super) fn parse_reader_with_source<R: Read>(
    input: &Path,
    mut reader: R,
) -> Result<(mmlx::mdx::MmlDocument, String)> {
    let mut source = String::new();
    reader.read_to_string(&mut source).with_context(|| {
        format!(
            "failed to read MML input from stdin for {}",
            input.display()
        )
    })?;
    let document = parse_source(input, &source)?;
    Ok((document, source))
}

pub(super) fn playback_error(
    input: &Path,
    source: &str,
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
        .and_then(|(track, command)| mmlx::mdx::locate_source_command(source, track, command));
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

pub(super) fn parse_source(input: &Path, source: &str) -> Result<mmlx::mdx::MmlDocument> {
    mmlx::mdx::parse(source).map_err(|error| {
        let location = match &error {
            mmlx::mdx::ParseError::Syntax(message) => syntax_error_location(message),
            mmlx::mdx::ParseError::InvalidValue {
                line_number,
                column,
                ..
            } => Some((*line_number, *column)),
        };
        let message = error.to_string();
        match location {
            Some((line, column)) => {
                anyhow!("{}:{line}:{column}: error: {message}", input.display())
            }
            None => anyhow!("{}: error: {message}", input.display()),
        }
    })
}

fn syntax_error_location(message: &str) -> Option<(usize, usize)> {
    let coordinates = message.lines().next()?.split_once(" at line ")?.1;
    let (line, columns) = coordinates.split_once(", columns ")?;
    let start = columns.split_once('-')?.0;
    Some((line.parse().ok()?, start.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::Path;

    use super::*;

    #[test]
    fn playback_diagnostics_fall_back_without_guessing_source_locations() {
        use soundlog::mdx::convert::{MdxConvertError, MdxPlaybackCheckError};
        let input = Path::new("songs/check.mml");
        for (source, track, command_index) in [
            ("A @42 c4", None, None),
            ("A r4", Some(0), Some(1)),
            ("A ?", Some(0), Some(0)),
        ] {
            let error = MdxPlaybackCheckError::Conversion {
                error: MdxConvertError::MissingTone { voice: 42 },
                track,
                command_index,
            };
            let expected = format!("songs/check.mml: error: {error}");
            assert_eq!(playback_error(input, source, error).to_string(), expected);
        }
        let error = MdxPlaybackCheckError::LimitExceeded {
            resource: "tick",
            track: None,
            command_index: None,
        };
        assert_eq!(
            playback_error(input, "A @42 c4", error).to_string(),
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
            parse_reader_with_source(input, Cursor::new("A c4\nB t5000")).is_err_and(|error| {
                error
                    .to_string()
                    .starts_with("songs/unsaved.mml:2:4: error: MML value error")
            })
        );
        assert!(parse_reader_with_source(input, Cursor::new("A c4")).is_ok());
    }
}

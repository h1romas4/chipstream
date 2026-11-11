//! Expand single-line MML macros and map expanded ranges to the original source.

use super::*;

/// Maximum expanded size, unless the original source is already larger.
const MAX_EXPANDED_BYTES: usize = 16 * 1024 * 1024;

/// Corresponding ranges in the expanded and original source.
struct Segment {
    /// Range in the expanded source.
    expanded: Span,
    /// Original source range or macro call replaced by this segment.
    original: Span,
    /// Whether the segment contains a macro body rather than original text.
    replacement: bool,
}

/// Expanded source and the segments needed to recover original locations.
pub(super) struct Expansion {
    /// Source text after removing definitions and expanding calls.
    pub text: String,
    /// Nonempty segments in expanded source order.
    segments: Vec<Segment>,
}

impl Expansion {
    /// Append a nonempty segment and record its original range.
    fn append(&mut self, text: &str, original: Span, replacement: bool) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        self.segments.push(Segment {
            expanded: Span::new(start, self.text.len()).expect("expanded source is bounded"),
            original,
            replacement,
        });
    }

    /// Map an expanded range to the original text or enclosing macro call.
    fn remap_span(&self, span: Span) -> Span {
        let segment_at = |offset: usize, end: bool| {
            let index = self.segments.partition_point(|segment| {
                if end {
                    segment.expanded.end() < offset
                } else {
                    segment.expanded.end() <= offset
                }
            });
            self.segments.get(index).or_else(|| self.segments.last())
        };
        let point = |offset: usize, end: bool| {
            let segment = segment_at(offset, end);
            let Some(segment) = segment else { return 0 };
            if segment.replacement {
                if end {
                    segment.original.end()
                } else {
                    segment.original.start()
                }
            } else {
                segment.original.start()
                    + offset
                        .saturating_sub(segment.expanded.start())
                        .min(segment.original.end() - segment.original.start())
            }
        };
        let start = point(span.start(), false);
        let end = if span.start() == span.end() {
            let segment = segment_at(span.start(), false)
                .filter(|segment| span.start() < segment.expanded.end());
            if segment.is_some_and(|segment| segment.replacement) {
                segment.unwrap().original.end()
            } else {
                start
            }
        } else {
            point(span.end(), true)
        };
        Span::new(start, end.max(start)).expect("original source is bounded")
    }

    /// Rebuild an error with positions in the original source.
    pub(super) fn remap_error(&self, error: ParseFailure, source: &str) -> ParseFailure {
        match error {
            ParseFailure::Syntax(error) => {
                let span = match error.location {
                    pest::error::InputLocation::Pos(offset) => Span::new(offset, offset).unwrap(),
                    pest::error::InputLocation::Span((start, end)) => {
                        Span::new(start, end).unwrap()
                    }
                };
                let span = self.remap_span(span);
                let error = if span.start() == span.end() {
                    pest::error::Error::new_from_pos(
                        error.variant,
                        pest::Position::new(source, span.start()).unwrap(),
                    )
                } else {
                    pest::error::Error::new_from_span(
                        error.variant,
                        pest::Span::new(source, span.start(), span.end()).unwrap(),
                    )
                };
                ParseFailure::Syntax(Box::new(error))
            }
            ParseFailure::Macro { span, message } => ParseFailure::Macro {
                span: self.remap_span(span),
                message,
            },
            ParseFailure::RepeatDepthExceeded { span } => ParseFailure::RepeatDepthExceeded {
                span: self.remap_span(span),
            },
            ParseFailure::Repeat {
                span,
                message,
                expected,
            } => ParseFailure::Repeat {
                span: self.remap_span(span),
                message,
                expected,
            },
            ParseFailure::Value {
                span,
                command,
                value,
                min,
                max,
            } => ParseFailure::Value {
                span: self.remap_span(span),
                command,
                value,
                min,
                max,
            },
            ParseFailure::Overflow { span, command } => ParseFailure::Overflow {
                span: self.remap_span(span),
                command,
            },
            ParseFailure::SourceTooLarge => ParseFailure::SourceTooLarge,
        }
    }

    /// Map captured syntax ranges back to the original source in place.
    pub(super) fn remap_sources(&self, sources: &mut MmlSourceMap) {
        /// Map command ranges, including nested repeat bodies.
        fn remap_commands(expansion: &Expansion, commands: &mut [CommandSource]) {
            for command in commands {
                command.position = expansion.remap_span(command.position);
                command.end_position = command.end_position.map(|span| expansion.remap_span(span));
                remap_commands(expansion, &mut command.body);
            }
        }
        for commands in &mut sources.tracks {
            remap_commands(self, commands);
        }
        #[cfg(feature = "source-map")]
        {
            sources.title = sources.title.map(|span| self.remap_span(span));
            sources.pcm_file = sources.pcm_file.map(|span| self.remap_span(span));
            for voice in &mut sources.voices {
                voice.span = self.remap_span(voice.span);
                voice.number = self.remap_span(voice.number);
                for span in &mut voice.parameters {
                    *span = self.remap_span(*span);
                }
            }
            for argument in &mut sources.arguments {
                argument.command = self.remap_span(argument.command);
                argument.span = self.remap_span(argument.span);
            }
        }
    }
}

/// Validate definitions and expand calls, or return `None` when no edits are needed.
pub(super) fn expand(source: &str) -> Result<Option<Expansion>, ParseFailure> {
    if !source.contains('$') {
        return Ok(None);
    }
    let document = MmlParser::parse(Rule::document, source)
        .map_err(|error| ParseFailure::Syntax(Box::new(error)))?
        .next()
        .expect("document rule must produce a pair");
    let mut definitions: [Option<Span>; 26] = [None; 26];
    let mut edits: Vec<(Span, String, bool)> = Vec::new();
    let mut expanded_size = source.len();

    for line in document
        .into_inner()
        .filter_map(|line| line.into_inner().next())
    {
        match line.as_rule() {
            Rule::macro_definition => {
                let definition_span =
                    Span::new(line.as_span().start(), line.as_span().end()).unwrap();
                let line_start = source[..definition_span.start()]
                    .rfind('\n')
                    .map_or(0, |offset| offset + 1);
                if !source[line_start..definition_span.start()]
                    .bytes()
                    .all(|byte| matches!(byte, b' ' | b'\t'))
                {
                    return Err(ParseFailure::Macro {
                        span: definition_span,
                        message: "macro definitions must be on their own line".to_owned(),
                    });
                }
                let mut children = line.into_inner();
                let name = children.next().expect("macro has a name");
                let body = children.next().expect("macro has a body");
                let name_span = source_span(&name);
                let index = usize::from(name.as_str().as_bytes()[0] - b'A');
                if definitions[index].is_some() {
                    return Err(ParseFailure::Macro {
                        span: name_span,
                        message: format!("macro ${} is already defined", name.as_str()),
                    });
                }
                let body_span = source_span(&body);
                validate_body(source, body_span)?;
                definitions[index] = Some(body_span);
                let blank = source[definition_span.range()]
                    .bytes()
                    .map(|byte| match byte {
                        b'\r' | b'\n' => byte,
                        _ => b' ',
                    })
                    .collect::<Vec<_>>();
                edits.push((definition_span, String::from_utf8(blank).unwrap(), false));
            }
            Rule::track => {
                for call in line
                    .into_inner()
                    .filter(|pair| pair.as_rule() == Rule::macro_call)
                {
                    let span = source_span(&call);
                    let name = call.as_str().as_bytes()[1];
                    let Some(body) = definitions[usize::from(name - b'A')] else {
                        return Err(ParseFailure::Macro {
                            span,
                            message: format!("macro ${} is not defined", char::from(name)),
                        });
                    };
                    let text = &source[body.range()];
                    expanded_size = expanded_size
                        .saturating_sub(span.end() - span.start())
                        .saturating_add(text.len());
                    if expanded_size > MAX_EXPANDED_BYTES.max(source.len())
                        || expanded_size > u32::MAX as usize
                    {
                        return Err(ParseFailure::Macro {
                            span,
                            message: "expanded source exceeds the size limit".to_owned(),
                        });
                    }
                    edits.push((span, text.to_owned(), true));
                }
            }
            _ => {}
        }
    }
    if edits.is_empty() {
        return Ok(None);
    }
    let mut expansion = Expansion {
        text: String::with_capacity(expanded_size),
        segments: Vec::new(),
    };
    let mut offset = 0;
    for (span, text, replacement) in edits {
        expansion.append(
            &source[offset..span.start()],
            Span::new(offset, span.start()).unwrap(),
            false,
        );
        expansion.append(&text, span, replacement);
        offset = span.end();
    }
    expansion.append(
        &source[offset..],
        Span::new(offset, source.len()).unwrap(),
        false,
    );
    Ok(Some(expansion))
}

/// Validate a complete, non-nested macro body and report errors in the original source.
fn validate_body(source: &str, body: Span) -> Result<(), ParseFailure> {
    let text = &source[body.range()];
    let fragment = MmlParser::parse(Rule::macro_fragment, text)
        .map_err(|error| ParseFailure::Syntax(Box::new(error)))
        .and_then(|mut pairs| {
            let mut commands = Vec::new();
            for pair in pairs.next().unwrap().into_inner() {
                match pair.as_rule() {
                    Rule::EOI | Rule::block_comment => continue,
                    Rule::macro_call => {
                        return Err(ParseFailure::Macro {
                            span: source_span(&pair),
                            message: "nested macro references are not allowed".to_owned(),
                        });
                    }
                    _ => commands.push(parse_parsed_command(pair, false)?),
                }
            }
            assemble_repeats(commands, text, None).map(|_| ())
        });
    fragment.map_err(|error| {
        let mapping = Expansion {
            text: text.to_owned(),
            segments: vec![Segment {
                expanded: Span::new(0, text.len()).unwrap(),
                original: body,
                replacement: false,
            }],
        };
        mapping.remap_error(error, source)
    })
}

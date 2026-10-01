use mmlx::mdx::{self, MmlCommand, MmlLength};
use soundlog::mdx::command::{MdxCommand, MdxLfoWaveform};
use soundlog::mdx::document::MdxDocument;

mod allocation {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    use super::*;

    struct CountingAllocator;

    thread_local! {
        static USAGE: Cell<Option<(usize, usize)>> = const { Cell::new(None) };
    }

    fn record(size: usize) {
        let _ = USAGE.try_with(|usage| {
            if let Some((count, bytes)) = usage.get() {
                usage.set(Some((count + 1, bytes + size)));
            }
        });
    }

    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            record(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            record(size);
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn measure<ResultValue>(
        operation: impl FnOnce() -> ResultValue,
    ) -> (ResultValue, (usize, usize)) {
        USAGE.with(|usage| usage.set(Some((0, 0))));
        let result = operation();
        let usage = USAGE.with(|usage| usage.take().unwrap());
        (result, usage)
    }

    #[test]
    fn ordinary_api_allocation_profile() {
        let source = "AB @42 [c%600& r%300]2\nP c4_>d4";
        let (document, ordinary) = measure(|| compile_source(source));
        assert!(ordinary.0 > 0);
        println!(
            "ordinary parse+compile: {} allocations/reallocations, {} requested bytes",
            ordinary.0, ordinary.1
        );
        println!(
            "AST sizes: document={}, track={}, command={}",
            std::mem::size_of::<mdx::MmlDocument>(),
            std::mem::size_of::<mdx::MmlTrack>(),
            std::mem::size_of::<MmlCommand>()
        );
        #[cfg(feature = "source-map")]
        {
            let (mapped, mapped_usage) = measure(|| {
                let parsed = mdx::frontend::parse(source).unwrap();
                mdx::frontend::compile(&parsed).unwrap()
            });
            assert_eq!(
                mapped.document().to_bytes().unwrap(),
                document.to_bytes().unwrap()
            );
            assert!(mapped_usage.0 > ordinary.0);
            assert!(mapped_usage.1 > ordinary.1);
        }
        #[cfg(not(feature = "source-map"))]
        drop(document);
    }
}

#[cfg(feature = "source-map")]
mod frontend {
    use super::*;
    use mmlx::diagnostic::Severity;
    use mmlx::mdx::frontend::{self, MdxLocation};

    #[test]
    fn mapped_compilation_preserves_bytes_and_final_command_coordinates() {
        for source in [
            "AB @42 c%600& r%300 d4",
            "AB @42 [r4\nA [c4]2\nAB d4]3\n",
            "A @42 c4_d4 e4",
            "A @42 c4_>d4 e4",
            "A L r4 c4\nA d4",
            "AP @42 c%600& d4",
            "P [c4 [d4]2]3\nA @42 e4",
            "A r4 ! c4",
            "A /* \u{65e5} */ c+4 d8.",
        ] {
            let parsed = frontend::parse(source).unwrap();
            assert_eq!(parsed.source().text().as_ptr(), source.as_ptr());
            assert_eq!(parsed.ast(), &mdx::parse(source).unwrap());
            let compiled = frontend::compile(&parsed).unwrap();
            drop(parsed);
            assert_eq!(
                compiled.document().to_bytes().unwrap(),
                compile_source(source).to_bytes().unwrap()
            );
            for (track, commands) in compiled.document().tracks.iter().enumerate() {
                for (index, command) in commands.iter().enumerate() {
                    let span = compiled
                        .source_map()
                        .get(&MdxLocation::TrackCommand { track, index });
                    if let Some(span) = span {
                        assert!(compiled.source().position(span.start()).is_some());
                        assert!(span.text(source).is_some());
                    }
                    if matches!(command, MdxCommand::Note(_)) {
                        let text = span.unwrap().text(source).unwrap();
                        assert!(text.starts_with(['a', 'b', 'c', 'd', 'e', 'f', 'g']));
                    }
                }
            }
            assert_eq!(
                compiled.source_map().get(&MdxLocation::TrackCommand {
                    track: 16,
                    index: 0
                }),
                None
            );
            assert_eq!(
                compiled.source_map().get(&MdxLocation::TrackCommand {
                    track: 0,
                    index: usize::MAX
                }),
                None
            );
        }
    }

    #[test]
    fn finalized_command_positions_cover_multiline_tracks_and_synthetic_commands() {
        for (source, track, index, expected) in [
            ("AB @42 [r4\nA [c4]2\nAB d4]3\n", 0, 4, Some((2, 4))),
            ("AB @42 [r4\nA [c4]2\nAB d4]3\n", 1, 3, Some((3, 4))),
            ("A r4", 0, 1, None),
            ("A r4", 16, 0, None),
            ("A r4", 0, usize::MAX, None),
            ("AP @42 c4", 0, 0, None),
            ("AP @42 c4", 0, 2, Some((1, 8))),
            ("AP @42 c4", 8, 1, Some((1, 8))),
            ("A L r4 c4", 0, 2, None),
        ] {
            let parsed = frontend::parse(source).unwrap();
            let compiled = frontend::compile(&parsed).unwrap();
            let position = compiled
                .source_map()
                .get(&MdxLocation::TrackCommand { track, index })
                .and_then(|span| compiled.source().position(span.start()));
            assert_eq!(
                position.map(|position| (position.line_number, position.column)),
                expected,
                "{source}: track {track}, index {index}"
            );
        }
    }

    #[test]
    fn syntax_map_captures_metadata_voice_parameters_and_nested_commands() {
        let source = "#title \"\u{65e5}\"\n#pcmfile \"test.pdx\"\n@1={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47}\nAB [c+4\nA [d8]2\nAB r4]3\n";
        let parsed = frontend::parse(source).unwrap();
        let syntax = parsed.syntax();
        assert!(
            syntax
                .title()
                .unwrap()
                .text(source)
                .unwrap()
                .starts_with("#title")
        );
        assert!(
            syntax
                .pcm_file()
                .unwrap()
                .text(source)
                .unwrap()
                .starts_with("#pcmfile")
        );
        assert_eq!(syntax.voices()[0].number.text(source), Some("1"));
        assert_eq!(syntax.voices()[0].parameters.len(), 47);
        assert_eq!(syntax.voices()[0].parameters[46].text(source), Some("47"));
        let repeat = &syntax.commands(0).unwrap()[0];
        assert_eq!(repeat.position.text(source), Some("["));
        assert_eq!(repeat.end_position.unwrap().text(source), Some("]3"));
        assert!(repeat.full_span().text(source).unwrap().starts_with("[c+4"));
        assert!(repeat.full_span().text(source).unwrap().ends_with("]3"));
        assert_eq!(repeat.body[0].position.text(source), Some("c+4"));
        assert_eq!(repeat.body[1].body[0].position.text(source), Some("d8"));
        assert_eq!(syntax.commands(1).unwrap()[0].body.len(), 2);
        assert!(syntax.arguments().iter().any(|argument| {
            argument.command.text(source) == Some("c+4") && argument.span.text(source) == Some("4")
        }));
        let compiled = frontend::compile(&parsed).unwrap();
        assert_eq!(
            compiled.source_map().get(&MdxLocation::Title),
            syntax.title()
        );
        assert_eq!(
            compiled.source_map().get(&MdxLocation::PcmFile),
            syntax.pcm_file()
        );
        assert_eq!(
            compiled.source_map().get(&MdxLocation::Tone { index: 0 }),
            Some(syntax.voices()[0].span)
        );
        assert_eq!(
            compiled.source_map().get(&MdxLocation::Tone { index: 1 }),
            None
        );
    }

    #[test]
    fn diagnostics_are_structured_and_do_not_copy_source_lines() {
        for (source, code, token) in [
            ("A /* \u{65e5} */ o9", "mmlx.invalid-value", "9"),
            ("A c4^/* \u{65e5} */999", "mmlx.invalid-value", "999"),
            (
                "A t999999999999999999999",
                "mmlx.integer-overflow",
                "999999999999999999999",
            ),
            ("A [c4", "mmlx.repeat", "["),
            ("A c4]2", "mmlx.repeat", "]2"),
        ] {
            let error = frontend::parse(source).unwrap_err();
            assert_eq!(error.code, code);
            assert_eq!(error.severity, Severity::Error);
            assert_eq!(error.span.unwrap().text(source), Some(token));
            assert!(!error.message.contains("/*"));
            assert_eq!(error.to_string(), error.message);
        }
        let syntax = frontend::parse("A ?").unwrap_err();
        assert_eq!(syntax.code, "mmlx.syntax");
        assert_eq!(syntax.span.unwrap().start(), 2);
        let eof = frontend::parse("#title \"unfinished").unwrap_err();
        assert!(eof.span.unwrap().end() <= "#title \"unfinished".len());
        for (source, token) in [
            ("A /* \u{65e5} */ o0 c", "c"),
            ("A l193 c", "c"),
            ("A c_>>>>>>>>>a", "a"),
            ("A [r4 [>>>>>>>>a]2]3", "a"),
        ] {
            let parsed = frontend::parse(source).unwrap();
            let error = frontend::compile(&parsed).unwrap_err();
            assert_eq!(error.code, "mmlx.mdx.invalid-value");
            assert_eq!(error.span.unwrap().text(source), Some(token));
        }
    }

    #[test]
    fn synthetic_commands_and_missing_metadata_have_no_source() {
        let parsed = frontend::parse("AP c4").unwrap();
        let compiled = frontend::compile(&parsed).unwrap();
        assert!(matches!(
            compiled.document().tracks[0][0],
            MdxCommand::PcmMode(_)
        ));
        assert_eq!(
            compiled
                .source_map()
                .get(&MdxLocation::TrackCommand { track: 0, index: 0 }),
            None
        );
        assert_eq!(
            compiled
                .source_map()
                .get(&MdxLocation::TrackCommand { track: 1, index: 0 }),
            None
        );
        assert_eq!(compiled.source_map().get(&MdxLocation::Title), None);
        assert_eq!(compiled.source_map().get(&MdxLocation::PcmFile), None);
        let mapped = compiled
            .source_map()
            .get(&MdxLocation::TrackCommand { track: 0, index: 1 })
            .unwrap();
        assert_eq!(mapped.text("AP c4"), Some("c4"));
        let parsed = frontend::parse("A L c4").unwrap();
        let compiled = frontend::compile(&parsed).unwrap();
        let index = compiled.document().tracks[0].len() - 1;
        assert_eq!(
            compiled
                .source_map()
                .get(&MdxLocation::TrackCommand { track: 0, index }),
            None
        );
        let document = compiled.into_document();
        assert_eq!(
            document.to_bytes().unwrap(),
            compile_source("A L c4").to_bytes().unwrap()
        );
    }

    #[test]
    fn command_ranges_exclude_implicitly_consumed_trailing_trivia() {
        let source = "A c4 /* \u{65e5} */ d8. r [e4] /* end */ f4";
        let parsed = frontend::parse(source).unwrap();
        let commands = parsed.syntax().commands(0).unwrap();
        assert_eq!(commands[0].position.text(source), Some("c4"));
        assert_eq!(commands[1].position.text(source), Some("d8."));
        assert_eq!(commands[2].position.text(source), Some("r"));
        assert_eq!(commands[3].end_position.unwrap().text(source), Some("]"));
        let compiled = frontend::compile(&parsed).unwrap();
        let span = compiled
            .source_map()
            .get(&MdxLocation::TrackCommand { track: 0, index: 0 })
            .unwrap();
        assert_eq!(span.text(source), Some("c4"));
    }

    #[test]
    fn builder_errors_do_not_report_the_last_successful_command() {
        let source = format!("A {}", "r4 ".repeat(65_536));
        let parsed = frontend::parse(&source).unwrap();
        let Err(error) = frontend::compile(&parsed) else {
            panic!("track offsets exceed the MDX limit");
        };
        assert_eq!(error.code, "mmlx.mdx.builder");
        assert_eq!(error.span, None);
    }
}

fn compile_source(source: &str) -> MdxDocument {
    mdx::compile(&mdx::parse(source).unwrap()).unwrap()
}

fn note_values(track: &[MdxCommand]) -> Vec<(u8, u16)> {
    track
        .iter()
        .filter_map(|command| match command {
            MdxCommand::Note(note) => Some((note.note, note.length)),
            _ => None,
        })
        .collect()
}

#[test]
fn parses_readable_mml_ast() {
    let source = r#"#title "Readable MXDRV parser fixture"
#pcmfile "fixture.pdx"
@1={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47}
A c
F f
P F4
"#;
    let document = mdx::parse(source).unwrap();

    assert_eq!(
        document.title.as_deref(),
        Some("Readable MXDRV parser fixture")
    );
    assert_eq!(document.pcm_file.as_deref(), Some("fixture.pdx"));
    assert_eq!(document.voices.len(), 1);
    assert_eq!(document.voices[0].values.len(), 47);
    assert_eq!(document.tracks.len(), 3);
    assert_eq!(document.tracks[0].channel, 'A');
    assert_eq!(document.tracks[2].channel, 'P');
    assert!(matches!(
        document.tracks[2].commands.as_slice(),
        [MmlCommand::PcmFrequency(4)]
    ));
}

#[test]
fn parses_compact_mml_ast() {
    let source = "#title \"Compact MXDRV parser fixture\"\n@1={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47}\nA @1 cdefgab\nP F4\n";
    let document = mdx::parse(source).unwrap();

    assert_eq!(
        document.title.as_deref(),
        Some("Compact MXDRV parser fixture")
    );
    assert_eq!(document.voices.len(), 1);
    assert_eq!(document.tracks.len(), 2);
    assert_eq!(document.tracks[0].channel, 'A');
    assert_eq!(document.tracks[0].commands.len(), 8);
    assert!(matches!(
        document.tracks[1].commands.as_slice(),
        [MmlCommand::PcmFrequency(4)]
    ));
}

#[test]
fn merges_repeated_channel_lines() {
    let document = mdx::parse("A c\nB d\nA e\n").unwrap();

    assert_eq!(document.tracks[0].commands.len(), 2);
    assert_eq!(document.tracks[0].channel, 'A');
    assert_eq!(document.tracks[1].channel, 'B');
}

#[test]
fn keeps_interleaved_channel_lines_out_of_repeat_body() {
    let document = mdx::parse("A [abc\nB c\nA def]\n").unwrap();

    assert_eq!(document.tracks.len(), 2);
    assert_eq!(document.tracks[0].channel, 'A');
    assert_eq!(document.tracks[1].channel, 'B');
    assert!(matches!(
        document.tracks[0].commands.as_slice(),
        [MmlCommand::Repeat { body, .. }]
            if body.len() == 6
    ));
    assert!(matches!(
        document.tracks[1].commands.as_slice(),
        [MmlCommand::Note { name: 'c', .. }]
    ));
}

#[test]
fn parses_dotted_lengths() {
    let document = mdx::parse("A c4. r8.. l16. d\n").unwrap();

    assert_eq!(
        document.tracks[0].commands,
        vec![
            MmlCommand::ExtendedNote {
                name: 'c',
                accidental: None,
                length: MmlLength::Sum(vec![MmlLength::Denominator(4), MmlLength::Denominator(8),]),
            },
            MmlCommand::ExtendedRest(MmlLength::Sum(vec![
                MmlLength::Denominator(8),
                MmlLength::Denominator(16),
                MmlLength::Denominator(32),
            ])),
            MmlCommand::DefaultLength(MmlLength::Sum(vec![
                MmlLength::Denominator(16),
                MmlLength::Denominator(32),
            ])),
            MmlCommand::Note {
                name: 'd',
                accidental: None,
                length: None,
            },
        ]
    );
}

#[test]
fn parses_repeats_across_track_lines() {
    let document = mdx::parse("A [c\nA d]3\n").unwrap();

    assert!(matches!(
        document.tracks[0].commands.as_slice(),
        [MmlCommand::Repeat { body, count }]
            if *count == 3 && body.len() == 2
    ));
}

#[test]
fn defaults_repeat_count_to_two() {
    let document = mdx::parse("A [c d]\n").unwrap();

    assert!(matches!(
        document.tracks[0].commands.as_slice(),
        [MmlCommand::Repeat { count: 2, .. }]
    ));
}

#[test]
fn compiles_multiple_standard_channels_ast() {
    let compiled = compile_source("AB c\n");

    assert!(matches!(compiled.tracks[0][0], MdxCommand::Note(_)));
    assert!(matches!(compiled.tracks[1][0], MdxCommand::Note(_)));
}

#[test]
fn compiles_extended_channels_ast() {
    let compiled = compile_source("A c\nH d\nP n0,8\nQ n1,8\nW n2,8\n");

    assert_eq!(compiled.tracks.len(), 16);
    assert!(matches!(compiled.tracks[0][1], MdxCommand::Note(_)));
    assert!(matches!(compiled.tracks[7][0], MdxCommand::Note(_)));
    assert!(matches!(compiled.tracks[8][0], MdxCommand::Note(_)));
    assert!(matches!(compiled.tracks[9][0], MdxCommand::Note(_)));
    assert!(matches!(compiled.tracks[15][0], MdxCommand::Note(_)));
}

#[test]
fn compiles_bare_default_length_reset_ast() {
    let commands = compile_source("A l8 l c\n").tracks[0].clone();

    assert!(matches!(commands[0], MdxCommand::Note(note) if note.length == 24));
}

#[test]
fn compiles_metadata_and_tone_ast() {
    let source = "#title \"Test\"\n#pcmfile \"test.pdx\"\n@1={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47}\nA @1 o4 l4 c4\n";
    let compiled = compile_source(source);

    assert_eq!(compiled.header.title, "Test");
    assert_eq!(compiled.header.pdx_name.as_deref(), Some("test.pdx"));
    assert_eq!(compiled.tracks.len(), 9);
    assert_eq!(compiled.tone_bank.tones.len(), 1);
    assert!(matches!(
        compiled.tracks[0][0],
        MdxCommand::VoiceOrPcmBank(_)
    ));
    assert!(matches!(compiled.tracks[0][1], MdxCommand::Note(_)));
}

#[test]
fn compiles_pitch_lfo_ast() {
    let compiled = compile_source("A MPON MP2,2,16 MP2,64,-24 MPOF\n");
    let commands = &compiled.tracks[0];

    assert!(matches!(commands[0], MdxCommand::PitchLfo(_)));
    assert!(matches!(
        commands[1],
        MdxCommand::PitchLfo(soundlog::mdx::command::MdxPitchLfo::Configure {
            waveform: MdxLfoWaveform::Triangle,
            frequency: 4,
            amplitude: 2048,
        })
    ));
    assert!(matches!(
        commands[2],
        MdxCommand::PitchLfo(soundlog::mdx::command::MdxPitchLfo::Configure {
            frequency: 128,
            amplitude: -96,
            ..
        })
    ));
    assert!(matches!(commands[3], MdxCommand::PitchLfo(_)));
}

#[test]
fn compiles_extended_pitch_lfo_waveform() {
    let compiled = compile_source("A MP6,72,-1\n");

    assert!(matches!(
        compiled.tracks[0][0],
        MdxCommand::PitchLfo(soundlog::mdx::command::MdxPitchLfo::Configure {
            waveform: soundlog::mdx::command::MdxLfoWaveform::Unknown(6),
            frequency: 144,
            amplitude: -3,
        })
    ));
}

#[test]
fn compiles_volume_lfo_ast() {
    let compiled = compile_source("A MAON MA2,4,1 MA2,4,2 MAOF\n");
    let commands = &compiled.tracks[0];

    assert!(matches!(commands[0], MdxCommand::VolumeLfo(_)));
    assert!(matches!(
        commands[1],
        MdxCommand::VolumeLfo(soundlog::mdx::command::MdxVolumeLfo::Configure {
            waveform: MdxLfoWaveform::Triangle,
            frequency: 8,
            amplitude: 16,
        })
    ));
    assert!(matches!(
        commands[2],
        MdxCommand::VolumeLfo(soundlog::mdx::command::MdxVolumeLfo::Configure {
            amplitude: 32,
            ..
        })
    ));
}

#[test]
fn compiles_negative_volume_lfo_amplitude() {
    let compiled = compile_source("A MA2,56,-1\n");

    assert!(matches!(
        compiled.tracks[0][0],
        MdxCommand::VolumeLfo(soundlog::mdx::command::MdxVolumeLfo::Configure {
            amplitude: 65_520,
            ..
        })
    ));
}

#[test]
fn compiles_opm_lfo_ast() {
    let compiled = compile_source("A MHON MH1,3,3,4,5,6,1 MHOF\n");
    let commands = &compiled.tracks[0];

    assert!(matches!(commands[0], MdxCommand::OpmLfo(_)));
    assert!(matches!(
        commands[1],
        MdxCommand::OpmLfo(soundlog::mdx::command::MdxOpmLfo::Configure {
            control: 65,
            lfrq: 3,
            pmd: 131,
            amd: 4,
            pms_ams: 86,
        })
    ));
    assert!(matches!(commands[2], MdxCommand::OpmLfo(_)));

    let wide_sensitivity = compile_source("A MH0,3,3,4,8,15,0\n").tracks[0].clone();
    assert!(matches!(
        wide_sensitivity[0],
        MdxCommand::OpmLfo(soundlog::mdx::command::MdxOpmLfo::Configure { pms_ams: 143, .. })
    ));

    let wide_pmd = compile_source("A MH0,3,255,4,8,15,0\n").tracks[0].clone();
    assert!(matches!(
        wide_pmd[0],
        MdxCommand::OpmLfo(soundlog::mdx::command::MdxOpmLfo::Configure { pmd: 255, .. })
    ));
}

#[test]
fn compiles_lfo_delay_ast() {
    let commands = compile_source("A MD100 c MD50 d MD0 e\n").tracks[0].clone();

    assert!(matches!(commands[1], MdxCommand::Note(_)));
    assert!(matches!(commands[0], MdxCommand::LfoDelay(_)));
    assert!(matches!(commands[2], MdxCommand::LfoDelay(_)));
    assert!(matches!(commands[4], MdxCommand::LfoDelay(_)));
}

#[test]
fn compiles_note_lengths_ast() {
    let commands = compile_source("A @0 l192 c l%96 d l2^8 e\n").tracks[0].clone();
    assert_eq!(
        note_values(&commands),
        vec![(173, 1), (175, 96), (177, 120)]
    );
}

#[test]
fn compiles_dotted_numeric_note_lengths_ast() {
    let commands = compile_source("A n0,8. n1,4.\n").tracks[0].clone();

    assert_eq!(note_values(&commands), vec![(128, 36), (129, 72)]);
}

#[test]
fn compiles_compound_lengths_with_dots_and_subtraction_ast() {
    let commands = compile_source("A c4^8. r4~8 n0,4^8.\n").tracks[0].clone();

    assert_eq!(note_values(&commands), vec![(173, 84), (128, 84)]);
    assert!(matches!(
        commands[1],
        MdxCommand::Rest(rest) if rest.ticks == 24
    ));
}

#[test]
fn compiles_named_notes_ast() {
    let commands = compile_source("A @0 cdefgab > cdefgab > c\nB @0 >> c < bagfedc\n").tracks;

    assert_eq!(note_values(&commands[0]).first(), Some(&(173, 48)));
    assert_eq!(note_values(&commands[0]).last(), Some(&(197, 48)));
    assert_eq!(note_values(&commands[1])[0], (197, 48));
}

#[test]
fn compiles_octave_commands_ast() {
    let commands = compile_source("A @0 o0 d+ o1 c > c o4 < c\n").tracks[0].clone();
    assert_eq!(
        note_values(&commands),
        vec![(128, 48), (137, 48), (149, 48), (161, 48)]
    );
}

#[test]
fn compiles_portamento_ast() {
    let commands = compile_source("A c d_e f\nC g b_a > c\n").tracks;

    assert!(matches!(
        commands[0][1],
        MdxCommand::Portamento(command) if command.offset == 682
    ));
    assert_eq!(
        note_values(&commands[0]),
        vec![(173, 48), (175, 48), (178, 48)]
    );
    assert!(matches!(
        commands[2][1],
        MdxCommand::Portamento(command) if command.offset == -682
    ));

    let short_note = compile_source("A d8_e\n").tracks[0].clone();
    assert!(matches!(
        short_note[0],
        MdxCommand::Portamento(command) if command.offset == 1365
    ));
}

#[test]
fn compiles_portamento_after_legato_source_note_ast() {
    let commands = compile_source("A c8&c8_d4\n").tracks[0].clone();

    assert!(commands.iter().any(|command| {
        matches!(
            command,
            MdxCommand::Portamento(command) if command.offset == 1365
        )
    }));
}

#[test]
fn compiles_staccato_ast() {
    let commands = compile_source("A q1 c q8 c @q1 d @q192 d\n").tracks[0].clone();
    let gates: Vec<u8> = commands
        .iter()
        .filter_map(|command| match command {
            MdxCommand::Gate(gate) => Some(gate.value),
            _ => None,
        })
        .collect();

    assert_eq!(gates, vec![1, 8, 255, 64]);
}

#[test]
fn compiles_tempo_ast() {
    let commands = compile_source("A t19 t30 t4882 c\n").tracks[0].clone();
    let tempos: Vec<u8> = commands
        .iter()
        .filter_map(|command| match command {
            MdxCommand::Tempo(tempo) => Some(tempo.value),
            _ => None,
        })
        .collect();

    assert_eq!(tempos, vec![0, 94, 255]);
}

#[test]
fn compiles_voice_selection_ast() {
    let compiled = compile_source(
        "@0={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47}\n@255={1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47}\nA @0 @255\n",
    );

    assert_eq!(compiled.tone_bank.tones.len(), 2);
    assert_eq!(compiled.tone_bank.tones[0].voice_number, 0);
    assert_eq!(compiled.tone_bank.tones[1].voice_number, 255);
    assert!(matches!(
        compiled.tracks[0][0],
        MdxCommand::VoiceOrPcmBank(_)
    ));
    assert!(matches!(
        compiled.tracks[0][1],
        MdxCommand::VoiceOrPcmBank(_)
    ));
}

#[test]
fn compiles_volume_ast() {
    let commands = compile_source("A v15 c v0 c @v0 d @v100 d\n").tracks[0].clone();
    let volumes: Vec<u8> = commands
        .iter()
        .filter_map(|command| match command {
            MdxCommand::Volume(volume) => Some(volume.value),
            _ => None,
        })
        .collect();

    assert_eq!(volumes, vec![15, 0, 255, 155]);
}

#[test]
fn compiles_control_commands_ast() {
    let commands = compile_source("A @t200 p1 v8 ( ) D-12 y1,2 k3 w4 S0 W F4\n").tracks[0].clone();

    assert!(matches!(commands[0], MdxCommand::Tempo(tempo) if tempo.value == 200));
    assert!(matches!(
        commands[1],
        MdxCommand::Pan(soundlog::mdx::command::MdxPan::Right)
    ));
    assert!(matches!(commands[2], MdxCommand::Volume(volume) if volume.value == 8));
    assert!(matches!(commands[3], MdxCommand::VolumeDown(_)));
    assert!(matches!(commands[4], MdxCommand::VolumeUp(_)));
    assert!(matches!(commands[5], MdxCommand::Detune(command) if command.offset == -12));
    assert!(matches!(
        commands[6],
        MdxCommand::OpmRegisterWrite(command) if command.register == 1 && command.value == 2
    ));
    assert!(matches!(commands[7], MdxCommand::KeyOnDelay(command) if command.value == 3));
    assert!(matches!(
        commands[8],
        MdxCommand::AdpcmOrNoiseFrequency(command) if command.value == 132
    ));
    assert!(matches!(commands[9], MdxCommand::SyncSend(command) if command.value == 0));
    assert!(matches!(commands[10], MdxCommand::SyncWait(_)));
    assert!(matches!(
        commands[11],
        MdxCommand::AdpcmOrNoiseFrequency(command) if command.value == 4
    ));
}

#[test]
fn compiles_noise_and_pcm_frequency_ast() {
    let commands = compile_source("A w0 w31 F0 F7\n").tracks[0].clone();
    let frequencies: Vec<u8> = commands
        .iter()
        .filter_map(|command| match command {
            MdxCommand::AdpcmOrNoiseFrequency(command) => Some(command.value),
            _ => None,
        })
        .collect();

    assert_eq!(frequencies, vec![0x80, 0x9f, 0, 7]);
}

#[test]
fn compiles_extended_pcm_frequency_modes_without_truncation() {
    let commands = compile_source("P F8 F12\n").tracks[8].clone();
    let frequencies: Vec<u8> = commands
        .iter()
        .filter_map(|command| match command {
            MdxCommand::AdpcmOrNoiseFrequency(command) => Some(command.value),
            _ => None,
        })
        .collect();

    assert_eq!(frequencies, vec![8, 12]);
}

#[test]
fn compiles_multidigit_sync_channel_ast() {
    let commands = compile_source("A S10\n").tracks[0].clone();

    assert!(matches!(
        commands[0],
        MdxCommand::SyncSend(command) if command.value == 10
    ));
}

#[test]
fn compiles_repeat_and_loop_commands_ast() {
    let commands = compile_source("A L [c / d]3\n").tracks[0].clone();

    assert!(matches!(commands[0], MdxCommand::LoopStart(command) if command.count == 3));
    assert!(matches!(commands[1], MdxCommand::Note(_)));
    assert!(matches!(
        commands[2],
        MdxCommand::LoopEscape(command) if command.offset == 3
    ));
    assert!(matches!(commands[3], MdxCommand::Note(_)));
    assert!(matches!(
        commands[4],
        MdxCommand::LoopEnd(command) if command.offset == -10
    ));
    assert!(matches!(
        commands[5],
        MdxCommand::EndOfTrackLoop(command) if command.opcode == 0xf1 && command.offset == -16
    ));
}

#[test]
fn compiles_repeat_with_default_count_ast() {
    let commands = compile_source("A [c d]\n").tracks[0].clone();

    assert!(matches!(commands[0], MdxCommand::LoopStart(command) if command.count == 2));
    assert!(matches!(commands[1], MdxCommand::Note(_)));
    assert!(matches!(commands[2], MdxCommand::Note(_)));
    assert!(matches!(
        commands[3],
        MdxCommand::LoopEnd(command) if command.offset == -7
    ));
}

#[test]
fn compiles_loop_point_ast() {
    let commands = compile_source("A c L d\n").tracks[0].clone();

    assert!(matches!(commands[0], MdxCommand::Note(_)));
    assert!(matches!(commands[1], MdxCommand::Note(_)));
    assert!(matches!(
        commands[2],
        MdxCommand::EndOfTrackLoop(command) if command.opcode == 0xf1 && command.offset == -5
    ));

    let empty_loop = compile_source("A L\n").tracks[0].clone();
    assert!(matches!(
        empty_loop.as_slice(),
        [MdxCommand::EndOfTrackLoop(command)] if command.opcode == 0xf1 && command.offset == -3
    ));
}

#[test]
fn uses_reference_loop_offset_for_pcm_track() {
    let commands = compile_source("P v8@0 L n57,8.F2 n67,%108 F4 [r1]5\n").tracks[8].clone();

    let Some(MdxCommand::EndOfTrackLoop(command)) = commands.last() else {
        panic!("expected an end-of-track loop command");
    };
    assert_eq!(command.offset, -19);
}

#[test]
fn uses_legacy_loop_offset_for_pcm_rest_start() {
    let commands = compile_source("P F4 L r1 [r1]7 n33,4\n").tracks[8].clone();

    let Some(MdxCommand::EndOfTrackLoop(command)) = commands.last() else {
        panic!("expected an end-of-track loop command");
    };
    assert_eq!(command.offset, -13);
}

#[test]
fn compiles_numeric_rest_and_legato_ast() {
    let commands = compile_source("A n12,8 r%24 c & d\n").tracks[0].clone();

    assert!(matches!(commands[0], MdxCommand::Note(note) if note.note == 140 && note.length == 24));
    assert!(matches!(commands[1], MdxCommand::Rest(rest) if rest.ticks == 24));
    assert!(matches!(commands[2], MdxCommand::KeyOffDisable(_)));
    assert!(matches!(commands[3], MdxCommand::Note(note) if note.note == 173));
    assert!(matches!(commands[4], MdxCommand::Note(note) if note.note == 175));
}

#[test]
fn ignores_commands_after_ignore_marker_ast() {
    let commands = compile_source("A c ! d\n").tracks[0].clone();

    assert_eq!(note_values(&commands), vec![(173, 48)]);
}

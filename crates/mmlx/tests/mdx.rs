use mmlx::mdx::{self, MmlCommand};
use soundlog::mdx::command::{MdxCommand, MdxLfoWaveform};
use soundlog::mdx::document::MdxDocument;

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
    let compiled = compile_source("A MPON MP2,2,16 MPOF\n");
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
    assert!(matches!(commands[2], MdxCommand::PitchLfo(_)));
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
            amplitude: 32,
        })
    ));
    assert!(matches!(
        commands[2],
        MdxCommand::VolumeLfo(soundlog::mdx::command::MdxVolumeLfo::Configure {
            amplitude: 64,
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

    assert_eq!(volumes, vec![15, 0, 128, 228]);
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
fn compiles_repeat_and_loop_commands_ast() {
    let commands = compile_source("A L [c / d]3\n").tracks[0].clone();

    assert!(matches!(commands[0], MdxCommand::LoopStart(command) if command.count == 3));
    assert!(matches!(commands[1], MdxCommand::Note(_)));
    assert!(matches!(commands[2], MdxCommand::LoopEscape(_)));
    assert!(matches!(commands[3], MdxCommand::Note(_)));
    assert!(matches!(commands[4], MdxCommand::LoopEnd(_)));
    assert!(matches!(
        commands[5],
        MdxCommand::EndOfTrackLoop(command) if command.opcode == 0xf1 && command.offset == -16
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

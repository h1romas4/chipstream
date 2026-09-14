#![cfg(feature = "mdx")]

use std::fs;
use std::io::ErrorKind;
use std::iter;
use std::path::Path;

use soundlog::mdx::command::{
    MdxAdpcmOrNoiseFrequency, MdxCommand, MdxEndOfTrack, MdxExtended2Command, MdxExtendedCommand,
    MdxLoopStart, MdxNote, MdxOpmLfo, MdxOpmRegisterWrite, MdxPan, MdxPitchLfo, MdxRawCommand,
    MdxRelativeOffset, MdxRest, MdxVoiceOrPcmBank, MdxVolume, MdxVolumeDown, MdxVolumeLfo,
    MdxVolumeUp,
};
use soundlog::mdx::convert::{MdxToVgmOptions, to_vgm_document, to_vgm_stream_generator};
use soundlog::mdx::document::{MdxBuilder, MdxDocument};
use soundlog::mdx::header::parse_mdx_header;
use soundlog::mdx::lz::encode as encode_lz;
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::parser::parse_mdx_command;
use soundlog::mdx::pcm::Pcm8aFormat;
use soundlog::mdx::pdx::{PdxBuilder, PdxDocument};
use soundlog::mdx::tone::{MdxOperator, MdxTone};
use soundlog::vgm::command::{VgmCommand, WaitSamples};
use soundlog::vgm::stream::StreamResult;
use soundlog::vgm::{VgmCallbackStream, VgmStream};

#[test]
fn mdx_rest_uses_one_based_tick_length() {
    let rest = MdxRest::new(1).expect("one tick rest should be valid");
    assert_eq!(rest.opcode(), 0x00);

    let rest = MdxRest::new(0x80).expect("maximum encoded rest should be valid");
    assert_eq!(rest.opcode(), 0x7f);
    assert!(MdxRest::new(0).is_none());
    assert!(MdxRest::new(0x81).is_none());
}

#[test]
fn mdx_note_preserves_note_and_encodes_length() {
    let note = MdxNote::new(0x80, 1).expect("minimum note should be valid");
    assert_eq!(note.note, 0x80);
    assert_eq!(note.length_byte(), 0);

    let note = MdxNote::new(0xdf, 0x100).expect("maximum note should be valid");
    assert_eq!(note.length_byte(), 0xff);
    assert!(MdxNote::new(0x7f, 1).is_none());
    assert!(MdxNote::new(0xe0, 1).is_none());
}

#[test]
fn mdx_raw_command_rejects_note_and_rest_ranges() {
    assert!(MdxRawCommand::new(0xdf).is_none());
    assert!(MdxRawCommand::new(0xe0).is_some());
}

#[test]
fn mdx_command_round_trips_opm_register_write() {
    let bytes = [0xfe, 0x08, 0x7f];
    let (command, length) = parse_mdx_command(&bytes, 0).expect("parse command");

    assert_eq!(length, bytes.len());
    assert_eq!(
        command,
        MdxCommand::OpmRegisterWrite(MdxOpmRegisterWrite {
            register: 0x08,
            value: 0x7f,
        })
    );
    assert_eq!(command.to_mdx_bytes().unwrap(), bytes);
}

#[test]
fn mdx_f1_zero_is_end_of_track() {
    let (command, length) = parse_mdx_command(&[0xf1, 0x00], 0).expect("parse end command");

    assert_eq!(command, MdxCommand::EndOfTrack(MdxEndOfTrack));
    assert_eq!(command.to_mdx_bytes().unwrap(), [0xf1, 0x00]);
    assert_eq!(length, 2);
}

#[test]
fn mdx_pitch_lfo_configuration_uses_big_endian_words() {
    let bytes = [0xec, 0x02, 0x12, 0x34, 0xff, 0xfe];
    let (command, length) = parse_mdx_command(&bytes, 0).expect("parse pitch LFO");

    assert_eq!(length, bytes.len());
    assert_eq!(
        command,
        MdxCommand::PitchLfo(MdxPitchLfo::Configure {
            waveform: 0x02,
            frequency: 0x1234,
            amplitude: -2,
        })
    );
    assert_eq!(command.to_mdx_bytes().unwrap(), bytes);
}

#[test]
fn mdx_lfo_enable_forms_are_two_bytes() {
    let cases = [
        (
            [0xea, 0x81],
            MdxCommand::OpmLfo(MdxOpmLfo::SetEnabled { enabled: true }),
        ),
        (
            [0xeb, 0x80],
            MdxCommand::VolumeLfo(MdxVolumeLfo::SetEnabled { enabled: false }),
        ),
        (
            [0xec, 0x81],
            MdxCommand::PitchLfo(MdxPitchLfo::SetEnabled { enabled: true }),
        ),
    ];

    for (bytes, expected) in cases {
        let (command, length) = parse_mdx_command(&bytes, 0).expect("parse LFO state");
        assert_eq!(length, 2);
        assert_eq!(command, expected);
        assert_eq!(command.to_mdx_bytes().unwrap(), bytes);
    }
}

#[test]
fn mdx_opm_lfo_configuration_preserves_all_operands() {
    let bytes = [0xea, 0x40, 0x12, 0x34, 0x56, 0x78];
    let (command, length) = parse_mdx_command(&bytes, 0).expect("parse OPM LFO");

    assert_eq!(length, bytes.len());
    assert_eq!(
        command,
        MdxCommand::OpmLfo(MdxOpmLfo::Configure {
            control: 0x40,
            lfrq: 0x12,
            pmd: 0x34,
            amd: 0x56,
            pms_ams: 0x78,
        })
    );
    assert_eq!(command.to_mdx_bytes().unwrap(), bytes);
}

#[test]
fn mdx_extended_commands_parse_known_subcommands() {
    let e6 = [0xe6, 0x01, 0x12, 0x34];
    let (command, length) = parse_mdx_command(&e6, 0).expect("parse extended2 command");
    assert_eq!(length, e6.len());
    assert_eq!(
        command,
        MdxCommand::Extended2(MdxExtended2Command::RelativeDetune { value: 0x1234 })
    );
    assert_eq!(command.to_mdx_bytes().unwrap(), e6);

    let e7 = [0xe7, 0x02, 1, 2, 3, 4, 5, 6];
    let (command, length) = parse_mdx_command(&e7, 0).expect("parse extended command");
    assert_eq!(length, e7.len());
    assert_eq!(
        command,
        MdxCommand::Extended(MdxExtendedCommand::Pcm8DirectDrive {
            data: [1, 2, 3, 4, 5, 6],
        })
    );
    assert_eq!(command.to_mdx_bytes().unwrap(), e7);
}

#[test]
fn mdx_known_commands_round_trip_as_a_stream() {
    let source = [
        0x00, // rest
        0x80, 0x01, // note
        0xe8, // PCM mode
        0xe9, 0x12, // LFO delay
        0xea, 0x40, 0x12, 0x34, 0x56, 0x78, // OPM LFO
        0xeb, 0x01, 0x00, 0x02, 0x00, // volume LFO
        0xec, 0x02, 0x00, 0x10, 0xff, 0xfe, // pitch LFO
        0xed, 0x03, // ADPCM/noise frequency
        0xee, // sync wait
        0xef, 0x01, // sync send
        0xf0, 0x02, // key-on delay
        0xf1, 0x00, // end of track
        0xf2, 0xff, 0xfe, // portamento
        0xf3, 0x00, 0x02, // detune
        0xf4, 0x00, 0x04, // loop escape
        0xf5, 0xff, 0xfc, // loop end
        0xf6, 0x03, 0x00, // loop start
        0xf7, // key-off disable
        0xf8, 0x7f, // gate
        0xf9, // volume up
        0xfa, // volume down
        0xfb, 0x20, // volume
        0xfc, 0x01, // pan
        0xfd, 0x02, // voice/PCM bank
        0xfe, 0x08, 0x7f, // OPM register write
        0xff, 0x06, // tempo
        0xe6, 0x00, // extended2 error
        0xe6, 0x01, 0x12, 0x34, // relative detune
        0xe6, 0x02, 0xfe, // transpose
        0xe6, 0x03, 0x02, // relative transpose
        0xe7, 0x00, // extended error
        0xe7, 0x01, 0x10, // fadeout
        0xe7, 0x02, 1, 2, 3, 4, 5, 6, // PCM8 direct drive
        0xe7, 0x03, 0x01, // key-off
        0xe7, 0x04, 0x02, // channel control
        0xe7, 0x05, 0x03, // add note length
        0xe7, 0x06, 0x04, // set flag
        0xe7, 0x0a, 0x05, // known one-byte payload
        0xf1, 0x00,
    ];

    let mut offset = 0;
    let mut rebuilt = Vec::new();
    while offset < source.len() {
        let (command, length) = parse_mdx_command(&source, offset).expect("parse MDX command");
        assert!(length > 0);
        rebuilt.extend(command.to_mdx_bytes().expect("serialize MDX command"));
        offset += length;
    }

    assert_eq!(offset, source.len());
    assert_eq!(rebuilt, source);
}

#[test]
fn mdx_header_round_trips_a_nine_track_file() {
    let mut bytes = b"TITLE\r\n\x1aexample.pdx\0".to_vec();
    bytes.extend_from_slice(&0x0024u16.to_be_bytes());
    bytes.extend_from_slice(&0x0040u16.to_be_bytes());
    bytes.extend_from_slice(&0xffffu16.to_be_bytes());
    bytes.extend_from_slice(&[0x00, 0x10, 0x00, 0x20, 0x00, 0x30, 0x00, 0x40]);
    bytes.extend_from_slice(&[0x00, 0x50, 0x00, 0x60, 0x00, 0x70]);

    let (header, base_offset) = parse_mdx_header(&bytes).expect("parse MDX header");

    assert_eq!(base_offset, 20);
    assert_eq!(header.title, "TITLE");
    assert_eq!(header.title_raw_bytes, b"TITLE");
    assert_eq!(header.pdx_name.as_deref(), Some("example.pdx"));
    assert_eq!(
        header.pdx_name_raw_bytes.as_deref(),
        Some(&b"example.pdx"[..])
    );
    assert_eq!(header.tone_data_offset, 0x0024);
    assert_eq!(header.track_count(), 9);
    assert_eq!(header.track_offsets[0], Some(0x0040));
    assert_eq!(header.track_offsets[1], None);
    assert_eq!(header.tone_data_position(), Some(0x0038));
    assert_eq!(header.track_position(0), Some(0x0054));
    assert_eq!(header.to_bytes(), bytes);
}

#[test]
fn mdx_header_detects_sixteen_tracks_from_pcm_mode_marker() {
    let mut bytes = b"TITLE\r\n\x1a\0".to_vec();
    bytes.extend_from_slice(&0x0024u16.to_be_bytes());
    bytes.extend_from_slice(&0x0040u16.to_be_bytes());
    bytes.extend(iter::repeat_n(0x00, 30));
    bytes.resize(9 + 0x0040 + 1, 0);
    bytes[9 + 0x0040] = 0xe8;

    let (header, _) = parse_mdx_header(&bytes).expect("parse MDX header");

    assert_eq!(header.track_count(), 16);
    assert_eq!(header.track_offsets.len(), 16);
}

#[test]
fn mdx_header_rejects_missing_delimiters() {
    assert!(parse_mdx_header(b"TITLE").is_err());
    assert!(parse_mdx_header(b"TITLE\r\n\x1aPDX").is_err());
}

#[test]
fn mdx_document_parses_tracks_and_builds_absolute_sourcemap() {
    let mut bytes = b"TITLE\r\n\x1a\0".to_vec();
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0x0014u16.to_be_bytes());
    bytes.extend(iter::repeat_n(0xffffu16, 8).flat_map(u16::to_be_bytes));
    bytes.extend_from_slice(&[0x00, 0xf1, 0x00]);

    let document = MdxDocument::parse(&bytes).expect("parse MDX document");

    assert_eq!(document.tracks.len(), 9);
    assert_eq!(document.tracks[0].len(), 2);
    assert!(document.tracks[1].is_empty());
    assert_eq!(document.sourcemap()[0], vec![(29, 1), (30, 2)]);
}

#[test]
fn mdx_track_ends_at_next_track_offset_without_end_command() {
    let mut bytes = b"TITLE\r\n\x1a\0".to_vec();
    bytes.extend_from_slice(&0u16.to_be_bytes());
    bytes.extend_from_slice(&0x0014u16.to_be_bytes());
    bytes.extend_from_slice(&0x0015u16.to_be_bytes());
    bytes.extend(iter::repeat_n(0xffffu16, 7).flat_map(u16::to_be_bytes));
    bytes.extend_from_slice(&[0x00, 0xf1, 0x00]);

    let document = MdxDocument::parse(&bytes).expect("parse boundary-terminated tracks");

    assert_eq!(
        document.tracks[0],
        vec![MdxCommand::Rest(MdxRest { ticks: 1 })]
    );
    assert_eq!(
        document.tracks[1],
        vec![MdxCommand::EndOfTrack(MdxEndOfTrack)]
    );
    assert_eq!(document.to_bytes(), bytes);
}

#[test]
fn mdx_builder_finalizes_tracks_and_serializes_them() {
    let mut builder = MdxBuilder::new();
    builder
        .set_title_bytes(b"BUILT".to_vec())
        .set_pdx_name_bytes(Some(b"example.pdx".to_vec()))
        .add_mdx_command(0, MdxCommand::Rest(MdxRest::new(1).unwrap()))
        .add_mdx_command(1, MdxCommand::VolumeUp(MdxVolumeUp));

    let document = builder.finalize();
    assert!(matches!(
        document.tracks[0].last(),
        Some(MdxCommand::EndOfTrack(_))
    ));
    assert!(matches!(
        document.tracks[1].last(),
        Some(MdxCommand::EndOfTrack(_))
    ));

    let bytes = document.to_bytes();
    let reparsed = MdxDocument::parse(&bytes).expect("parse serialized MDX document");
    assert_eq!(reparsed.tracks, document.tracks);
    assert_eq!(reparsed.header.title, "BUILT");
    assert_eq!(reparsed.header.pdx_name.as_deref(), Some("example.pdx"));
}

#[test]
fn mdx_builder_encodes_string_metadata_as_shift_jis() {
    let mut builder = MdxBuilder::new();
    builder.set_title("テスト").set_pdx_name(Some("音色.pdx"));

    let document = builder.finalize();
    let reparsed = MdxDocument::parse(&document.to_bytes()).expect("parse serialized metadata");

    assert_eq!(reparsed.header.title, "テスト");
    assert_eq!(reparsed.header.pdx_name.as_deref(), Some("音色.pdx"));
}

#[test]
fn mdx_builder_serializes_tone_bank_before_tracks() {
    let tone = MdxTone {
        voice_number: 3,
        con: 1,
        fl: 4,
        op: 0x0f,
        operators: [MdxOperator {
            ar: 0,
            dr: 0,
            sr: 0,
            rr: 0,
            sl: 0,
            ol: 0x10,
            ks: 0,
            ml: 0,
            dt1: 1,
            dt2: 0,
            ame: 0,
        }; 4],
    };
    let mut builder = MdxBuilder::new();
    builder
        .append_tone(tone.clone())
        .add_mdx_command(0, MdxVolumeUp);

    let document = builder.finalize();
    let reparsed = MdxDocument::parse(&document.to_bytes()).expect("parse built tone bank");

    assert_eq!(reparsed.tone_bank.tones, vec![tone]);
    assert_eq!(reparsed.tracks, document.tracks);
}

#[test]
fn mdx_tone_definition_matches_mml_parameter_order() {
    let tone = MdxTone {
        voice_number: 1,
        con: 2,
        fl: 7,
        op: 15,
        operators: [
            MdxOperator {
                ar: 28,
                dr: 4,
                sr: 0,
                rr: 5,
                sl: 1,
                ol: 37,
                ks: 2,
                ml: 1,
                dt1: 7,
                dt2: 0,
                ame: 0,
            },
            MdxOperator {
                ar: 22,
                dr: 9,
                sr: 1,
                rr: 2,
                sl: 1,
                ol: 47,
                ks: 2,
                ml: 12,
                dt1: 0,
                dt2: 0,
                ame: 0,
            },
            MdxOperator {
                ar: 29,
                dr: 4,
                sr: 3,
                rr: 6,
                sl: 1,
                ol: 37,
                ks: 1,
                ml: 3,
                dt1: 3,
                dt2: 0,
                ame: 0,
            },
            MdxOperator {
                ar: 15,
                dr: 7,
                sr: 0,
                rr: 5,
                sl: 10,
                ol: 0,
                ks: 2,
                ml: 1,
                dt1: 0,
                dt2: 0,
                ame: 1,
            },
        ],
    };

    let expected = [
        0x01, 0x3a, 0x0f, // voice, CON/FL, OP
        0x71, 0x0c, 0x33, 0x01, // DT1/ML
        0x25, 0x2f, 0x25, 0x00, // OL
        0x9c, 0x96, 0x5d, 0x8f, // KS/AR
        0x04, 0x09, 0x04, 0x87, // AME/DR
        0x00, 0x01, 0x03, 0x00, // DT2/SR
        0x15, 0x12, 0x16, 0xa5, // SL/RR
    ];

    assert_eq!(tone.to_bytes(), expected);
    assert_eq!(MdxTone::from_bytes(&expected), Some(tone));
}

#[test]
fn mdx_document_parses_lz_compressed_body() {
    let mut builder = MdxBuilder::new();
    builder
        .set_title_bytes(b"COMPRESSED".to_vec())
        .add_mdx_command(0, MdxCommand::VolumeUp(MdxVolumeUp));
    let original = builder.finalize().to_bytes();
    let (_, body_start) = parse_mdx_header(&original).expect("parse original header");

    let mut compressed = original[..body_start].to_vec();
    compressed.extend_from_slice(&[0x7f, 0xff, 0xff, 0x4c]);
    compressed.extend_from_slice(&encode_lz(&original[body_start..]));

    let document = MdxDocument::parse(&compressed).expect("parse compressed MDX");
    assert_eq!(document.header.title, "COMPRESSED");
    assert_eq!(document.to_bytes(), original);
}

#[test]
fn mdx_builder_can_enable_lz_compression() {
    let mut builder = MdxBuilder::new();
    builder
        .set_title_bytes(b"BUILDER LZ".to_vec())
        .set_lz_compressed(true)
        .add_mdx_command(0, MdxCommand::VolumeUp(MdxVolumeUp));
    let document = builder.finalize();
    let compressed = document.to_bytes();

    assert!(
        compressed
            .windows(4)
            .any(|window| window == [0x7f, 0xff, 0xff, 0x4c])
    );
    let reparsed = MdxDocument::parse(&compressed).expect("parse builder-compressed MDX");
    assert_eq!(reparsed.header.title, "BUILDER LZ");
    assert_eq!(reparsed.tracks, document.tracks);
    assert!(
        !reparsed
            .to_bytes()
            .windows(4)
            .any(|window| window == [0x7f, 0xff, 0xff, 0x4c])
    );
}

#[test]
#[ignore = "requires fixture files under assets/mdx"]
fn pdx_document_parses_and_round_trips_fixtures() {
    let asset_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mdx");
    let mut paths = fs::read_dir(&asset_dir)
        .expect("read PDX asset directory")
        .map(|entry| entry.expect("read PDX asset directory entry").path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("pdx"))
        })
        .collect::<Vec<_>>();
    paths.sort();

    assert!(!paths.is_empty(), "PDX fixtures should be present");
    for path in paths {
        let bytes = fs::read(&path).expect("read PDX fixture");
        let document = PdxDocument::parse(&bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error:?}", path.display()));
        assert_eq!(
            document.to_bytes(),
            bytes,
            "round-trip failed for {}",
            path.display()
        );
        let first_sample = document.banks.iter().enumerate().find_map(|(bank, data)| {
            data.entries
                .iter()
                .enumerate()
                .find_map(|(note, sample)| sample.map(|_| (bank, note)))
        });
        let Some((bank, note)) = first_sample else {
            panic!("fixture has no valid samples: {}", path.display());
        };
        assert!(document.sample_bytes(bank, note).is_some());

        let rebuilt = PdxBuilder::from_document(&document)
            .unwrap_or_else(|error| panic!("build {}: {error:?}", path.display()))
            .finalize();
        let reparsed = PdxDocument::parse(&rebuilt.to_bytes())
            .unwrap_or_else(|error| panic!("reparse rebuilt {}: {error:?}", path.display()));
        assert_eq!(reparsed.banks.len(), document.banks.len());
        for bank in 0..document.banks.len() {
            for note in 0..96 {
                assert_eq!(
                    reparsed.sample_bytes(bank, note),
                    document.sample_bytes(bank, note),
                    "sample round-trip failed for {} bank {} note {}",
                    path.display(),
                    bank,
                    note
                );
            }
        }
    }
}

#[test]
fn pdx_document_round_trips_lz_compressed_data() {
    let mut decoded = vec![0u8; 0x300 + 8 + 4];
    decoded[0..4].copy_from_slice(&0x308u32.to_be_bytes());
    decoded[4..8].copy_from_slice(&4u32.to_be_bytes());
    decoded[0x308..].copy_from_slice(b"PCM1");

    let mut compressed = vec![0x7f, 0xff, 0xff, 0x4c];
    compressed.extend_from_slice(&encode_lz(&decoded));
    let document = PdxDocument::parse(&compressed).expect("parse compressed PDX");

    assert!(document.is_compressed());
    assert_eq!(document.to_bytes(), compressed);
    assert_eq!(document.sample_bytes(0, 0), Some(&b"PCM1"[..]));
}

#[test]
fn pdx_document_owned_parse_round_trips_without_retaining_input() {
    let mut decoded = vec![0u8; 0x300 + 8 + 4];
    decoded[0..4].copy_from_slice(&0x308u32.to_be_bytes());
    decoded[4..8].copy_from_slice(&4u32.to_be_bytes());
    decoded[0x308..].copy_from_slice(b"PCM1");

    let mut compressed = vec![0x7f, 0xff, 0xff, 0x4c];
    compressed.extend_from_slice(&encode_lz(&decoded));
    let document = PdxDocument::parse_owned(compressed.clone()).expect("parse owned PDX");

    assert_eq!(document.to_bytes(), compressed);
    assert_eq!(document.decoded_bytes(), decoded);
    assert_eq!(document.sample_bytes(0, 0), Some(&b"PCM1"[..]));
}

#[test]
fn pdx_document_rejects_sample_ranges_outside_the_file() {
    let mut bytes = vec![0u8; 0x300 + 4];
    bytes[0..4].copy_from_slice(&0x300u32.to_be_bytes());
    bytes[4..8].copy_from_slice(&8u32.to_be_bytes());

    assert!(PdxDocument::parse(&bytes).is_err());
}

#[test]
fn pdx_builder_rebuilds_multi_bank_tables() {
    let mut builder = PdxBuilder::new();
    builder
        .set_sample(0, 2, b"BANK0".to_vec())
        .expect("set bank 0 sample")
        .set_sample(1, 3, b"BANK1".to_vec())
        .expect("set bank 1 sample")
        .set_lz_compressed(true);

    assert!(PdxBuilder::new().set_sample(32, 0, Vec::new()).is_err());
    assert!(PdxBuilder::new().set_sample(0, 96, Vec::new()).is_err());

    let document = builder.finalize();
    assert!(document.is_compressed());
    assert_eq!(document.sample_bytes(0, 2), Some(&b"BANK0"[..]));
    assert_eq!(document.sample_bytes(1, 3), Some(&b"BANK1"[..]));

    let reparsed = PdxDocument::parse(&document.to_bytes()).expect("parse built PDX");
    assert_eq!(reparsed.decoded_bytes(), document.decoded_bytes());
    assert_eq!(reparsed.sample_bytes(0, 2), Some(&b"BANK0"[..]));
    assert_eq!(reparsed.sample_bytes(1, 3), Some(&b"BANK1"[..]));
}

#[test]
fn mdx_package_keeps_mdx_and_optional_pdx_separate() {
    let mdx_bytes = b"TITLE\r\n\x1aexample.pdx\0".to_vec();
    let mut complete_mdx = mdx_bytes;
    complete_mdx.extend_from_slice(&0u16.to_be_bytes());
    complete_mdx.extend_from_slice(&0xffffu16.to_be_bytes());
    complete_mdx.extend(iter::repeat_n(0xffffu16, 8).flat_map(u16::to_be_bytes));

    let pdx_bytes = vec![0; 0x300];
    let package = MdxPackage::parse(&complete_mdx, Some(&pdx_bytes)).expect("parse package");

    assert_eq!(package.pdx_name(), Some("example.pdx"));
    assert_eq!(package.to_mdx_bytes(), complete_mdx);
    assert_eq!(package.to_pdx_bytes(), Some(pdx_bytes));

    let without_pdx = MdxPackage::parse(&complete_mdx, None).expect("parse package without PDX");
    assert!(without_pdx.pdx.is_none());
    assert_eq!(without_pdx.to_mdx_bytes(), complete_mdx);
}

#[test]
fn mdx_package_resolves_pcm_notes_to_pdx_entries() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(0, MdxRest::new(1).unwrap())
        .add_mdx_command(8, MdxVoiceOrPcmBank { value: 1 })
        .add_mdx_command(8, MdxNote::new(0x83, 1).unwrap());
    let mdx_bytes = builder.finalize().to_bytes();

    let mut pdx_bytes = vec![0u8; 0x608];
    pdx_bytes[0x318..0x31c].copy_from_slice(&0x604u32.to_be_bytes());
    pdx_bytes[0x31c..0x320].copy_from_slice(&4u32.to_be_bytes());
    pdx_bytes[0x604..].copy_from_slice(b"PCM1");

    let package = MdxPackage::parse(&mdx_bytes, Some(&pdx_bytes)).expect("parse PCM package");
    assert_eq!(
        package.pcm_references(),
        vec![soundlog::mdx::package::MdxPcmReference {
            track: 8,
            bank: 1,
            note: 3,
            sample: Some(soundlog::mdx::pdx::PdxSample {
                start: 0x604,
                size: 4,
            }),
        }]
    );

    let package_without_pdx = MdxPackage::parse(&mdx_bytes, None).expect("parse without PDX");
    let references = package_without_pdx.pcm_references();
    assert_eq!(references.len(), 1);
    assert_eq!(references[0].bank, 1);
    assert_eq!(references[0].note, 3);
    assert_eq!(references[0].sample, None);
}

#[test]
fn mdx_package_decodes_a_resolved_pcm_reference() {
    let mut builder = MdxBuilder::new();
    builder.add_mdx_command(8, MdxNote::new(0x80, 1).unwrap());
    let mdx_bytes = builder.finalize().to_bytes();

    let mut pdx_builder = PdxBuilder::new();
    pdx_builder.set_sample(0, 0, vec![0x80, 0x7f]).unwrap();
    let package = MdxPackage::parse(&mdx_bytes, Some(&pdx_builder.finalize().to_bytes()))
        .expect("parse PCM package");
    let reference = package.pcm_references().pop().expect("PCM reference");

    assert_eq!(
        package.pcm_sample_bytes(&reference),
        Some(&[0x80, 0x7f][..])
    );
    assert_eq!(
        package.decode_pcm_reference(&reference, Pcm8aFormat::Pcm8),
        Ok(Some(vec![-2048, 2032]))
    );
}

#[test]
fn mdx_converter_emits_fm_initialization_and_rest_duration() {
    let mut builder = MdxBuilder::new();
    builder.add_mdx_command(0, MdxRest::new(1).unwrap());
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let document =
        to_vgm_document(&package, &MdxToVgmOptions::default()).expect("convert basic MDX to VGM");
    assert!(document.commands.iter().any(|command| matches!(
        command,
        VgmCommand::Ym2151Write(_, spec) if spec.register == 0x01 && spec.value == 0
    )));
    assert!(document.commands.iter().any(|command| matches!(
        command,
        VgmCommand::WaitSamples(WaitSamples(value)) if *value > 0
    )));
}

#[test]
fn mdx_converter_writes_ym2151_keycode_and_key_fraction() {
    let mut builder = MdxBuilder::new();
    builder
        .append_tone(MdxTone {
            voice_number: 0,
            con: 0,
            fl: 0,
            op: 0,
            operators: [MdxOperator::default(); 4],
        })
        .add_mdx_command(0, MdxNote::new(0x8c, 1).unwrap());
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let document =
        to_vgm_document(&package, &MdxToVgmOptions::default()).expect("convert note to VGM");
    assert!(document.commands.iter().any(|command| matches!(
        command,
        VgmCommand::Ym2151Write(_, spec) if spec.register == 0x28 && spec.value == 0x10
    )));
    assert!(document.commands.iter().any(|command| matches!(
        command,
        VgmCommand::Ym2151Write(_, spec) if spec.register == 0x30 && spec.value == 0x14
    )));
}

#[test]
fn mdx_converter_reapplies_pan_register_when_voice_changes_algorithm() {
    // Register 0x20 combines pan with CON/FL, so switching to a voice with a
    // different algorithm must rewrite it even without a new Pan command.
    let mut builder = MdxBuilder::new();
    builder
        .append_tone(MdxTone {
            voice_number: 0,
            con: 3,
            fl: 0,
            op: 0x0f,
            operators: [MdxOperator::default(); 4],
        })
        .append_tone(MdxTone {
            voice_number: 1,
            con: 5,
            fl: 0,
            op: 0x0f,
            operators: [MdxOperator::default(); 4],
        })
        .add_mdx_command(
            0,
            MdxCommand::Pan(soundlog::mdx::command::MdxPan { value: 3 }),
        )
        .add_mdx_command(0, MdxVoiceOrPcmBank { value: 0 })
        .add_mdx_command(0, MdxNote::new(0x80, 1).unwrap())
        .add_mdx_command(0, MdxVoiceOrPcmBank { value: 1 })
        .add_mdx_command(0, MdxNote::new(0x80, 1).unwrap());
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let document =
        to_vgm_document(&package, &MdxToVgmOptions::default()).expect("convert notes to VGM");
    let register_0x20_writes: Vec<u8> = document
        .commands
        .iter()
        .filter_map(|command| match command {
            VgmCommand::Ym2151Write(_, spec) if spec.register == 0x20 => Some(spec.value),
            _ => None,
        })
        .collect();

    assert_eq!(
        register_0x20_writes,
        vec![0xc3, 0xc5],
        "register 0x20 must be rewritten with the new CON/FL on each voice change"
    );
}

#[test]
fn mdx_converter_remaps_fm_channel_from_raw_register_0x08_writes_under_mxdrv16y() {
    let mut builder = MdxBuilder::new();
    builder
        .append_tone(MdxTone {
            voice_number: 0,
            con: 0,
            fl: 0,
            op: 0x0f,
            operators: [MdxOperator::default(); 4],
        })
        .add_mdx_command(0, MdxVoiceOrPcmBank { value: 0 })
        .add_mdx_command(0, MdxNote::new(0x80, 1).unwrap())
        .add_mdx_command(
            0,
            MdxCommand::OpmRegisterWrite(MdxOpmRegisterWrite {
                register: 0x08,
                value: 0x05,
            }),
        )
        .add_mdx_command(0, MdxNote::new(0x80, 1).unwrap());
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let options = MdxToVgmOptions {
        mxdrv16y: true,
        ..MdxToVgmOptions::default()
    };
    let document = to_vgm_document(&package, &options).expect("convert notes to VGM");
    let register_0x08_writes: Vec<u8> = document
        .commands
        .iter()
        .filter_map(|command| match command {
            VgmCommand::Ym2151Write(_, spec) if spec.register == 0x08 => Some(spec.value),
            _ => None,
        })
        .collect();

    // First key-on on channel 0 (0x78 | 0), its natural key-off on the same
    // channel, the raw hint write (0x05) passed through verbatim, then the
    // second key-on retriggered on channel 5 (0x78 | 5) because the voice
    // was reapplied to the remapped channel, and its key-off correctly
    // using the remapped channel too.
    assert_eq!(register_0x08_writes, vec![0x78, 0x00, 0x05, 0x7d, 0x05]);
}

#[test]
fn mdx_converter_escapes_empty_infinite_loop_under_mxdrv16y() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(
            0,
            MdxLoopStart {
                count: 0,
                reserved: 0,
            },
        )
        .add_mdx_command(
            0,
            MdxCommand::LoopEnd(MdxRelativeOffset {
                opcode: 0xf5,
                offset: 0,
            }),
        )
        .add_mdx_command(0, MdxRest::new(5).unwrap());
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let default_document = to_vgm_document(&package, &MdxToVgmOptions::default())
        .expect("an empty infinite loop should not hang by default either");
    assert!(
        default_document.loop_command_index().is_some(),
        "without mxdrv16y, an empty infinite loop still becomes a native loop point"
    );

    let mxdrv16y_options = MdxToVgmOptions {
        mxdrv16y: true,
        ..MdxToVgmOptions::default()
    };
    let mxdrv16y_document =
        to_vgm_document(&package, &mxdrv16y_options).expect("mxdrv16y should escape the trap");
    assert!(
        mxdrv16y_document.loop_command_index().is_none(),
        "mxdrv16y must escape the empty loop via its own offset instead of looping"
    );
}

#[test]
fn mdx_converter_pcm_notes_do_not_emit_fm_register_writes() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(8, MdxVoiceOrPcmBank { value: 0 })
        .add_mdx_command(8, MdxAdpcmOrNoiseFrequency { value: 4 })
        .add_mdx_command(8, MdxPan { value: 1 })
        .add_mdx_command(8, MdxVolume { value: 8 })
        .add_mdx_command(8, MdxNote::new(0x80, 4).unwrap())
        .add_mdx_command(8, MdxVolumeUp)
        .add_mdx_command(8, MdxVolumeDown)
        .add_mdx_command(8, MdxRest::new(4).unwrap());
    let mut pdx_builder = PdxBuilder::new();
    pdx_builder.set_sample(0, 0, vec![0x11, 0x22]).unwrap();
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: Some(pdx_builder.finalize()),
    };

    let document = to_vgm_document(&package, &MdxToVgmOptions::default())
        .expect("a PCM-only track should convert without hanging or erroring");

    let ym2151_writes: Vec<_> = document
        .commands
        .iter()
        .filter_map(|command| match command {
            VgmCommand::Ym2151Write(_, spec) => Some((spec.register, spec.value)),
            _ => None,
        })
        .collect();
    // Only the fixed FM initialization sequence should appear; the PCM
    // track's bank/rate/pan/volume/note commands must not leak into OPM
    // registers.
    assert_eq!(
        ym2151_writes,
        vec![
            (0x38, 0),
            (0x39, 0),
            (0x3a, 0),
            (0x3b, 0),
            (0x3c, 0),
            (0x3d, 0),
            (0x3e, 0),
            (0x3f, 0),
            (0x01, 0),
            (0x0f, 0),
            (0x19, 0),
            (0x19, 0x80),
        ]
    );

    let pcm_pan_writes: Vec<_> = document
        .commands
        .iter()
        .filter_map(|command| match command {
            VgmCommand::Okim6258Write(_, spec) if spec.register == 0x02 => Some(spec.value),
            _ => None,
        })
        .collect();
    assert_eq!(pcm_pan_writes, vec![1]);
}

#[test]
fn mdx_converter_pcm_track_loop_uses_shared_loop_machinery() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(
            8,
            MdxLoopStart {
                count: 3,
                reserved: 0,
            },
        )
        .add_mdx_command(8, MdxRest::new(2).unwrap())
        .add_mdx_command(
            8,
            MdxCommand::LoopEnd(MdxRelativeOffset {
                opcode: 0xf5,
                offset: -2,
            }),
        )
        .add_mdx_command(8, MdxRest::new(1).unwrap());
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let document = to_vgm_document(&package, &MdxToVgmOptions::default())
        .expect("a finite loop on a PCM track should terminate normally");
    assert!(
        document.loop_command_index().is_none(),
        "a finite repeat block must not be mistaken for the song's native loop point"
    );
}

#[test]
fn mdx_converter_resolves_a_whole_song_backward_jump_to_a_native_vgm_loop_point() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(0, MdxRest::new(1).unwrap())
        .add_mdx_command(
            0,
            MdxCommand::Jump(MdxRelativeOffset {
                opcode: 0xf1,
                offset: -4,
            }),
        );
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let document = to_vgm_document(&package, &MdxToVgmOptions::default())
        .expect("a whole-song backward jump should not loop forever");

    assert!(
        document.loop_command_index().is_some(),
        "a backward jump should produce a native VGM loop point instead of \
         repeating internally forever"
    );
}

#[test]
fn mdx_converter_loop_count_does_not_override_nested_repeat_blocks() {
    // Track 0 repeats a 1-tick rest 4 times; track 1 repeats a 2-tick rest 2
    // times. Both total 4 ticks, so with each block's own encoded count
    // honored, both tracks finish at the same time regardless of
    // `loop_count`, which must only govern the whole-song repeat.
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(
            0,
            MdxLoopStart {
                count: 4,
                reserved: 0,
            },
        )
        .add_mdx_command(0, MdxRest::new(1).unwrap())
        .add_mdx_command(
            0,
            MdxCommand::LoopEnd(MdxRelativeOffset {
                opcode: 0xf5,
                offset: -4,
            }),
        )
        .add_mdx_command(
            1,
            MdxLoopStart {
                count: 2,
                reserved: 0,
            },
        )
        .add_mdx_command(1, MdxRest::new(2).unwrap())
        .add_mdx_command(
            1,
            MdxCommand::LoopEnd(MdxRelativeOffset {
                opcode: 0xf5,
                offset: -4,
            }),
        );
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let default_document =
        to_vgm_document(&package, &MdxToVgmOptions::default()).expect("convert with defaults");
    let capped_document = to_vgm_document(
        &package,
        &MdxToVgmOptions {
            loop_count: Some(1),
            ..MdxToVgmOptions::default()
        },
    )
    .expect("convert with loop_count capped");

    assert_eq!(
        default_document.header.total_samples, capped_document.header.total_samples,
        "loop_count must not override nested repeat block counts, or tracks \
         with different repeat counts will desync"
    );
}

#[test]
fn mdx_converter_does_not_emit_okim6258_without_pdx() {
    // Mirrors NanoDriveX's real MDX player (`src/mdx.cpp`), which gates all
    // OKIM6258 engagement purely on `pdxLoaded`, regardless of whether any
    // track references a PCM note — so even a track 8 PCM key-on must not
    // drive OKIM6258 when no PDX was supplied.
    let mut builder = MdxBuilder::new();
    builder
        .append_tone(MdxTone {
            voice_number: 0,
            con: 0,
            fl: 0,
            op: 0,
            operators: [MdxOperator::default(); 4],
        })
        .add_mdx_command(0, MdxNote::new(0x8c, 1).unwrap())
        .add_mdx_command(8, MdxNote::new(0x80, 1).unwrap());
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };
    assert_eq!(package.mdx.tracks.len(), 9);
    assert!(!package.drives_okim6258());

    let document = to_vgm_document(&package, &MdxToVgmOptions::default())
        .expect("convert MDX without a PDX to VGM");
    assert!(
        !document
            .commands
            .iter()
            .any(|command| matches!(command, VgmCommand::Okim6258Write(..))),
        "OKIM6258 must not be driven without a loaded PDX, even if a track \
         references a PCM note"
    );
}

#[test]
fn mdx_package_try_into_vgm_stream_matches_eager_conversion() {
    // The lazy `VgmStream`/`VgmCallbackStream` paths (built from
    // `to_vgm_stream_generator`) must produce the exact same command sequence
    // as the eager `to_vgm_document` path, just generated on demand as the
    // stream is iterated instead of all up front.
    let mut builder = MdxBuilder::new();
    builder
        .append_tone(MdxTone {
            voice_number: 0,
            con: 0,
            fl: 0,
            op: 0,
            operators: [MdxOperator::default(); 4],
        })
        .add_mdx_command(0, MdxNote::new(0x8c, 1).unwrap())
        .add_mdx_command(0, MdxRest::new(4).unwrap());
    let package = MdxPackage {
        mdx: builder.finalize(),
        pdx: None,
    };

    let expected_document =
        to_vgm_document(&package, &MdxToVgmOptions::default()).expect("convert eagerly");

    let expected_commands = drain_finite_stream(VgmStream::from_document(expected_document));
    let generator = to_vgm_stream_generator(package.clone(), MdxToVgmOptions::default())
        .expect("convert MDX lazily");
    let lazy_stream = VgmStream::from_generator(generator);
    let lazy_commands = drain_finite_stream(lazy_stream);
    assert_eq!(lazy_commands, expected_commands);

    // The same conversion should also work through `VgmCallbackStream`.
    // Its `Iterator::next()` returns `None` on `EndOfStream` (rather than
    // yielding it), so a plain `for` loop terminates on its own.
    let generator =
        to_vgm_stream_generator(package, MdxToVgmOptions::default()).expect("convert MDX lazily");
    let mut callback_stream = VgmCallbackStream::new(VgmStream::from_generator(generator));
    callback_stream.set_loop_count(Some(1));
    let mut callback_commands = Vec::new();
    for result in callback_stream {
        match result.expect("unexpected stream error") {
            StreamResult::Command(cmd) => callback_commands.push(cmd),
            StreamResult::EndOfStream => unreachable!("iterator stops before yielding this"),
            StreamResult::NeedsMoreData => panic!("generator-backed stream needs no data"),
        }
    }
    assert_eq!(callback_commands, expected_commands);
}

/// Drains a stream to completion, capping loop playback at one pass.
///
/// `VgmStream` never yields the trailing `EndOfData` command itself (it
/// consumes it internally to signal end-of-stream/looping), so this is the
/// comparable, filtered baseline to diff lazy vs. eager output against,
/// rather than comparing against a `VgmDocument`'s raw `.commands`.
fn drain_finite_stream(mut stream: VgmStream) -> Vec<VgmCommand> {
    stream.set_loop_count(Some(1));
    let mut commands = Vec::new();
    loop {
        match stream.next().expect("stream should not be empty") {
            Ok(StreamResult::Command(cmd)) => commands.push(cmd),
            Ok(StreamResult::EndOfStream) => break,
            Ok(StreamResult::NeedsMoreData) => panic!("generator-backed stream needs no data"),
            Err(e) => panic!("unexpected stream error: {e}"),
        }
    }
    commands
}

/// PDX sidecar filenames for the real MDX fixtures under `assets/mdx` that
/// need one to fully resolve (see also `real_mdx_and_pdx_fixtures_resolve_pcm_references`).
const REAL_FIXTURE_PDX_PAIRS: &[(&str, &str)] = &[("example.mdx", "example.pdx")];

#[test]
#[ignore = "requires fixture files under assets/mdx"]
fn real_mdx_fixtures_lazy_stream_matches_eager_conversion_with_finite_loop_count() {
    // A finite `loop_count` takes the exact same code path in both the
    // eager (`to_vgm_document`) and lazy (`to_vgm_stream_generator`) drivers
    // (see `PlaybackState::take_repeating_jump_to`'s `Some(limit)` branch,
    // which does not consult `mark_native_loop` at all), so the two are
    // logically guaranteed to produce byte-identical command sequences.
    // Verify that guarantee against every real-world fixture, not just the
    // synthetic one above, since real files exercise nested repeat blocks,
    // PCM tracks and other features the synthetic fixture does not.
    let asset_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mdx");
    let mut mdx_paths = fs::read_dir(&asset_dir)
        .expect("read assets/mdx")
        .map(|entry| entry.expect("read assets/mdx entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("mdx"))
        })
        .collect::<Vec<_>>();
    mdx_paths.sort();
    assert!(!mdx_paths.is_empty(), "expected real MDX fixtures to exist");

    let options = MdxToVgmOptions {
        loop_count: Some(2),
        ..MdxToVgmOptions::default()
    };

    for path in mdx_paths {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mdx_bytes = fs::read(&path).unwrap_or_else(|e| panic!("read example.mdx: {e}"));
        let pdx_bytes = REAL_FIXTURE_PDX_PAIRS
            .iter()
            .find(|(mdx, _)| *mdx == name)
            .map(|(_, pdx)| {
                fs::read(asset_dir.join(pdx)).unwrap_or_else(|e| panic!("read {pdx}: {e}"))
            });
        let package = MdxPackage::parse(&mdx_bytes, pdx_bytes.as_deref())
            .unwrap_or_else(|e| panic!("parse example.mdx: {e:?}"));

        let expected_document = to_vgm_document(&package, &options)
            .unwrap_or_else(|e| panic!("eager convert example.mdx: {e:?}"));
        let expected_commands = drain_finite_stream(VgmStream::from_document(expected_document));

        let generator = to_vgm_stream_generator(package, options)
            .unwrap_or_else(|e| panic!("lazy convert example.mdx: {e:?}"));
        let lazy_commands = drain_finite_stream(VgmStream::from_generator(generator));

        assert_eq!(lazy_commands, expected_commands, "mismatch for example.mdx");
    }
}

#[test]
#[ignore = "requires fixture files under assets/mdx"]
fn real_mdx_fixture_lazy_stream_prefix_matches_native_loop_document() {
    // Under the default `loop_count: None`, the eager path stops once an
    // unconditional repeat is seen a second time and records that position
    // as a native VGM loop point (`mark_native_loop: true`), while the lazy
    // path just keeps simulating the same repeat forever instead
    // (`mark_native_loop: false`, since it never retains history to loop
    // back to). Both run the identical simulation up to that point, EXCEPT
    // for the shared final tick in which the repeat is detected a second
    // time: the eager side freezes that track in place right there (so its
    // document ends mid-tick), while the lazy side's `process_commands`
    // does not pause and immediately keeps consuming whatever commands
    // follow the jump target, so it does not reproduce that exact partial
    // tick. Verified empirically (see PR discussion) to affect only the
    // trailing command(s) of the eager document, never anything earlier.
    let asset_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mdx");
    let mdx_bytes = fs::read(asset_dir.join("example.mdx")).expect("read example.mdx");
    let package = MdxPackage::parse(&mdx_bytes, None).expect("parse example.mdx");

    let expected_document =
        to_vgm_document(&package, &MdxToVgmOptions::default()).expect("eager convert example.mdx");
    assert!(
        expected_document.loop_command_index().is_some(),
        "example.mdx is expected to have a native loop point for this test to be meaningful"
    );
    let expected_commands = drain_finite_stream(VgmStream::from_document(expected_document));

    let generator = to_vgm_stream_generator(package, MdxToVgmOptions::default())
        .expect("lazy convert example.mdx");
    let mut lazy_stream = VgmStream::from_generator(generator);
    let mut lazy_commands = Vec::with_capacity(expected_commands.len());
    while lazy_commands.len() < expected_commands.len() {
        match lazy_stream
            .next()
            .expect("infinite-loop generator stream should not be empty")
        {
            Ok(StreamResult::Command(cmd)) => lazy_commands.push(cmd),
            other => panic!("unexpected result while draining lazy stream: {other:?}"),
        }
    }

    // Allow only the last handful of commands (the boundary tick described
    // above) to disagree; everything before that must match exactly.
    const MAX_TRAILING_DIVERGENCE: usize = 8;
    let common_prefix_len = expected_commands
        .iter()
        .zip(lazy_commands.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let diverging = expected_commands.len() - common_prefix_len;
    assert!(
        diverging <= MAX_TRAILING_DIVERGENCE,
        "expected only the final boundary tick to diverge (<= {MAX_TRAILING_DIVERGENCE} \
         commands), but {diverging} of {} trailing commands differ: eager={:?} lazy={:?}",
        expected_commands.len(),
        &expected_commands[common_prefix_len..],
        &lazy_commands[common_prefix_len..expected_commands.len()],
    );
}

#[test]
#[ignore = "requires fixture files under assets/mdx"]
fn real_mdx_and_pdx_fixtures_resolve_pcm_references() {
    let asset_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mdx");
    let pdx_bytes = fs::read(asset_dir.join("example.pdx")).expect("read example.pdx");
    let mut paths = fs::read_dir(&asset_dir)
        .expect("read GR fixture directory")
        .map(|entry| entry.expect("read GR fixture entry").path())
        .filter(|path| {
            path.is_file()
                && path.file_name().is_some_and(|name| {
                    name.to_string_lossy().starts_with("GR_")
                        && path
                            .extension()
                            .is_some_and(|extension| extension.eq_ignore_ascii_case("mdx"))
                })
        })
        .collect::<Vec<_>>();
    paths.sort();

    for path in paths {
        let mdx_bytes = fs::read(&path).expect("read GR MDX fixture");
        let package = MdxPackage::parse(&mdx_bytes, Some(&pdx_bytes))
            .unwrap_or_else(|error| panic!("parse {}: {error:?}", path.display()));
        assert_eq!(package.pdx_name(), Some("example.pdx"));
        let references = package.pcm_references();
        assert!(
            !references.is_empty(),
            "{} should contain PCM notes",
            path.display()
        );
        assert!(
            references
                .iter()
                .all(|reference| reference.sample.is_some())
        );
    }
}

#[test]
#[ignore = "requires fixture files under assets/mdx"]
fn parses_mdx_fixtures_and_round_trips_them() {
    let asset_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/mdx");
    let entries = match fs::read_dir(&asset_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return,
        Err(error) => panic!("read {}: {error}", asset_dir.display()),
    };
    let mut paths = entries
        .map(|entry| entry.expect("read MDX asset directory entry").path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("mdx"))
        })
        .collect::<Vec<_>>();
    paths.sort();
    if paths.is_empty() {
        return;
    }

    for path in &paths {
        let bytes = fs::read(path).expect("read MDX fixture");
        let document = MdxDocument::parse(&bytes)
            .unwrap_or_else(|error| panic!("parse {}: {error:?}", path.display()));
        let serialized = document.to_bytes();
        assert_eq!(
            serialized,
            bytes,
            "round-trip failed for {}",
            path.display()
        );
        let reparsed = MdxDocument::parse(&serialized)
            .unwrap_or_else(|error| panic!("reparse {}: {error:?}", path.display()));
        assert_eq!(
            reparsed.to_bytes(),
            serialized,
            "round-trip failed for {}",
            path.display()
        );
    }
}

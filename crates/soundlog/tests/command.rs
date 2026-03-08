// chipstream/crates/soundlog/tests/command.rs
//
// Round-trip test for DAC stream control using the Secondary instance.
// This test builds a small VGM document (via VgmBuilder), including a
// SetupStreamControl that targets the SECONDARY instance, then
// serializes the document and feeds it to VgmStream to ensure the
// generated writes are associated with the Secondary instance.

use soundlog::VgmBuilder;
use soundlog::vgm::command::ChipId;
use soundlog::vgm::command::{
    DacStreamChipType, DataBlock, EndOfData, Instance, LengthMode, SetStreamData,
    SetStreamFrequency, SetupStreamControl, StartStream, StopStream, VgmCommand, WaitSamples,
};
use soundlog::vgm::stream::StreamResult;
use soundlog::vgm::stream::VgmStream;

/// Construct a VGM document that routes a data bank to YM2612 Secondary
/// instance and ensure the generated writes are tagged as Secondary.
#[test]
fn test_dac_stream_chip_type_roundtrip_secondary() {
    let mut builder = VgmBuilder::new();

    // Simple uncompressed stream data block (type 0x00 = YM2612 PCM)
    let stream_data = vec![0x01, 0x02, 0x03, 0x04];
    let block = DataBlock {
        marker: 0x66,
        chip_instance: Instance::Primary as u8,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(block);

    // Configure stream 0 to write to YM2612 (secondary instance), register 0x2A
    builder.add_vgm_command(SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Secondary,
        },
        write_port: 0,
        write_command: 0x2A,
    });

    // Point stream 0 at data bank 0x00 with step size 1
    builder.add_vgm_command(SetStreamData {
        stream_id: 0,
        data_bank_id: 0x00,
        step_size: 1,
        step_base: 0,
    });

    // Set stream frequency
    builder.add_vgm_command(SetStreamFrequency {
        stream_id: 0,
        frequency: 22050,
    });

    // Start stream: use CommandCount length to process 4 commands
    builder.add_vgm_command(StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: LengthMode::CommandCount {
            reverse: false,
            looped: false,
        },
        data_length: 4,
    });

    // Wait long enough for the stream writes to be emitted
    builder.add_vgm_command(WaitSamples(100));

    // Stop stream and end
    builder.add_vgm_command(StopStream { stream_id: 0 });
    builder.add_vgm_command(EndOfData);

    // Finalize and serialize
    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();

    // Parse with VgmStream::from_vgm and check emitted commands
    let mut stream = VgmStream::from_vgm(bytes.clone()).expect("from_vgm");

    let mut seen_secondary_write = false;
    for res in &mut stream {
        match res {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::Ym2612Write(inst, spec) = cmd
                    && inst == Instance::Secondary
                    && spec.register == 0x2A
                {
                    seen_secondary_write = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => panic!("stream error: {:?}, bytes_len: {}", e, bytes.len()),
        }
    }

    assert!(
        seen_secondary_write,
        "Should see YM2612 write for Secondary instance (roundtrip)"
    );
}

/// Round-trip tests for chip write serialization/parsing.
///
/// These tests construct several `VgmCommand` chip-write variants, serialize
/// them into full VGM documents (via `VgmBuilder` / `VgmDocument::to_bytes()`),
/// parse the serialized bytes back into a `VgmDocument`, and assert that the
/// parsed command stream contains an equivalent `VgmCommand`.
#[test]
fn test_chip_write_roundtrip_various() {
    use soundlog::chip;

    let commands: Vec<soundlog::vgm::command::VgmCommand> = vec![
        // YM2612 primary (port 0)
        soundlog::vgm::command::VgmCommand::Ym2612Write(
            Instance::Primary,
            chip::Ym2612Spec {
                port: 0,
                register: 0x2A,
                value: 0x55,
            },
        ),
        // YM2612 secondary (port 1)
        soundlog::vgm::command::VgmCommand::Ym2612Write(
            Instance::Secondary,
            chip::Ym2612Spec {
                port: 1,
                register: 0x40,
                value: 0x99,
            },
        ),
        // PSG (SN76489) write
        soundlog::vgm::command::VgmCommand::Sn76489Write(
            Instance::Primary,
            chip::PsgSpec { value: 0xAA },
        ),
        // Mikey write
        soundlog::vgm::command::VgmCommand::MikeyWrite(
            Instance::Primary,
            chip::MikeySpec {
                register: 0x10,
                value: 0x20,
            },
        ),
        // SegaPCM memory write
        soundlog::vgm::command::VgmCommand::SegaPcmWrite(
            Instance::Primary,
            chip::SegaPcmSpec {
                offset: 0x1234,
                value: 0x7F,
            },
        ),
    ];

    for cmd in commands.into_iter() {
        let mut builder = soundlog::VgmBuilder::new();
        builder.add_vgm_command(cmd.clone());
        // finalize() will ensure EndOfData is present
        let doc = builder.finalize();
        let bytes: Vec<u8> = (&doc).into();

        // Parse back using the public VgmStream API which accepts full VGM bytes.
        // Collect parsed commands emitted by the stream and check presence.
        let mut stream = VgmStream::from_vgm(bytes.clone()).expect("from_vgm");
        let mut parsed_commands: Vec<soundlog::vgm::command::VgmCommand> = Vec::new();
        for res in &mut stream {
            match res {
                Ok(StreamResult::Command(c)) => parsed_commands.push(c),
                Ok(StreamResult::NeedsMoreData) => break,
                Ok(StreamResult::EndOfStream) => break,
                Err(e) => panic!(
                    "stream error: {:?} for original: {:?}, bytes_len: {}",
                    e,
                    cmd,
                    bytes.len()
                ),
            }
        }

        // Assert that the parsed command stream contains an equivalent command.
        let found = parsed_commands.iter().any(|c| c == &cmd);
        assert!(found, "Roundtrip failed for command: {:?}", cmd);
    }
}

/// Comprehensive round-trip test for all chip write opcode branches
///
/// This test enumerates a broad set of chip-write `VgmCommand` variants that
/// cover the opcode branches handled by `parse_chip_write` and ensures that
/// serializing via `VgmDocument::to_bytes()` and parsing back preserves the
/// command semantics as recognized by the parser.
#[test]
fn test_parse_chip_write_all_opcodes_roundtrip() {
    use soundlog::VgmBuilder;
    use soundlog::chip;
    use soundlog::vgm::command::Instance;
    use soundlog::vgm::command::VgmCommand;

    // Helper to construct many common chip write commands. Each entry is
    // intended to exercise a distinct opcode branch in `parse_chip_write`.
    let mut cases: Vec<VgmCommand> = Vec::new();

    // Common register/value style chips
    cases.push(VgmCommand::Sn76489Write(
        Instance::Primary,
        chip::PsgSpec { value: 0x11 },
    ));
    cases.push(VgmCommand::Ym2413Write(
        Instance::Primary,
        chip::Ym2413Spec {
            register: 0x10,
            value: 0x22,
        },
    ));
    cases.push(VgmCommand::Ym2612Write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x2A,
            value: 0x33,
        },
    ));
    cases.push(VgmCommand::Ym2612Write(
        Instance::Secondary,
        chip::Ym2612Spec {
            port: 1,
            register: 0x3C,
            value: 0x44,
        },
    ));
    cases.push(VgmCommand::Ym2151Write(
        Instance::Primary,
        chip::Ym2151Spec {
            register: 0x01,
            value: 0x55,
        },
    ));
    cases.push(VgmCommand::Ym2203Write(
        Instance::Primary,
        chip::Ym2203Spec {
            register: 0x02,
            value: 0x66,
        },
    ));
    cases.push(VgmCommand::Ym2608Write(
        Instance::Primary,
        chip::Ym2608Spec {
            port: 0,
            register: 0x03,
            value: 0x77,
        },
    ));
    cases.push(VgmCommand::Ym2610bWrite(
        Instance::Primary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x04,
            value: 0x88,
        },
    ));
    cases.push(VgmCommand::Ym3812Write(
        Instance::Primary,
        chip::Ym3812Spec {
            register: 0x05,
            value: 0x99,
        },
    ));
    cases.push(VgmCommand::Ym3526Write(
        Instance::Primary,
        chip::Ym3526Spec {
            register: 0x06,
            value: 0xA0,
        },
    ));
    cases.push(VgmCommand::Y8950Write(
        Instance::Primary,
        chip::Y8950Spec {
            register: 0x07,
            value: 0xB1,
        },
    ));
    cases.push(VgmCommand::Ymz280bWrite(
        Instance::Primary,
        chip::Ymz280bSpec {
            register: 0x08,
            value: 0xC2,
        },
    ));
    cases.push(VgmCommand::Ymf262Write(
        Instance::Primary,
        chip::Ymf262Spec {
            port: 0,
            register: 0x09,
            value: 0xD3,
        },
    ));
    // AY8910 (A0)
    cases.push(VgmCommand::Ay8910Write(
        Instance::Primary,
        chip::Ay8910Spec {
            register: 0x05,
            value: 0x12,
        },
    ));
    cases.push(VgmCommand::Ay8910Write(
        Instance::Secondary,
        chip::Ay8910Spec {
            register: 0x07,
            value: 0x34,
        },
    ));
    // Mikey (0x40)
    cases.push(VgmCommand::MikeyWrite(
        Instance::Primary,
        chip::MikeySpec {
            register: 0x10,
            value: 0x77,
        },
    ));
    // Game Gear PSG (0x4F)
    cases.push(VgmCommand::GameGearPsgWrite(
        Instance::Primary,
        chip::GameGearPsgSpec { value: 0x99 },
    ));
    // Rf5c68 U8 (B0)
    cases.push(VgmCommand::Rf5c68U8Write(
        Instance::Primary,
        chip::Rf5c68U8Spec {
            offset: 0x10,
            value: 0x21,
        },
    ));
    // Rf5c68 U16 (C1)
    cases.push(VgmCommand::Rf5c68U16Write(
        Instance::Primary,
        chip::Rf5c68U16Spec {
            offset: 0x1234,
            value: 0x22,
        },
    ));
    // Rf5c164 U8 (B1)
    cases.push(VgmCommand::Rf5c164U8Write(
        Instance::Primary,
        chip::Rf5c164U8Spec {
            offset: 0x05,
            value: 0x23,
        },
    ));
    // Rf5c164 U16 (C2)
    cases.push(VgmCommand::Rf5c164U16Write(
        Instance::Primary,
        chip::Rf5c164U16Spec {
            offset: 0x4321,
            value: 0x24,
        },
    ));
    // Pwm (B2): value is 12-bit (ddd in "ad dd" format), max is 0x0FFF
    cases.push(VgmCommand::PwmWrite(
        Instance::Primary,
        chip::PwmSpec {
            register: 0x01,
            value: 0x0FFFu32,
        },
    ));
    // GbDmg (B3)
    cases.push(VgmCommand::GbDmgWrite(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x0A,
            value: 0x42,
        },
    ));
    // NesApu (B4)
    cases.push(VgmCommand::NesApuWrite(
        Instance::Primary,
        chip::NesApuSpec {
            register: 0x0B,
            value: 0x43,
        },
    ));
    // MultiPcm (B5)
    cases.push(VgmCommand::MultiPcmWrite(
        Instance::Primary,
        chip::MultiPcmSpec {
            register: 0x0C,
            value: 0x44,
        },
    ));
    // Upd7759 (B6)
    cases.push(VgmCommand::Upd7759Write(
        Instance::Primary,
        chip::Upd7759Spec {
            register: 0x0D,
            value: 0x45,
        },
    ));
    // Okim6258 (B7)
    cases.push(VgmCommand::Okim6258Write(
        Instance::Primary,
        chip::Okim6258Spec {
            register: 0x0E,
            value: 0x46,
        },
    ));
    // Okim6295 (B8)
    cases.push(VgmCommand::Okim6295Write(
        Instance::Primary,
        chip::Okim6295Spec {
            register: 0x0F,
            value: 0x47,
        },
    ));
    // Huc6280 (B9)
    cases.push(VgmCommand::Huc6280Write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x11,
            value: 0x48,
        },
    ));
    // K053260 (BA)
    cases.push(VgmCommand::K053260Write(
        Instance::Primary,
        chip::K053260Spec {
            register: 0x12,
            value: 0x49,
        },
    ));
    // Pokey (BB)
    cases.push(VgmCommand::PokeyWrite(
        Instance::Primary,
        chip::PokeySpec {
            register: 0x13,
            value: 0x4A,
        },
    ));
    // WonderSwan Reg (BC)
    cases.push(VgmCommand::WonderSwanRegWrite(
        Instance::Primary,
        chip::WonderSwanRegSpec {
            register: 0x14,
            value: 0x4B,
        },
    ));
    // SAA1099 (BD)
    cases.push(VgmCommand::Saa1099Write(
        Instance::Primary,
        chip::Saa1099Spec {
            register: 0x15,
            value: 0x4C,
        },
    ));
    // ES5506 U8 (BE)
    cases.push(VgmCommand::Es5506BEWrite(
        Instance::Primary,
        chip::Es5506U8Spec {
            register: 0x16,
            value: 0x4D,
        },
    ));
    // GA20 (BF)
    cases.push(VgmCommand::Ga20Write(
        Instance::Primary,
        chip::Ga20Spec {
            register: 0x17,
            value: 0x4E,
        },
    ));
    // MultiPcmBank (C3)
    cases.push(VgmCommand::MultiPcmBankWrite(
        Instance::Primary,
        chip::MultiPcmBankSpec {
            channel: 0x01,
            bank_offset: 0x0200,
        },
    ));
    // QSound (C4)
    cases.push(VgmCommand::QsoundWrite(
        Instance::Primary,
        chip::QsoundSpec {
            register: 0x01,
            value: 0x50,
        },
    ));
    // SCSP (C5)
    cases.push(VgmCommand::ScspWrite(
        Instance::Primary,
        chip::ScspSpec {
            offset: 0x20,
            value: 0x51,
        },
    ));
    // WonderSwan (C6)
    cases.push(VgmCommand::WonderSwanWrite(
        Instance::Primary,
        chip::WonderSwanSpec {
            offset: 0x1234,
            value: 0x52,
        },
    ));
    // VSU (C7)
    cases.push(VgmCommand::VsuWrite(
        Instance::Primary,
        chip::VsuSpec {
            offset: 0x21,
            value: 0x53,
        },
    ));
    // X1010 (C8)
    cases.push(VgmCommand::X1010Write(
        Instance::Primary,
        chip::X1010Spec {
            offset: 0x22,
            value: 0x54,
        },
    ));
    // YMF family and others at 0xD0..0xD4
    cases.push(VgmCommand::Ymf278bWrite(
        Instance::Primary,
        chip::Ymf278bSpec {
            port: 0,
            register: 0x30,
            value: 0x60,
        },
    ));
    cases.push(VgmCommand::Ymf271Write(
        Instance::Primary,
        chip::Ymf271Spec {
            port: 0,
            register: 0x31,
            value: 0x61,
        },
    ));
    cases.push(VgmCommand::Scc1Write(
        Instance::Primary,
        chip::Scc1Spec {
            port: 0,
            register: 0x32,
            value: 0x62,
        },
    ));
    cases.push(VgmCommand::K054539Write(
        Instance::Primary,
        chip::K054539Spec {
            register: 0x33,
            value: 0x63,
        },
    ));
    cases.push(VgmCommand::C140Write(
        Instance::Primary,
        chip::C140Spec {
            register: 0x34,
            value: 0x64,
        },
    ));
    cases.push(VgmCommand::Es5503Write(
        Instance::Primary,
        chip::Es5503Spec {
            register: 0x35,
            value: 0x65,
        },
    ));
    cases.push(VgmCommand::Es5506D6Write(
        Instance::Primary,
        chip::Es5506U16Spec {
            register: 0x36,
            value: 0x1234,
        },
    ));
    cases.push(VgmCommand::C352Write(
        Instance::Primary,
        chip::C352Spec {
            register: 0x40,
            value: 0x66,
        },
    ));

    // ----------------------------------------------------------------
    // Secondary instance cases — one per Dual Chip Support eligible chip
    // (VGM spec: chips that support two instances).
    // ----------------------------------------------------------------

    // PSG / SN76489 Secondary (0x30)
    cases.push(VgmCommand::Sn76489Write(
        Instance::Secondary,
        chip::PsgSpec { value: 0x12 },
    ));
    // YM2413 Secondary (0xA1)
    cases.push(VgmCommand::Ym2413Write(
        Instance::Secondary,
        chip::Ym2413Spec {
            register: 0x10,
            value: 0x23,
        },
    ));
    // YM2151 Secondary (0xA4)
    cases.push(VgmCommand::Ym2151Write(
        Instance::Secondary,
        chip::Ym2151Spec {
            register: 0x01,
            value: 0x34,
        },
    ));
    // YM2203 Secondary (0xA5)
    cases.push(VgmCommand::Ym2203Write(
        Instance::Secondary,
        chip::Ym2203Spec {
            register: 0x02,
            value: 0x45,
        },
    ));
    // YM2608 Secondary (0xA6 / 0xA7)
    cases.push(VgmCommand::Ym2608Write(
        Instance::Secondary,
        chip::Ym2608Spec {
            port: 0,
            register: 0x03,
            value: 0x56,
        },
    ));
    // YM2610 Secondary (0xA8 / 0xA9)
    cases.push(VgmCommand::Ym2610bWrite(
        Instance::Secondary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x04,
            value: 0x67,
        },
    ));
    // YM3812 Secondary (0xAA)
    cases.push(VgmCommand::Ym3812Write(
        Instance::Secondary,
        chip::Ym3812Spec {
            register: 0x05,
            value: 0x78,
        },
    ));
    // YM3526 Secondary (0xAB)
    cases.push(VgmCommand::Ym3526Write(
        Instance::Secondary,
        chip::Ym3526Spec {
            register: 0x06,
            value: 0x79,
        },
    ));
    // Y8950 Secondary (0xAC)
    cases.push(VgmCommand::Y8950Write(
        Instance::Secondary,
        chip::Y8950Spec {
            register: 0x07,
            value: 0x7A,
        },
    ));
    // YMZ280B Secondary (0xAD)
    cases.push(VgmCommand::Ymz280bWrite(
        Instance::Secondary,
        chip::Ymz280bSpec {
            register: 0x08,
            value: 0x7B,
        },
    ));
    // YMF262 Secondary (0xAE / 0xAF)
    cases.push(VgmCommand::Ymf262Write(
        Instance::Secondary,
        chip::Ymf262Spec {
            port: 0,
            register: 0x09,
            value: 0x7C,
        },
    ));
    // GbDmg Secondary (0xB3, bit7 on first param)
    cases.push(VgmCommand::GbDmgWrite(
        Instance::Secondary,
        chip::GbDmgSpec {
            register: 0x0A,
            value: 0x11,
        },
    ));
    // NesApu Secondary (0xB4, bit7 on first param)
    cases.push(VgmCommand::NesApuWrite(
        Instance::Secondary,
        chip::NesApuSpec {
            register: 0x0B,
            value: 0x22,
        },
    ));
    // MultiPcm Secondary (0xB5, bit7 on first param)
    cases.push(VgmCommand::MultiPcmWrite(
        Instance::Secondary,
        chip::MultiPcmSpec {
            register: 0x0C,
            value: 0x33,
        },
    ));
    // Upd7759 Secondary (0xB6, bit7 on first param)
    cases.push(VgmCommand::Upd7759Write(
        Instance::Secondary,
        chip::Upd7759Spec {
            register: 0x0D,
            value: 0x44,
        },
    ));
    // Okim6258 Secondary (0xB7, bit7 on first param)
    cases.push(VgmCommand::Okim6258Write(
        Instance::Secondary,
        chip::Okim6258Spec {
            register: 0x0E,
            value: 0x55,
        },
    ));
    // Okim6295 Secondary (0xB8, bit7 on first param)
    cases.push(VgmCommand::Okim6295Write(
        Instance::Secondary,
        chip::Okim6295Spec {
            register: 0x0F,
            value: 0x66,
        },
    ));
    // Huc6280 Secondary (0xB9, bit7 on first param)
    cases.push(VgmCommand::Huc6280Write(
        Instance::Secondary,
        chip::Huc6280Spec {
            register: 0x11,
            value: 0x77,
        },
    ));
    // K053260 Secondary (0xBA, bit7 on first param)
    cases.push(VgmCommand::K053260Write(
        Instance::Secondary,
        chip::K053260Spec {
            register: 0x12,
            value: 0x11,
        },
    ));
    // Pokey Secondary (0xBB, bit7 on first param)
    cases.push(VgmCommand::PokeyWrite(
        Instance::Secondary,
        chip::PokeySpec {
            register: 0x13,
            value: 0x22,
        },
    ));
    // WonderSwanReg Secondary (0xBC, bit7 on first param)
    cases.push(VgmCommand::WonderSwanRegWrite(
        Instance::Secondary,
        chip::WonderSwanRegSpec {
            register: 0x14,
            value: 0x33,
        },
    ));
    // SAA1099 Secondary (0xBD, bit7 on first param)
    cases.push(VgmCommand::Saa1099Write(
        Instance::Secondary,
        chip::Saa1099Spec {
            register: 0x15,
            value: 0x44,
        },
    ));
    // ES5506 U8 Secondary (0xBE, bit7 on first param)
    cases.push(VgmCommand::Es5506BEWrite(
        Instance::Secondary,
        chip::Es5506U8Spec {
            register: 0x16,
            value: 0x55,
        },
    ));
    // GA20 Secondary (0xBF, bit7 on first param)
    cases.push(VgmCommand::Ga20Write(
        Instance::Secondary,
        chip::Ga20Spec {
            register: 0x17,
            value: 0x66,
        },
    ));
    // SegaPCM Secondary (0xC0, bit7 on *second* param = high byte of address)
    cases.push(VgmCommand::SegaPcmWrite(
        Instance::Secondary,
        chip::SegaPcmSpec {
            offset: 0x1234,
            value: 0x7F,
        },
    ));
    // MultiPcmBank Secondary (0xC3, bit7 on first param = channel)
    cases.push(VgmCommand::MultiPcmBankWrite(
        Instance::Secondary,
        chip::MultiPcmBankSpec {
            channel: 0x01,
            bank_offset: 0x0100,
        },
    ));
    // SCSP Secondary (0xC5, bit7 on first param)
    cases.push(VgmCommand::ScspWrite(
        Instance::Secondary,
        chip::ScspSpec {
            offset: 0x20,
            value: 0x11,
        },
    ));
    // WonderSwan Secondary (0xC6, bit7 on first param)
    cases.push(VgmCommand::WonderSwanWrite(
        Instance::Secondary,
        chip::WonderSwanSpec {
            offset: 0x1234,
            value: 0x22,
        },
    ));
    // VSU Secondary (0xC7, bit7 on first param)
    cases.push(VgmCommand::VsuWrite(
        Instance::Secondary,
        chip::VsuSpec {
            offset: 0x21,
            value: 0x33,
        },
    ));
    // X1010 Secondary (0xC8, bit7 on first param)
    cases.push(VgmCommand::X1010Write(
        Instance::Secondary,
        chip::X1010Spec {
            offset: 0x22,
            value: 0x44,
        },
    ));
    // YMF278B Secondary (0xD0, bit7 on first param = port)
    cases.push(VgmCommand::Ymf278bWrite(
        Instance::Secondary,
        chip::Ymf278bSpec {
            port: 0x00,
            register: 0x30,
            value: 0x55,
        },
    ));
    // YMF271 Secondary (0xD1, bit7 on first param = port)
    cases.push(VgmCommand::Ymf271Write(
        Instance::Secondary,
        chip::Ymf271Spec {
            port: 0x00,
            register: 0x31,
            value: 0x66,
        },
    ));
    // SCC1 Secondary (0xD2, bit7 on first param = port)
    cases.push(VgmCommand::Scc1Write(
        Instance::Secondary,
        chip::Scc1Spec {
            port: 0x00,
            register: 0x32,
            value: 0x77,
        },
    ));
    // K054539 Secondary (0xD3, bit7 on first param)
    cases.push(VgmCommand::K054539Write(
        Instance::Secondary,
        chip::K054539Spec {
            register: 0x33,
            value: 0x11,
        },
    ));
    // C140 Secondary (0xD4, bit7 on first param)
    cases.push(VgmCommand::C140Write(
        Instance::Secondary,
        chip::C140Spec {
            register: 0x34,
            value: 0x22,
        },
    ));
    // ES5503 Secondary (0xD5, bit7 on first param)
    cases.push(VgmCommand::Es5503Write(
        Instance::Secondary,
        chip::Es5503Spec {
            register: 0x35,
            value: 0x33,
        },
    ));
    // ES5506 U16 Secondary (0xD6, bit7 on first param)
    cases.push(VgmCommand::Es5506D6Write(
        Instance::Secondary,
        chip::Es5506U16Spec {
            register: 0x36,
            value: 0x1234,
        },
    ));
    // C352 Secondary (0xE1, bit7 on first param)
    cases.push(VgmCommand::C352Write(
        Instance::Secondary,
        chip::C352Spec {
            register: 0x40,
            value: 0x44,
        },
    ));

    // For each constructed command, create a document, serialize and parse back.
    for (case_index, original) in cases.into_iter().enumerate() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(original.clone());
        // finalize() ensures EndOfData present
        let doc = builder.finalize();
        let bytes: Vec<u8> = (&doc).into();

        // Parse the serialized VGM bytes using the public VgmStream API which accepts
        // finalized VGM bytes (header + commands). Collect parsed commands emitted by
        // the stream and check presence.
        // Diagnostic: parse header first to inspect computed offsets. This helps
        // identify header-related OffsetOutOfRange errors when the file length
        // and claimed header/data offsets are inconsistent.
        let header_res = soundlog::vgm::header::VgmHeader::from_bytes(&bytes);
        if case_index == 14 || header_res.is_err() {
            eprintln!("case {} original: {:?}", case_index, original);
            match &header_res {
                Ok(h) => {
                    let cmd_start =
                        soundlog::vgm::header::VgmHeader::command_start(h.version, h.data_offset);
                    eprintln!(
                        "header ok: version={:#X} data_offset={} command_start={} extra_header_offset={} loop_offset={} bytes_len={}",
                        h.version,
                        h.data_offset,
                        cmd_start,
                        h.extra_header_offset,
                        h.loop_offset,
                        bytes.len()
                    );
                }
                Err(e) => {
                    eprintln!("header parse error: {:?} bytes_len={}", e, bytes.len());
                }
            }
        }
        let mut stream = VgmStream::from_vgm(bytes.clone()).expect("from_vgm");
        let mut parsed_commands: Vec<VgmCommand> = Vec::new();
        for res in &mut stream {
            match res {
                Ok(StreamResult::Command(c)) => parsed_commands.push(c),
                Ok(StreamResult::EndOfStream) => break,
                Ok(StreamResult::NeedsMoreData) => break,
                Err(e) => {
                    // Diagnostic context to help investigate OffsetOutOfRange:
                    // - case index and original command
                    // - total bytes length
                    // - short hex dump (head/tail) up to 32 bytes each
                    let len = bytes.len();
                    let head_len = len.min(32);
                    let tail_len = if len > 64 { 32 } else { 0 };
                    let head = bytes[..head_len]
                        .iter()
                        .map(|b| format!("{:02X}", b))
                        .collect::<Vec<_>>()
                        .join(" ");
                    let tail = if tail_len > 0 {
                        bytes[len - tail_len..]
                            .iter()
                            .map(|b| format!("{:02X}", b))
                            .collect::<Vec<_>>()
                            .join(" ")
                    } else {
                        String::new()
                    };

                    // Attempt to gather a parser-side trace up to the first parse error.
                    // This will run the parser in trace mode and return the sequence of
                    // parsed opcodes/offsets it decoded prior to failing. It helps
                    // pinpoint exactly which command (and byte offset) triggered the
                    // OffsetOutOfRange.
                    let (trace, trace_err) =
                        soundlog::vgm::parser::trace_vgm_commands_until_error(&bytes);

                    let trace_summary = if trace.is_empty() {
                        "no commands parsed".to_string()
                    } else {
                        trace
                            .into_iter()
                            .map(|(pos, opcode, cons)| {
                                format!("[pos={:#X}, op={:#X}, cons={}]", pos, opcode, cons)
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    };

                    if case_index == 14 {
                        // Print the full hex dump for case 14 to aid debugging of the failure.
                        // This gives complete visibility into the serialized VGM bytes that
                        // triggered the OffsetOutOfRange during parsing.
                        let full_hex = bytes
                            .iter()
                            .map(|b| format!("{:02X}", b))
                            .collect::<Vec<_>>()
                            .join(" ");
                        eprintln!("FULL BYTES (len={}): {}", bytes.len(), full_hex);
                    }
                    panic!(
                        "stream error: {:?} for case {} original: {:?}\nbytes_len: {}\nhead: {}\ntail: {}\ntrace_err: {:?}\ntrace: {}",
                        e, case_index, original, len, head, tail, trace_err, trace_summary
                    );
                }
            }
        }

        // Ensure the original command is present in the parsed commands.
        let found = parsed_commands.iter().any(|c| c == &original);
        assert!(found, "Roundtrip failed for command: {:?}", original);
    }
}

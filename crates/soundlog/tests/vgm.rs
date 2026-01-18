use soundlog::chip::*;
use soundlog::vgm::command::{
    Ay8910StereoMask, DataBlock, Instance, PcmRamWrite, SeekOffset, SetStreamFrequency,
    SetupStreamControl, StartStream, StartStreamFastCall, StopStream, VgmCommand, Wait735Samples,
    Wait882Samples, WaitNSample, WaitSamples, Ym2612Port0Address2AWriteAndWaitN,
};
use soundlog::{VgmBuilder, VgmDocument, VgmHeader};

#[test]
fn build_minimal_vgmdocument() {
    // Build an empty/default VGM document using the builder API.
    let doc: VgmDocument = VgmBuilder::new().finalize();
    // Header defaults are set and commands are empty.
    assert_eq!(doc.iter().count(), 0);
    assert_eq!(doc.header, VgmHeader::default());
}

#[test]
fn test_total_samples_computed_correctly() {
    // build vgm
    let mut b = VgmBuilder::new();
    b.add_vgm_command(WaitSamples(100)); // 100
    b.add_vgm_command(Wait735Samples); // 735
    b.add_vgm_command(Wait882Samples); // 882
    b.add_vgm_command(WaitNSample(5)); // 5
    b.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(3)); // 3
    let doc = b.finalize();

    // test re-parse
    let bytes: Vec<u8> = (&doc).into();
    let parsed: VgmDocument = (bytes.as_slice())
        .try_into()
        .expect("failed to parse serialized VGM");

    // compute total samples manually
    let computed_total: u32 = parsed
        .iter()
        .map(|cmd| match cmd {
            VgmCommand::WaitSamples(s) => s.0 as u32,
            VgmCommand::Wait735Samples(_) => 735,
            VgmCommand::Wait882Samples(_) => 882,
            VgmCommand::WaitNSample(s) => s.0 as u32,
            VgmCommand::YM2612Port0Address2AWriteAndWaitN(s) => s.0 as u32,
            _ => 0,
        })
        .sum();

    assert_eq!(computed_total, 1725u32);
}

// Tests using `add_chip_write` to ensure `Into<chip::Chip>` conversions work
#[test]
fn add_chip_write_psg() {
    let mut b = VgmBuilder::new();
    b.add_chip_write(0usize, PsgSpec { value: 0xAB });
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::Sn76489Write(id, s) => {
            assert_eq!(usize::from(id), 0usize);
            assert_eq!(s, PsgSpec { value: 0xAB });
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_chip_write_ym2413() {
    let mut b = VgmBuilder::new();
    b.add_chip_write(
        1usize,
        Ym2413Spec {
            register: 0x10,
            value: 0x22,
        },
    );
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::Ym2413Write(id, s) => {
            assert_eq!(usize::from(id), 1usize);
            assert_eq!(
                s,
                Ym2413Spec {
                    register: 0x10,
                    value: 0x22
                }
            );
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_chip_write_ym2612_ports() {
    let mut b = VgmBuilder::new();
    b.add_chip_write(
        Instance::Secondary,
        Ym2612Spec {
            port: 0,
            register: 0x2A,
            value: 0x55,
        },
    );
    b.add_chip_write(
        Instance::Secondary,
        Ym2612Spec {
            port: 1,
            register: 0x2A,
            value: 0x66,
        },
    );
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 2);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::Ym2612Write(id, s) => {
            assert_eq!(usize::from(id), 1usize);
            assert_eq!(
                s,
                Ym2612Spec {
                    port: 0,
                    register: 0x2A,
                    value: 0x55
                }
            );
        }
        other => panic!("unexpected: {:?}", other),
    }
    match doc.iter().nth(1).unwrap().clone() {
        VgmCommand::Ym2612Write(id, s) => {
            assert_eq!(usize::from(id), 1usize);
            assert_eq!(
                s,
                Ym2612Spec {
                    port: 1,
                    register: 0x2A,
                    value: 0x66
                }
            );
        }
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn add_chip_write_pwm() {
    let mut b = VgmBuilder::new();
    b.add_chip_write(
        Instance::Secondary,
        PwmSpec {
            register: 0x01,
            value: 0x0000_FFEE,
        },
    );
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::PwmWrite(id, s) => {
            assert_eq!(usize::from(id), 1usize);
            assert_eq!(
                s,
                PwmSpec {
                    register: 0x01,
                    value: 0x0000_FFEE
                }
            );
        }
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn add_chip_write_okim6295() {
    let mut b = VgmBuilder::new();
    b.add_chip_write(
        Instance::Secondary,
        Okim6295Spec {
            register: 0x0F,
            value: 0x10,
        },
    );
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::Okim6295Write(id, s) => {
            assert_eq!(usize::from(id), 1usize);
            assert_eq!(
                s,
                Okim6295Spec {
                    register: 0x0F,
                    value: 0x10
                }
            );
        }
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn add_command_wait_samples() {
    let mut b = VgmBuilder::new();
    b.add_vgm_command(WaitSamples(0x1234));
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::WaitSamples(s) => assert_eq!(s, WaitSamples(0x1234)),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_command_data_block() {
    let mut b = VgmBuilder::new();
    let data = vec![1u8, 2, 3];
    let spec = DataBlock {
        marker: 0x66,
        chip_id: 1,
        data_type: 0x01,
        size: data.len() as u32,
        data: data.clone(),
    };
    b.add_vgm_command(spec.clone());
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::DataBlock(s) => assert_eq!(s.data, data),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_command_pcm_ram_write() {
    let mut b = VgmBuilder::new();
    let spec = PcmRamWrite {
        marker: 0x66,
        chip_type: 0x66,
        read_offset: 0x010203,
        write_offset: 0x030201,
        size: 3,
        data: vec![4, 5, 6],
    };
    b.add_vgm_command(spec.clone());
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::PcmRamWrite(s) => assert_eq!(s, spec),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_command_wait_n_sample() {
    let mut b = VgmBuilder::new();
    b.add_vgm_command(WaitNSample(5));
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::WaitNSample(s) => assert_eq!(s, WaitNSample(5)),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_command_ay8910_mask_and_seek() {
    let mut b = VgmBuilder::new();
    b.add_vgm_command(Ay8910StereoMask(0xAA));
    b.add_vgm_command(SeekOffset(0xDEADBEEF));
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 2);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::AY8910StereoMask(s) => assert_eq!(s, Ay8910StereoMask(0xAA)),
        other => panic!("unexpected command: {:?}", other),
    }
    match doc.iter().nth(1).unwrap().clone() {
        VgmCommand::SeekOffset(s) => assert_eq!(s, SeekOffset(0xDEADBEEF)),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_command_stream_controls() {
    let mut b = VgmBuilder::new();
    let setup = SetupStreamControl {
        stream_id: 1,
        chip_type: 2,
        write_port: 3,
        write_command: 4,
    };
    let freq = SetStreamFrequency {
        stream_id: 1,
        frequency: 0x11223344,
    };
    b.add_vgm_command(setup.clone());
    b.add_vgm_command(freq.clone());
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 2);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::SetupStreamControl(s) => assert_eq!(s, setup),
        other => panic!("unexpected command: {:?}", other),
    }
    match doc.iter().nth(1).unwrap().clone() {
        VgmCommand::SetStreamFrequency(s) => assert_eq!(s, freq),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_command_start_stop_and_fastcall() {
    let mut b = VgmBuilder::new();
    b.add_vgm_command(StartStream {
        stream_id: 7,
        data_start_offset: -1,
        length_mode: 0,
        data_length: 0,
    });
    b.add_vgm_command(StopStream { stream_id: 7 });
    b.add_vgm_command(StartStreamFastCall {
        stream_id: 8,
        block_id: 0x1234,
        flags: 9,
    });
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 3);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::StartStream(s) => assert_eq!(
            s,
            StartStream {
                stream_id: 7,
                data_start_offset: -1,
                length_mode: 0,
                data_length: 0
            }
        ),
        other => panic!("unexpected: {:?}", other),
    }
    match doc.iter().nth(1).unwrap().clone() {
        VgmCommand::StopStream(s) => assert_eq!(s, StopStream { stream_id: 7 }),
        other => panic!("unexpected: {:?}", other),
    }
    match doc.iter().nth(2).unwrap().clone() {
        VgmCommand::StartStreamFastCall(s) => assert_eq!(
            s,
            StartStreamFastCall {
                stream_id: 8,
                block_id: 0x1234,
                flags: 9
            }
        ),
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn add_command_ym2612_port0_address2a() {
    let mut b = VgmBuilder::new();
    b.add_vgm_command(Ym2612Port0Address2AWriteAndWaitN(3));
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::YM2612Port0Address2AWriteAndWaitN(s) => {
            assert_eq!(s, Ym2612Port0Address2AWriteAndWaitN(3))
        }
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn add_chip_registers_and_sets_header_clock() {
    let mut b = VgmBuilder::new();
    // register a YM2413 instance at id 0
    b.register_chip(Chip::Ym2413, 0, 3579545);
    let doc = b.finalize();
    assert_eq!(doc.header.ym2413_clock, 3579545);
}

#[test]
fn add_chip_sets_msb_for_instance1() {
    let mut b = VgmBuilder::new();
    // chip_id 1 should set MSB of the clock field
    b.register_chip(Chip::Ym2413, 1, 3579545);
    let doc = b.finalize();
    assert_eq!(doc.header.ym2413_clock, 3579545u32 | 0x8000_0000u32);
}

#[test]
fn add_chip_write_uses_registered_instance() {
    let mut b = VgmBuilder::new();
    b.register_chip(Chip::Ym2612, 0, 7987200);
    b.add_chip_write(
        0,
        Ym2612Spec {
            port: 0,
            register: 0x2A,
            value: 0x77,
        },
    );
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::Ym2612Write(i, s) => {
            assert_eq!(usize::from(i), 0usize);
            assert_eq!(
                s,
                Ym2612Spec {
                    port: 0,
                    register: 0x2A,
                    value: 0x77
                }
            );
        }
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn header_chip_instances_enumeration() {
    let mut b = VgmBuilder::new();
    // primary Ym2413 at id 0
    b.register_chip(Chip::Ym2413, 0, 3579545);
    // secondary Ym2612 at id 1
    b.register_chip(Chip::Ym2612, 1, 7987200);
    let doc = b.finalize();

    let instances = doc.header.chip_instances();

    assert_eq!(instances.len(), 2);
    assert!(instances.contains(&(Instance::Primary, Chip::Ym2413)));
    assert!(instances.contains(&(Instance::Secondary, Chip::Ym2612)));
}

#[test]
fn add_chip_write_scc1() {
    let mut b = VgmBuilder::new();
    b.add_chip_write(
        Instance::Secondary,
        Scc1Spec {
            port: 0x05,
            register: 0x06,
            value: 0x07,
        },
    );
    let doc = b.finalize();
    assert_eq!(doc.iter().count(), 1);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::Scc1Write(id, s) => {
            assert_eq!(usize::from(id), 1usize);
            assert_eq!(
                s,
                Scc1Spec {
                    port: 0x05,
                    register: 0x06,
                    value: 0x07
                }
            );
        }
        other => panic!("unexpected: {:?}", other),
    }
}

#[test]
fn roundtrip_vgmdocument_into_vec_and_parse() {
    // Build a small document with a registered chip and a couple commands.
    let mut b = VgmBuilder::new();
    b.register_chip(Chip::Ym2612, 0, 7987200);
    b.add_chip_write(
        0usize,
        Ym2612Spec {
            port: 0,
            register: 0x2A,
            value: 0x77,
        },
    );
    b.add_vgm_command(WaitSamples(0x1234));

    let doc = b.finalize();

    let bytes: Vec<u8> = (&doc).into();

    let parsed: VgmDocument = (bytes.as_slice())
        .try_into()
        .expect("failed to parse serialized VGM");

    let mut parsed_commands: Vec<VgmCommand> = parsed.iter().cloned().collect();
    if let Some(c) = parsed_commands.last()
        && matches!(c, VgmCommand::EndOfData(_))
    {
        parsed_commands.pop();
    }
    assert_eq!(parsed_commands, doc.iter().cloned().collect::<Vec<_>>());
    assert_eq!(parsed.gd3, doc.gd3);
    assert_eq!(parsed.header.ident, doc.header.ident);
    assert_eq!(parsed.header.version, doc.header.version);
    assert_eq!(parsed.header.total_samples, doc.header.total_samples);
    assert_eq!(parsed.header.sample_rate, doc.header.sample_rate);
    assert_eq!(parsed.header.ym2612_clock, doc.header.ym2612_clock);
}

#[test]
fn iterate_vgmdocument_by_ref_mut_and_value() {
    // Build a document with a couple commands
    let mut b = VgmBuilder::new();
    b.add_vgm_command(WaitSamples(0x10));
    b.add_vgm_command(SeekOffset(0x1234));
    let mut doc = b.finalize();

    // Iterate by reference
    let mut collected_ref: Vec<VgmCommand> = Vec::new();
    for c in &doc {
        collected_ref.push(c.clone());
    }
    assert_eq!(collected_ref, doc.iter().cloned().collect::<Vec<_>>());

    let first_before = doc.iter().next().unwrap().clone();
    if let Some(c) = doc.iter_mut().next() {
        *c = WaitSamples(0xFFFF).into();
    }
    assert_ne!(doc.iter().next().unwrap(), &first_before);
    assert_eq!(doc.iter().next().unwrap(), &WaitSamples(0xFFFF).into());

    let expected: Vec<VgmCommand> = doc.iter().cloned().collect();
    let consumed: Vec<VgmCommand> = doc.into_iter().collect();
    assert_eq!(consumed, expected);
}

#[test]
fn test_create_and_parse_vgm_document() {
    let mut builder = VgmBuilder::new();

    builder.add_chip_write(
        0,
        Ym2203Spec {
            register: 0x22,
            value: 0x33,
        },
    );
    builder.add_chip_write(
        1,
        Ym2203Spec {
            register: 0x22,
            value: 0x33,
        },
    );
    builder.add_chip_write(
        1,
        Ymf262Spec {
            port: 0,
            register: 0x22,
            value: 0x33,
        },
    );
    builder.add_vgm_command(WaitNSample(20));

    let vgm: VgmDocument = builder.finalize();

    vgm.into_iter().for_each(|cmd| {
        println!("{:?}", cmd);
    });
}

#[test]
fn test_loop_offset_serialized_matches_header() {
    let mut b = VgmBuilder::new();
    // two commands, set loop to the second one (index 1)
    b.add_vgm_command(WaitSamples(1));
    b.add_vgm_command(WaitSamples(2));
    b.set_loop_offset(1usize);
    let doc = b.finalize();

    // header field computed by finalize
    let header_loop = doc.header.loop_offset;

    // serialized bytes should contain the same little-endian u32 at 0x1C
    let bytes: Vec<u8> = (&doc).into();
    assert!(bytes.len() >= 0x20);
    let ser_loop = u32::from_le_bytes([bytes[0x1C], bytes[0x1D], bytes[0x1E], bytes[0x1F]]);

    assert_eq!(header_loop, ser_loop);
}

#[test]
fn test_fallback_header_size_v1_00() {
    // Public VgmDocument serialization expands the header to the full
    // VGM_MAX_HEADER_SIZE (0x100) when data_offset == 0. Ensure the
    // serialized document reflects that behavior.
    let mut doc = VgmBuilder::new().finalize();
    // force version to 1.00 and ensure no explicit data_offset
    doc.header.version = 0x00000100;
    doc.header.data_offset = 0;

    // Serialize the full document and inspect resulting file bytes.
    let bytes: Vec<u8> = (&doc).into();

    // Serialized header (excluding appended EndOfData opcode) should be 0x100
    assert_eq!(bytes.len(), 0x100);
}

#[test]
fn test_explicit_data_offset_affects_header_size() {
    // When data_offset is non-zero, header size should be 0x34 + data_offset.
    let mut doc = VgmBuilder::new().finalize();
    doc.header.data_offset = 0x20; // example small explicit offset

    let bytes: Vec<u8> = (&doc).into();

    assert_eq!(bytes.len(), 0x34usize + 0x20usize);
}

#[test]
fn test_small_version_does_not_include_extra_header_field() {
    // Historically older versions had smaller main-header layouts that did not
    // include the stored extra-header offset (0xBC..0xBF). The public
    // serialization, however, expands the header to 0x100 when data_offset == 0.
    // Confirm the public serialization behavior for a small-version header.
    let mut doc = VgmBuilder::new().finalize();
    doc.header.version = 0x00000110; // legacy small-version indicator
    doc.header.data_offset = 0;

    let bytes: Vec<u8> = (&doc).into();

    // Serialized header (excluding appended EndOfData opcode) should be 0x100
    assert_eq!(bytes.len(), 0x100);
}

#[test]
fn test_extra_header_stored_offset_written_when_layout_allows() {
    // When an extra-header is present and the main header layout contains the
    // stored-offset field (0xBC..0xBF), the serialized file should contain a
    // non-zero stored extra-header offset.
    let mut b = VgmBuilder::new();

    // Create a minimal extra header (to_bytes will compute the real header_size)
    let extra = soundlog::VgmExtraHeader {
        header_size: 0,
        chip_clock_offset: 0,
        chip_vol_offset: 0,
        chip_clocks: Vec::new(),
        chip_volumes: Vec::new(),
    };
    b.set_extra_header(extra);

    let doc = b.finalize();
    let bytes: Vec<u8> = (&doc).into();

    // If the main header layout contains the stored extra-header offset field,
    // it will be written at 0xBC..0xBF. Check that field is non-zero.
    if bytes.len() >= 0xC0 {
        let extra_offset = u32::from_le_bytes([bytes[0xBC], bytes[0xBD], bytes[0xBE], bytes[0xBF]]);
        assert!(extra_offset != 0);
    } else {
        // If the serialized header is unusually short (shouldn't be for modern versions),
        // then the test is not applicable; at minimum ensure we didn't panic and bytes were produced.
        assert!(!bytes.is_empty());
    }
}

#[test]
fn test_tail_header_fields_roundtrip() {
    // Ensure that tail header fields (wonderswan, vsu, saa1099, es5503) are
    // preserved through serialization and re-parsing.
    let mut doc = VgmBuilder::new().finalize();

    // sample non-zero values for tail fields
    let wonderswan_val: u32 = 0x1111_2222;
    let vsu_val: u32 = 0x3333_4444;
    let saa1099_val: u32 = 0x5555_6666;
    let es5503_val: u32 = 0x7777_8888;

    doc.header.wonderswan_clock = wonderswan_val;
    doc.header.vsu_clock = vsu_val;
    doc.header.saa1099_clock = saa1099_val;
    doc.header.es5503_clock = es5503_val;

    let bytes: Vec<u8> = (&doc).into();

    // Re-parse and verify header fields preserved
    let parsed: VgmDocument = (&bytes[..])
        .try_into()
        .expect("failed to parse serialized VGM");

    assert_eq!(parsed.header.wonderswan_clock, wonderswan_val);
    assert_eq!(parsed.header.vsu_clock, vsu_val);
    assert_eq!(parsed.header.saa1099_clock, saa1099_val);
    assert_eq!(parsed.header.es5503_clock, es5503_val);

    // If the serialized buffer includes those offsets, also verify raw bytes match.
    if bytes.len() >= 0xC0 + 4 {
        let raw_wonderswan =
            u32::from_le_bytes([bytes[0xC0], bytes[0xC1], bytes[0xC2], bytes[0xC3]]);
        assert_eq!(raw_wonderswan, wonderswan_val);
    }
    if bytes.len() >= 0xC4 + 4 {
        let raw_vsu = u32::from_le_bytes([bytes[0xC4], bytes[0xC5], bytes[0xC6], bytes[0xC7]]);
        assert_eq!(raw_vsu, vsu_val);
    }
    if bytes.len() >= 0xC8 + 4 {
        let raw_saa1099 = u32::from_le_bytes([bytes[0xC8], bytes[0xC9], bytes[0xCA], bytes[0xCB]]);
        assert_eq!(raw_saa1099, saa1099_val);
    }
    if bytes.len() >= 0xCC + 4 {
        let raw_es5503 = u32::from_le_bytes([bytes[0xCC], bytes[0xCD], bytes[0xCE], bytes[0xCF]]);
        assert_eq!(raw_es5503, es5503_val);
    }

    // Serialize the reparsed document again and ensure the tail fields remain
    let reserialized: Vec<u8> = (&parsed).into();
    if reserialized.len() >= 0xC0 + 4 {
        let raw_w2 = u32::from_le_bytes([
            reserialized[0xC0],
            reserialized[0xC1],
            reserialized[0xC2],
            reserialized[0xC3],
        ]);
        assert_eq!(raw_w2, wonderswan_val);
    }
    if reserialized.len() >= 0xC4 + 4 {
        let raw_v2 = u32::from_le_bytes([
            reserialized[0xC4],
            reserialized[0xC5],
            reserialized[0xC6],
            reserialized[0xC7],
        ]);
        assert_eq!(raw_v2, vsu_val);
    }
    if reserialized.len() >= 0xC8 + 4 {
        let raw_s2 = u32::from_le_bytes([
            reserialized[0xC8],
            reserialized[0xC9],
            reserialized[0xCA],
            reserialized[0xCB],
        ]);
        assert_eq!(raw_s2, saa1099_val);
    }
    if reserialized.len() >= 0xCC + 4 {
        let raw_e2 = u32::from_le_bytes([
            reserialized[0xCC],
            reserialized[0xCD],
            reserialized[0xCE],
            reserialized[0xCF],
        ]);
        assert_eq!(raw_e2, es5503_val);
    }

    // And ensure reparsing the reserialized bytes yields the same header fields.
    let reparsed2: VgmDocument = (&reserialized[..])
        .try_into()
        .expect("failed to parse reserialized VGM");
    assert_eq!(reparsed2.header.wonderswan_clock, wonderswan_val);
    assert_eq!(reparsed2.header.vsu_clock, vsu_val);
    assert_eq!(reparsed2.header.saa1099_clock, saa1099_val);
    assert_eq!(reparsed2.header.es5503_clock, es5503_val);
}

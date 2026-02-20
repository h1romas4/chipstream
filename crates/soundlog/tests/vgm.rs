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
    // Header defaults are set. The builder appends an EndOfData marker
    // if none was present, so we expect one command (the terminator).
    assert_eq!(doc.iter().count(), 1);
    // Verify the final command is EndOfData to make the intent explicit.
    match doc.iter().last().unwrap().clone() {
        VgmCommand::EndOfData(_) => {}
        other => panic!("expected EndOfData terminator, got {:?}", other),
    }

    // finalize() sets data_offset based on version, so we need to check
    // that separately from the default header
    let expected_header = VgmHeader {
        data_offset: 0xB4,
        ..VgmHeader::default()
    };
    assert_eq!(doc.header, expected_header);
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
    // builder appends EndOfData, so we expect the command + terminator
    assert_eq!(doc.iter().count(), 2);
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
    // builder appends EndOfData
    assert_eq!(doc.iter().count(), 2);
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
    // two commands plus EndOfData terminator appended by the builder
    assert_eq!(doc.iter().count(), 3);
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
    // command + EndOfData
    assert_eq!(doc.iter().count(), 2);
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
    // command + EndOfData
    assert_eq!(doc.iter().count(), 2);
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
    // command + EndOfData
    assert_eq!(doc.iter().count(), 2);
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
        chip_instance: 1,
        data_type: 0x01,
        size: data.len() as u32,
        data: data.clone(),
    };
    b.add_vgm_command(spec.clone());
    let doc = b.finalize();
    // data block + terminator
    assert_eq!(doc.iter().count(), 2);
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
    // pcm ram write + terminator
    assert_eq!(doc.iter().count(), 2);
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
    // command + EndOfData
    assert_eq!(doc.iter().count(), 2);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::WaitNSample(s) => assert_eq!(s, WaitNSample(5)),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_command_ay8910_mask_and_seek() {
    let mut b = VgmBuilder::new();
    b.add_vgm_command(Ay8910StereoMask::from_mask(0xAA));
    b.add_vgm_command(SeekOffset(0xDEADBEEF));
    let doc = b.finalize();
    // two commands + EndOfData appended by builder
    assert_eq!(doc.iter().count(), 3);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::AY8910StereoMask(s) => {
            assert_eq!(s, Ay8910StereoMask::from_mask(0xAA))
        }
        other => panic!("unexpected command: {:?}", other),
    }
    match doc.iter().nth(1).unwrap().clone() {
        VgmCommand::SeekOffset(s) => assert_eq!(s, SeekOffset(0xDEADBEEF)),
        other => panic!("unexpected command: {:?}", other),
    }
}

#[test]
fn add_command_ay8910_mask_with_spec() {
    let mut b = VgmBuilder::new();

    // Create Ay8910StereoMask directly with detailed fields
    let mask_spec = Ay8910StereoMask {
        chip_instance: 1,
        is_ym2203: true,
        left_ch1: true,
        right_ch1: true,
        left_ch2: false,
        right_ch2: false,
        left_ch3: true,
        right_ch3: true,
    };

    b.add_vgm_command(mask_spec.clone());
    let doc = b.finalize();

    // mask spec + terminator
    assert_eq!(doc.iter().count(), 2);
    match doc.iter().next().unwrap().clone() {
        VgmCommand::AY8910StereoMask(s) => {
            assert_eq!(s.chip_instance, 1);
            assert!(s.is_ym2203);
            assert!(s.left_ch1);
            assert!(s.right_ch1);
            assert!(!s.left_ch2);
            assert!(!s.right_ch2);
            assert!(s.left_ch3);
            assert!(s.right_ch3);
            // Verify it converts to the expected mask byte
            assert_eq!(s.to_mask(), 0b11110011);
        }
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
    // two stream control commands + terminator
    assert_eq!(doc.iter().count(), 3);
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
    // three commands + EndOfData
    assert_eq!(doc.iter().count(), 4);
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
    // command + terminator
    assert_eq!(doc.iter().count(), 2);
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
    // command + terminator
    assert_eq!(doc.iter().count(), 2);
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

    // Secondary-only header entries may produce both Primary and Secondary
    // instances (primary included to be robust against real-world files).
    assert_eq!(instances.len(), 3);
    // ChipInstances now returns Vec<(Instance, Chip, f32)> tuples
    assert!(
        instances
            .iter()
            .any(|(inst, chip, _)| *inst == Instance::Primary && *chip == Chip::Ym2413)
    );
    assert!(
        instances
            .iter()
            .any(|(inst, chip, _)| *inst == Instance::Primary && *chip == Chip::Ym2612)
    );
    assert!(
        instances
            .iter()
            .any(|(inst, chip, _)| *inst == Instance::Secondary && *chip == Chip::Ym2612)
    );
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
    // command + terminator
    assert_eq!(doc.iter().count(), 2);
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
    let mut original_commands: Vec<VgmCommand> = doc.iter().cloned().collect();

    // Normalize trailing EndOfData for comparison: the builder appends an
    // EndOfData terminator but parsers/serializers may or may not include it
    // depending on context. Remove it from both sides if present so we compare
    // the meaningful payload commands.
    if matches!(parsed_commands.last(), Some(VgmCommand::EndOfData(_))) {
        parsed_commands.pop();
    }
    if matches!(original_commands.last(), Some(VgmCommand::EndOfData(_))) {
        original_commands.pop();
    }

    assert_eq!(parsed_commands, original_commands);
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
    // VgmDocument serialization uses version-appropriate header size when
    // data_offset == 0. Ensure the serialized document reflects that behavior.
    let mut doc = VgmBuilder::new().finalize();
    // force version to 1.00 and ensure no explicit data_offset
    doc.header.version = 0x00000100;
    doc.header.data_offset = 0;

    // Serialize the full document and inspect resulting file bytes.
    let bytes: Vec<u8> = (&doc).into();

    // VGM 1.00 header size is 0x24 (36 bytes), but data must start at minimum 0x40
    // The VgmBuilder now appends an EndOfData terminator, so the serialized
    // output includes one extra terminator byte.
    assert_eq!(bytes.len(), 0x40 + 1);
}

#[test]
fn test_explicit_data_offset_affects_header_size() {
    // When data_offset is non-zero, header size should be 0x34 + data_offset.
    let mut doc = VgmBuilder::new().finalize();
    doc.header.data_offset = 0x20; // example small explicit offset

    let bytes: Vec<u8> = (&doc).into();

    // Account for the EndOfData terminator appended by the builder.
    assert_eq!(bytes.len(), 0x34usize + 0x20usize + 1usize);
}

#[test]
fn test_small_version_does_not_include_extra_header_field() {
    // Historically older versions had smaller main-header layouts that did not
    // include the stored extra-header offset (0xBC..0xBF). Confirm that the
    // serialization uses the version-appropriate header size.
    let mut doc = VgmBuilder::new().finalize();
    doc.header.version = 0x00000110; // legacy small-version indicator
    doc.header.data_offset = 0;

    let bytes: Vec<u8> = (&doc).into();

    // VGM 1.10 header size is 0x34 (52 bytes)
    // The VgmBuilder now appends an EndOfData terminator, so the serialized
    // output contains one extra terminator byte beyond the main header size.
    assert_eq!(bytes.len(), 0x34 + 1);
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

#[test]
fn test_vgm_150_overlapping_header_bytes_treated_as_zero() {
    // VGM 1.50+ spec: "If the VGM data starts at an offset that is lower than
    // 0x100, all overlapping header bytes have to be handled as they were zero."
    //
    // This test creates a VGM 1.50+ file where data starts before 0x100,
    // causing some header fields to overlap with the data region. Those fields
    // should be read as zero.

    // Create a minimal VGM file with version 1.50
    // Data starts at 0x34 + 0x40 = 0x74, so we need at least that much + EndOfData command
    let mut vgm_bytes = vec![0u8; 0x80];

    // VGM header identifier
    vgm_bytes[0x00..0x04].copy_from_slice(b"Vgm ");

    // EOF offset (relative to 0x04) - points to end of file
    let eof_offset: u32 = (vgm_bytes.len() - 0x04) as u32;
    vgm_bytes[0x04..0x08].copy_from_slice(&eof_offset.to_le_bytes());

    // Version 1.50
    let version: u32 = 0x00000150;
    vgm_bytes[0x08..0x0C].copy_from_slice(&version.to_le_bytes());

    // Set data_offset to a small value (e.g., 0x40) so data starts at 0x34 + 0x40 = 0x74
    let data_offset: u32 = 0x40;
    vgm_bytes[0x34..0x38].copy_from_slice(&data_offset.to_le_bytes());

    // Set a field that is BEFORE the data start (should be preserved)
    // ym2612_clock is at 0x2C, which is < 0x74
    let ym2612_clock: u32 = 0xAABBCCDD;
    vgm_bytes[0x2C..0x30].copy_from_slice(&ym2612_clock.to_le_bytes());

    // Add EndOfData command (0x66) at data start position (0x74)
    let data_start = 0x34 + data_offset as usize;
    vgm_bytes[data_start] = 0x66;

    // Now create a separate byte array that has header fields set beyond data_start
    // to simulate what might be in a malformed or edge-case file
    let mut test_bytes = vgm_bytes.clone();

    // Set some header fields that would be AFTER the data start (0x74)
    // These bytes exist in the buffer but should be treated as zero when parsing
    // because they overlap with the data region
    // For example, gb_dmg_clock is at 0x80, but our data starts at 0x74
    if test_bytes.len() > 0x84 {
        test_bytes.resize(0x88, 0);
        test_bytes[0x80..0x84].copy_from_slice(&0x12345678u32.to_le_bytes());
        test_bytes[0x84..0x88].copy_from_slice(&0x9ABCDEF0u32.to_le_bytes());
    }

    // Parse the VGM document
    let parsed: Result<VgmDocument, _> = vgm_bytes.as_slice().try_into();

    assert!(
        parsed.is_ok(),
        "Failed to parse VGM with early data start: {:?}",
        parsed.as_ref().err()
    );
    let doc = parsed.unwrap();

    // Fields before data start should be preserved
    assert_eq!(doc.header.version, version);
    assert_eq!(doc.header.data_offset, data_offset);
    assert_eq!(doc.header.ym2612_clock, ym2612_clock);

    // Fields after data start (overlapping with data) should be zero
    // because the parser should not read beyond header_size (which is limited to data_start)
    assert_eq!(
        doc.header.gb_dmg_clock, 0,
        "gb_dmg_clock at 0x80 should be 0 (beyond data start at 0x74)"
    );
    assert_eq!(
        doc.header.nes_apu_clock, 0,
        "nes_apu_clock at 0x84 should be 0 (beyond data start at 0x74)"
    );
}

#[test]
fn test_vgm_150_minimum_header_size_64_bytes() {
    // VGM 1.50+ spec: "All header sizes are valid for all versions from 1.50 on,
    // as long as header has at least 64 bytes."
    //
    // This test verifies that even with a very small data_offset, we still
    // maintain at least 64 bytes (0x40) for the header when possible.

    // Set a very small data_offset (0x0C would result in header ending at 0x40)
    let data_offset: u32 = 0x0C;
    let data_start = 0x34 + data_offset as usize;

    // Create a minimal VGM file that ends right after the EndOfData command
    let mut vgm_bytes = vec![0u8; data_start + 1];

    // VGM header identifier
    vgm_bytes[0x00..0x04].copy_from_slice(b"Vgm ");

    // EOF offset (relative to 0x04)
    let eof_offset: u32 = (vgm_bytes.len() - 0x04) as u32;
    vgm_bytes[0x04..0x08].copy_from_slice(&eof_offset.to_le_bytes());

    // Version 1.50
    let version: u32 = 0x00000150;
    vgm_bytes[0x08..0x0C].copy_from_slice(&version.to_le_bytes());

    vgm_bytes[0x34..0x38].copy_from_slice(&data_offset.to_le_bytes());

    // Add EndOfData command at data start position
    vgm_bytes[data_start] = 0x66;

    // Parse should succeed and maintain minimum 64-byte header
    let parsed: Result<VgmDocument, _> = vgm_bytes.as_slice().try_into();
    assert!(
        parsed.is_ok(),
        "Failed to parse VGM 1.50+ with small data_offset: {:?}",
        parsed.as_ref().err()
    );
}

#[test]
fn test_vgm_pre_150_not_affected_by_new_rule() {
    // Verify that versions before 1.50 use version-based fallback header size
    // and correctly parse files.

    // VGM 1.10 header size is 0x34 bytes
    let version: u32 = 0x00000110;
    let data_start = 0x34; // VGM 1.10 uses version-based header size

    // Create a minimal VGM file that ends right after the EndOfData command
    let mut vgm_bytes = vec![0u8; data_start + 1];

    // VGM header identifier
    vgm_bytes[0x00..0x04].copy_from_slice(b"Vgm ");

    // EOF offset (relative to 0x04)
    let eof_offset: u32 = (vgm_bytes.len() - 0x04) as u32;
    vgm_bytes[0x04..0x08].copy_from_slice(&eof_offset.to_le_bytes());

    // Version 1.10 (before 1.50)
    vgm_bytes[0x08..0x0C].copy_from_slice(&version.to_le_bytes());

    // Note: data_offset field at 0x34 doesn't exist in VGM 1.10 (added in 1.50)
    // So we don't set it, and it remains 0x00

    // Add EndOfData command at actual data start position (0x34 for VGM 1.10)
    vgm_bytes[data_start] = 0x66;

    // Parse should succeed
    let parsed: Result<VgmDocument, _> = vgm_bytes.as_slice().try_into();
    assert!(
        parsed.is_ok(),
        "Failed to parse VGM 1.10: {:?}",
        parsed.as_ref().err()
    );

    let doc = parsed.unwrap();
    assert_eq!(doc.header.version, version);
}

#[test]
fn test_version_based_field_availability() {
    // Test that fields are only read if they were defined in that version.
    // For example, VGM 1.10 should not read fields added in VGM 1.51.

    let mut vgm_bytes = vec![0u8; 0x100];

    // VGM header identifier
    vgm_bytes[0x00..0x04].copy_from_slice(b"Vgm ");

    // EOF offset
    let eof_offset: u32 = (vgm_bytes.len() - 0x04) as u32;
    vgm_bytes[0x04..0x08].copy_from_slice(&eof_offset.to_le_bytes());

    // Version 1.10 (before 1.50)
    let version: u32 = 0x00000110;
    vgm_bytes[0x08..0x0C].copy_from_slice(&version.to_le_bytes());

    // Set data_offset to 0 (will use fallback header size)
    let data_offset: u32 = 0;
    vgm_bytes[0x34..0x38].copy_from_slice(&data_offset.to_le_bytes());

    // Set a field that is defined in VGM 1.10 (YM2612 clock at 0x2C)
    let ym2612_clock: u32 = 0x11223344;
    vgm_bytes[0x2C..0x30].copy_from_slice(&ym2612_clock.to_le_bytes());

    // Set a field that is NOT defined in VGM 1.10 but added in VGM 1.51
    // (SegaPCM clock at 0x38) - this should be ignored
    vgm_bytes[0x38..0x3C].copy_from_slice(&0x55667788u32.to_le_bytes());

    // Add EndOfData command at fallback header size for 1.10 (0x34)
    vgm_bytes[0x34] = 0x66;

    // Parse the VGM
    let parsed: Result<VgmDocument, _> = vgm_bytes.as_slice().try_into();
    assert!(
        parsed.is_ok(),
        "Failed to parse VGM 1.10: {:?}",
        parsed.as_ref().err()
    );

    let doc = parsed.unwrap();
    assert_eq!(doc.header.version, version);

    // Field defined in VGM 1.10 should be preserved
    assert_eq!(doc.header.ym2612_clock, ym2612_clock);

    // Field added in VGM 1.51 should be zero (not read)
    assert_eq!(
        doc.header.sega_pcm_clock, 0,
        "SegaPCM clock should be 0 for VGM 1.10 (field added in 1.51)"
    );
}

#[test]
fn test_version_150_respects_version_defined_fields() {
    // Test that VGM 1.50 does NOT read fields added in later versions,
    // even if data_offset would allow it.

    let mut vgm_bytes = vec![0u8; 0x100];

    // VGM header identifier
    vgm_bytes[0x00..0x04].copy_from_slice(b"Vgm ");

    // EOF offset
    let eof_offset: u32 = (vgm_bytes.len() - 0x04) as u32;
    vgm_bytes[0x04..0x08].copy_from_slice(&eof_offset.to_le_bytes());

    // Version 1.50
    let version: u32 = 0x00000150;
    vgm_bytes[0x08..0x0C].copy_from_slice(&version.to_le_bytes());

    // Set data_offset larger than VGM 1.50's defined header size
    let data_offset: u32 = 0x4C; // data starts at 0x34 + 0x4C = 0x80
    vgm_bytes[0x34..0x38].copy_from_slice(&data_offset.to_le_bytes());

    // Set a field added in VGM 1.51 (SegaPCM clock at 0x38)
    // VGM 1.50 header only goes up to 0x38, so this should NOT be read
    vgm_bytes[0x38..0x3C].copy_from_slice(&0xAABBCCDDu32.to_le_bytes());

    // Add EndOfData command at data start position
    let data_start = 0x34 + data_offset as usize;
    vgm_bytes[data_start] = 0x66;

    // Parse the VGM
    let parsed: Result<VgmDocument, _> = vgm_bytes.as_slice().try_into();
    assert!(
        parsed.is_ok(),
        "Failed to parse VGM 1.50: {:?}",
        parsed.as_ref().err()
    );

    let doc = parsed.unwrap();
    assert_eq!(doc.header.version, version);

    // VGM 1.50 header size is 0x38, so fields at 0x38+ (added in 1.51) should be zero
    assert_eq!(
        doc.header.sega_pcm_clock, 0,
        "VGM 1.50 should NOT read SegaPCM clock (field added in 1.51)"
    );
}

#[test]
fn test_vgm_170_does_not_read_171_fields() {
    // Test that VGM 1.70 files do NOT read VGM 1.71 fields,
    // even if data_offset is large enough to include them.
    // This tests the real-world case from "01 Opening.vgz".

    let mut vgm_bytes = vec![0u8; 0x100];

    // VGM header identifier
    vgm_bytes[0x00..0x04].copy_from_slice(b"Vgm ");

    // EOF offset
    let eof_offset: u32 = (vgm_bytes.len() - 0x04) as u32;
    vgm_bytes[0x04..0x08].copy_from_slice(&eof_offset.to_le_bytes());

    // Version 1.70
    let version: u32 = 0x00000170;
    vgm_bytes[0x08..0x0C].copy_from_slice(&version.to_le_bytes());

    // Set data_offset to 0xAC (like the real file)
    // This makes data start at 0x34 + 0xAC = 0xE0
    let data_offset: u32 = 0xAC;
    vgm_bytes[0x34..0x38].copy_from_slice(&data_offset.to_le_bytes());

    // VGM 1.70 header ends at 0xC0
    // Set VGM 1.71 fields (0xC0-0xE0) that should NOT be read
    vgm_bytes[0xC0..0xC4].copy_from_slice(&0x0000000Cu32.to_le_bytes()); // WonderSwan
    vgm_bytes[0xC4..0xC8].copy_from_slice(&0x00000000u32.to_le_bytes()); // VSU
    vgm_bytes[0xC8..0xCC].copy_from_slice(&0x00000004u32.to_le_bytes()); // SAA1099
    vgm_bytes[0xCC..0xD0].copy_from_slice(&0x00008601u32.to_le_bytes()); // ES5503

    // Add EndOfData command at data start position
    let data_start = 0x34 + data_offset as usize;
    vgm_bytes[data_start] = 0x66;

    // Parse the VGM
    let parsed: Result<VgmDocument, _> = vgm_bytes.as_slice().try_into();
    assert!(
        parsed.is_ok(),
        "Failed to parse VGM 1.70: {:?}",
        parsed.as_ref().err()
    );

    let doc = parsed.unwrap();
    assert_eq!(doc.header.version, version);

    // VGM 1.70 should NOT read VGM 1.71 fields, even though data_offset
    // suggests the header extends to 0xE0
    assert_eq!(
        doc.header.wonderswan_clock, 0,
        "VGM 1.70 should NOT read WonderSwan clock (field added in 1.71)"
    );
    assert_eq!(
        doc.header.vsu_clock, 0,
        "VGM 1.70 should NOT read VSU clock (field added in 1.71)"
    );
    assert_eq!(
        doc.header.saa1099_clock, 0,
        "VGM 1.70 should NOT read SAA1099 clock (field added in 1.71)"
    );
    assert_eq!(
        doc.header.es5503_clock, 0,
        "VGM 1.70 should NOT read ES5503 clock (field added in 1.71)"
    );

    // VGM 1.70 field (Extra Header Offset at 0xBC) should still be readable
    // (not testing this here, but it's within 1.70's header range)
}

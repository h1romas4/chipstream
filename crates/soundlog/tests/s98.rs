#![cfg(feature = "s98")]

use soundlog::s98::{
    S98Builder, S98Command, S98Device, S98DeviceType, S98Document, S98Header, S98ParseLimits,
    S98PlaybackError, S98PlaybackOptions, S98Port, S98Version, S98VgmGenerator,
};
use soundlog::vgm::command::{Instance, VgmCommand};
use soundlog::{ParseError, VgmCommandGenerator};
use std::cell::RefCell;
use std::rc::Rc;

fn write(device: u8, port: S98Port, register: u8, value: u8) -> S98Command {
    S98Command::Write {
        device,
        port,
        register,
        value,
    }
}

fn fixture(version: u8) -> Vec<u8> {
    let mut bytes = vec![0; if version == b'3' { 0x30 } else { 0x40 }];
    bytes[..4].copy_from_slice(&[b'S', b'9', b'8', version]);
    bytes[4..8].copy_from_slice(&1u32.to_le_bytes());
    if version == b'3' {
        bytes[0x1c..0x20].copy_from_slice(&1u32.to_le_bytes());
    }
    bytes[0x20..0x24].copy_from_slice(&4u32.to_le_bytes());
    bytes[0x24..0x28].copy_from_slice(&7_987_200u32.to_le_bytes());
    let start = bytes.len() as u32;
    bytes[0x14..0x18].copy_from_slice(&start.to_le_bytes());
    bytes[0x18..0x1c].copy_from_slice(&(start + 3).to_le_bytes());
    bytes.extend_from_slice(&[0x01, 0x10, 0x80, 0xfe, 0x80, 0x01, 0xff, 0xfd]);
    let tag_offset = bytes.len() as u32;
    bytes[0x10..0x14].copy_from_slice(&tag_offset.to_le_bytes());
    bytes.extend_from_slice(if version == b'3' {
        b"[S98]\xef\xbb\xbftitle=test\n"
    } else {
        b"\x82\xa0"
    });
    bytes
}

#[test]
fn versions_parse_and_serialize_semantics() {
    for (byte, version) in [
        (b'1', S98Version::V1),
        (b'2', S98Version::V2),
        (b'3', S98Version::V3),
    ] {
        let document = S98Document::parse(&fixture(byte)).unwrap();
        assert_eq!(document.header.version, version);
        assert_eq!(
            document.header.devices,
            vec![S98Device::new(S98DeviceType::Ym2608, 7_987_200)]
        );
        assert_eq!(
            document.commands,
            vec![
                write(0, S98Port::Extended, 0x10, 0x80),
                S98Command::Wait(130),
                S98Command::Wait(1)
            ]
        );
        assert_eq!(document.loop_index, Some(1));
        assert_eq!(
            S98Document::parse(&document.to_bytes().unwrap()).unwrap(),
            document
        );
    }
}

#[test]
fn malformed_inputs_and_resource_limits_are_rejected() {
    let input = fixture(b'3');
    let mut bad = input.clone();
    bad[0x18..0x1c].copy_from_slice(&0x31u32.to_le_bytes());
    assert!(S98Document::parse(&bad).is_err());
    let mut bad = input.clone();
    bad[0x1c..0x20].copy_from_slice(&65u32.to_le_bytes());
    assert!(S98Document::parse(&bad).is_err());
    let mut bad = input.clone();
    bad[0x0c] = 1;
    assert!(S98Document::parse(&bad).is_err());
    let mut bad = input.clone();
    bad[0x10..0x14].copy_from_slice(&0x30u32.to_le_bytes());
    assert!(S98Document::parse(&bad).is_err());
    let mut bad = input.clone();
    bad[0x30..0x38].copy_from_slice(&[0xfe, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    assert!(S98Document::parse(&bad).is_err());
    let mut bad = input.clone();
    bad[0x30] = 2;
    assert!(S98Document::parse(&bad).is_err());
    for limits in [
        S98ParseLimits {
            max_file_bytes: Some(input.len() - 1),
            ..Default::default()
        },
        S98ParseLimits {
            max_commands: Some(2),
            ..Default::default()
        },
        S98ParseLimits {
            max_tag_bytes: Some(1),
            ..Default::default()
        },
    ] {
        assert!(S98Document::parse_with_limits(&input, limits).is_err());
    }
    let mut document = S98Document::parse(&input).unwrap();
    document.commands.push(S98Command::Wait(0));
    assert!(document.to_bytes().is_err());
}

#[test]
fn default_device_and_legacy_title_before_dump() {
    let mut bytes = vec![0u8; 0x30];
    bytes[..4].copy_from_slice(b"S981");
    bytes[0x10..0x14].copy_from_slice(&0x20u32.to_le_bytes());
    bytes[0x14..0x18].copy_from_slice(&0x30u32.to_le_bytes());
    bytes[0x20..0x23].copy_from_slice(&[0x82, 0xa0, 0]);
    bytes.extend_from_slice(&[0, 0x22, 8, 0xff, 0xfd]);
    let document = S98Document::parse(&bytes).unwrap();
    assert_eq!(document.tag.as_deref(), Some(&[0x82, 0xa0][..]));
    assert!(document.header.devices.is_empty());
    let mut generator = S98VgmGenerator::new(document, Default::default()).unwrap();
    assert_eq!(generator.vgm_header().ym2608_clock, 7_987_200);
    assert!(matches!(
        generator.next_command().unwrap(),
        Some(VgmCommand::Ym2608Write(Instance::Primary, _))
    ));
    assert!(
        matches!(generator.next_command().unwrap(), Some(VgmCommand::WaitSamples(w)) if w.0 == 441)
    );
}

#[test]
fn fractional_waits_and_loop_events_preserve_timeline() {
    let mut header = S98Header::new(S98Version::V3);
    header.timer_numerator = 1;
    let mut builder = S98Builder::new(header);
    builder.mark_loop();
    for _ in 0..10 {
        builder.push(S98Command::Wait(1));
    }
    let document = builder.finalize().unwrap();
    let events = Rc::new(RefCell::new(Vec::new()));
    let captured = events.clone();
    let mut generator = S98VgmGenerator::new(
        document,
        S98PlaybackOptions {
            loop_count: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    generator.set_observer(move |event| {
        captured.borrow_mut().push((event.sample, event.data));
        Ok(())
    });
    let mut waits = Vec::new();
    while let Some(command) = generator.next_command().unwrap() {
        if let VgmCommand::WaitSamples(wait) = command {
            waits.push(wait.0);
        }
    }
    assert_eq!(waits.iter().map(|&x| u64::from(x)).sum::<u64>(), 1323);
    assert_eq!(events.borrow()[10], (441, 0));
    assert_eq!(events.borrow()[20], (882, 0));
    assert!(generator.finished_at_loop());
}

#[test]
fn loop_marker_large_waits_and_playback_limits() {
    let mut builder = S98Builder::new(S98Header::new(S98Version::V3));
    builder.mark_loop().push(S98Command::Wait(200));
    let document = builder.finalize().unwrap();
    let (mut generator, marker) =
        S98VgmGenerator::new_with_loop_marker(document.clone(), Default::default()).unwrap();
    assert!(
        matches!(generator.next_command().unwrap(), Some(VgmCommand::WaitSamples(w)) if w.0 == 65535)
    );
    assert!(
        matches!(generator.next_command().unwrap(), Some(VgmCommand::WaitSamples(w)) if w.0 == 22665)
    );
    assert!(generator.next_command().unwrap().is_some());
    assert_eq!(marker.sample(), Some(88_200));
    let mut limited = S98VgmGenerator::new(
        document,
        S98PlaybackOptions {
            max_ticks: Some(199),
            ..Default::default()
        },
    )
    .unwrap();
    let error = limited.next_command().unwrap_err();
    assert!(
        matches!(error, ParseError::GeneratorError(source) if matches!(source.downcast_ref::<S98PlaybackError>(), Some(S98PlaybackError::TickLimitExceeded { max_ticks: 199 })))
    );
    let mut empty = S98Builder::new(S98Header::new(S98Version::V3));
    empty.mark_loop();
    let mut looping = S98VgmGenerator::new(
        empty.finalize().unwrap(),
        S98PlaybackOptions {
            loop_count: None,
            max_commands_without_wait: Some(3),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(matches!(
        looping.next_command(),
        Err(ParseError::GeneratorError(_))
    ));
    let mut once = S98Builder::new(S98Header::new(S98Version::V3));
    once.push(write(0, S98Port::Normal, 0x22, 8));
    let mut generator = S98VgmGenerator::new(
        once.finalize().unwrap(),
        S98PlaybackOptions {
            max_commands_without_wait: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(generator.next_command().unwrap().is_some());
    assert!(generator.next_command().unwrap().is_none());
}

#[test]
fn device_configuration_rejects_unrepresentable_cases() {
    let mut header = S98Header::new(S98Version::V3);
    header.devices = vec![
        S98Device::new(S98DeviceType::None, 0),
        S98Device::new(S98DeviceType::Ym2608, 8_000_000),
        S98Device::new(S98DeviceType::Ym2608, 8_000_000),
    ];
    let mut builder = S98Builder::new(header.clone());
    builder.push(write(2, S98Port::Normal, 0x22, 0));
    let mut generator =
        S98VgmGenerator::new(builder.finalize().unwrap(), Default::default()).unwrap();
    assert_eq!(generator.vgm_header().ym2608_clock, 0x4000_0000 | 8_000_000);
    assert!(matches!(
        generator.next_command().unwrap(),
        Some(VgmCommand::Ym2608Write(Instance::Secondary, _))
    ));
    header.devices[2].clock += 1;
    assert!(
        S98VgmGenerator::new(
            S98Builder::new(header.clone()).finalize().unwrap(),
            Default::default()
        )
        .is_err()
    );
    header.devices[2].clock -= 1;
    header.devices[1].pan = 1;
    assert!(
        S98VgmGenerator::new(
            S98Builder::new(header).finalize().unwrap(),
            Default::default()
        )
        .is_err()
    );
}

#[test]
fn unknown_devices_round_trip_and_none_writes_are_ignored() {
    let mut header = S98Header::new(S98Version::V3);
    header.devices = vec![
        S98Device::new(S98DeviceType::None, 0),
        S98Device::new(S98DeviceType::Unknown(1234), 1),
    ];
    let mut builder = S98Builder::new(header);
    builder.push(write(0, S98Port::Normal, 1, 2));
    let document = builder.finalize().unwrap();
    assert_eq!(
        S98Document::parse(&document.to_bytes().unwrap()).unwrap(),
        document
    );
    assert!(S98VgmGenerator::new(document, Default::default()).is_err());

    let mut header = S98Header::new(S98Version::V3);
    header.devices.push(S98Device::new(S98DeviceType::None, 0));
    let mut builder = S98Builder::new(header);
    builder
        .push(write(0, S98Port::Extended, 1, 2))
        .push(S98Command::Wait(1));
    let mut generator =
        S98VgmGenerator::new(builder.finalize().unwrap(), Default::default()).unwrap();
    assert!(matches!(
        generator.next_command().unwrap(),
        Some(VgmCommand::WaitSamples(_))
    ));
}

#[test]
fn observer_errors_are_returned() {
    let mut builder = S98Builder::new(S98Header::new(S98Version::V3));
    builder.push(S98Command::Wait(1));
    let mut generator =
        S98VgmGenerator::new(builder.finalize().unwrap(), Default::default()).unwrap();
    generator.set_observer(|_| Err(ParseError::DataInconsistency("observer failed".into())));
    assert!(matches!(
        generator.next_command(),
        Err(ParseError::DataInconsistency(message)) if message == "observer failed"
    ));
}

#[test]
#[ignore = "set S98_TEST_DIR to a local collection to run this compatibility check"]
fn local_collection_parses_and_round_trips() {
    fn visit(path: &std::path::Path, count: &mut usize) {
        for item in std::fs::read_dir(path).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                visit(&path, count);
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("s98"))
            {
                let document = S98Document::parse(&std::fs::read(&path).unwrap()).unwrap();
                let rebuilt = document.to_bytes().unwrap();
                assert_eq!(S98Document::parse(&rebuilt).unwrap(), document);
                let mut generator = S98VgmGenerator::new(document, Default::default()).unwrap();
                while generator.next_command().unwrap().is_some() {}
                *count += 1;
            }
        }
    }
    let mut count = 0;
    visit(
        std::path::Path::new(&std::env::var("S98_TEST_DIR").expect("S98_TEST_DIR")),
        &mut count,
    );
    assert!(count > 0);
    eprintln!("checked {count} local S98 files");
}

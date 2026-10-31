use ndsif::{
    AudioEvent, AudioStatus, BytePosition, Capabilities, Command, Divider, Error, Frame,
    MessageType, OkiClock, Pan, RegisterWrite, Reply, Request, RequestEvent, Response, Status,
    StatusFlags, WriteId, ZeroPair,
};

#[test]
fn controls_and_generic_register_writes_use_message_types_and_vgm_ids() {
    let volume = Command::SetOutputVolume {
        attenuation: 12,
        response: true,
    }
    .to_frame(0x1234)
    .unwrap();
    assert_eq!(volume.message_type(), MessageType::Request);
    assert_eq!(volume.command_id(), 0x03);
    assert_eq!(volume.transaction_id(), 0x1234);
    assert_eq!(volume.payload(), &[12]);
    assert_eq!(
        Request::decode(&volume).unwrap(),
        Request::SetOutputVolume { attenuation: 12 }
    );

    let clock = Command::SetChipClock {
        write_id: WriteId::YM2151,
        instance: 0,
        hz: 3_579_545,
        response: false,
    }
    .to_frame(7)
    .unwrap();
    assert_eq!(clock.message_type(), MessageType::NoResponse);
    assert_eq!(clock.command_id(), 0x04);
    assert_eq!(
        Request::decode(&clock).unwrap(),
        Request::SetChipClock {
            write_id: WriteId::YM2151,
            instance: 0,
            hz: 3_579_545
        }
    );

    let writes = [
        RegisterWrite::new(0x20, 0xc7),
        RegisterWrite::new(0x08, 0x78),
    ];
    let frame = Command::WriteRegisters {
        write_id: WriteId::YM2151,
        instance: 0,
        page: 0,
        writes: &writes,
    }
    .to_frame(0x4321)
    .unwrap();
    assert_eq!(frame.message_type(), MessageType::NoResponse);
    assert_eq!(frame.command_id(), 0x54);
    assert_eq!(frame.payload(), &[0, 0, 0x20, 0xc7, 0x08, 0x78]);
    let Request::WriteRegisters {
        write_id,
        instance,
        page,
        writes: decoded_writes,
    } = Request::decode(&frame).unwrap()
    else {
        panic!("expected register burst");
    };
    assert_eq!(write_id, WriteId::YM2151);
    assert_eq!((instance, page), (0, 0));
    assert_eq!(decoded_writes.iter().collect::<Vec<_>>(), writes);
}

#[test]
fn audio_stream_commands_encode_generic_target_and_event_payloads() {
    let adpcm = [0x12, 0x34];
    let data = Command::AudioData {
        stream_id: 0xb7,
        instance: 1,
        position: BytePosition::new(0x1234),
        adpcm: &adpcm,
    }
    .to_frame(8)
    .unwrap();
    assert_eq!(data.command_id(), 0x10);
    assert_eq!(data.payload(), &[0xb7, 1, 0x34, 0x12, 0, 0, 0x12, 0x34]);
    assert!(matches!(
        Request::decode(&data).unwrap(),
        Request::AudioData {
            stream_id: 0xb7,
            instance: 1,
            position,
            adpcm: bytes
        } if position.get() == 0x1234 && bytes == adpcm
    ));

    let writes = [RegisterWrite::new(0x28, 0xf0)];
    let event = Command::AudioEvent {
        stream_id: 0xb7,
        instance: 0,
        position: BytePosition::new(19),
        event: AudioEvent::Registers {
            write_id: WriteId::YM2151,
            instance: 1,
            page: 0,
            writes: &writes,
        },
    }
    .to_frame(9)
    .unwrap();
    assert_eq!(event.command_id(), 0x11);
    assert_eq!(
        event.payload(),
        &[0xb7, 0, 19, 0, 0, 0, 0, 0x54, 1, 0, 0x28, 0xf0]
    );
    assert!(matches!(
        Request::decode(&event).unwrap(),
        Request::AudioEvent {
            event: RequestEvent::Registers {
                write_id,
                instance: 1,
                page: 0,
                writes,
            },
            ..
        } if write_id == WriteId::YM2151 && writes.iter().next() == Some(writes_item())
    ));

    for (event, expected) in [
        (
            AudioEvent::Clock {
                clock: OkiClock::Mhz8,
                divider: Divider::Div512,
            },
            1,
        ),
        (AudioEvent::Pan(Pan::Left), 2),
        (AudioEvent::End(ZeroPair::Byte80), 3),
    ] {
        let frame = Command::AudioEvent {
            stream_id: 0xb7,
            instance: 0,
            position: BytePosition::new(20),
            event,
        }
        .to_frame(10)
        .unwrap();
        assert_eq!(frame.payload()[6], expected);
        assert!(matches!(
            Request::decode(&frame),
            Ok(Request::AudioEvent { .. })
        ));
    }

    let start = Command::AudioStart {
        stream_id: 0xb7,
        instance: 0,
        clock: OkiClock::Mhz4,
        divider: Divider::Div768,
    }
    .to_frame(11)
    .unwrap();
    assert_eq!(start.payload(), &[0xb7, 0, 0, 9, 0x3d, 0, 0, 0x03]);
    assert!(matches!(
        Request::decode(&start),
        Ok(Request::AudioStart { .. })
    ));

    let status = Command::AudioStatus {
        stream_id: 0xb7,
        instance: 0,
    }
    .to_frame(12)
    .unwrap();
    assert_eq!(status.message_type(), MessageType::Request);
    assert_eq!(status.payload(), &[0xb7, 0]);
}

#[test]
fn response_correlates_by_command_and_transaction_id() {
    let request = Command::Ping(b"ND8").to_frame(0x3412).unwrap();
    let response = Response::for_request(&request, Reply::Ping(b"ND8")).unwrap();
    let frame = response.to_frame().unwrap();
    assert_eq!(frame.message_type(), MessageType::Response);
    assert_eq!(frame.command_id(), 0x01);
    assert_eq!(frame.transaction_id(), 0x3412);
    assert_eq!(frame.payload(), b"\0ND8");
    let decoded = Response::decode(&frame).unwrap();
    assert_eq!(decoded.reply, Reply::Ping(b"ND8"));
    decoded.matches_request(&request).unwrap();

    let wrong_transaction = Frame::new(MessageType::Response, 1, 0x3413, b"\0ND8").unwrap();
    assert_eq!(
        Response::decode(&wrong_transaction)
            .unwrap()
            .matches_request(&request),
        Err(Error::ResponseMismatch)
    );
    let legacy_id_response = Frame::new(MessageType::Response, 0x81, 0x3412, b"\0ND8").unwrap();
    assert_eq!(
        Response::decode(&legacy_id_response)
            .unwrap()
            .matches_request(&request),
        Err(Error::ResponseMismatch)
    );
}

#[test]
fn info_capabilities_and_audio_status_decode_the_draft_payload_shapes() {
    let info_request = Command::GetInfo.to_frame(42).unwrap();
    let info = Response::for_request(
        &info_request,
        Reply::Info(ndsif::DeviceInfo {
            model: "YM2151 host",
            firmware: "v1-draft",
        }),
    )
    .unwrap();
    assert_eq!(
        Response::decode(&info.to_frame().unwrap()).unwrap().reply,
        info.reply
    );

    let capabilities_request = Command::GetCapabilities { offset: 0 }.to_frame(43).unwrap();
    let entries = [
        0x54, 0, 0, 0x07, 2, 0, 0, 0, 0, 0xb7, 0, 0, 0x0c, 2, 0, 0, 0, 0,
    ];
    let capabilities = Capabilities::new(256, 4096, 128, 1, u16::MAX, &entries).unwrap();
    let response =
        Response::for_request(&capabilities_request, Reply::Capabilities(capabilities)).unwrap();
    let response_frame = response.to_frame().unwrap();
    let decoded = Response::decode(&response_frame).unwrap();
    let Reply::Capabilities(capabilities) = decoded.reply else {
        panic!("expected capabilities");
    };
    assert_eq!(capabilities.max_payload, 256);
    assert_eq!(capabilities.data_capacity, 4096);
    assert_eq!(capabilities.event_capacity, 128);
    assert_eq!(capabilities.control_features, 1);
    assert_eq!(capabilities.next_offset, u16::MAX);
    assert_eq!(
        capabilities.entries().collect::<Vec<_>>(),
        [
            ndsif::CapabilityEntry {
                write_id: 0x54,
                instance: 0,
                page: 0,
                features: 0x07,
                clock_mode: ndsif::ClockMode::Variable,
                fixed_clock_hz: 0,
            },
            ndsif::CapabilityEntry {
                write_id: 0xb7,
                instance: 0,
                page: 0,
                features: 0x0c,
                clock_mode: ndsif::ClockMode::Variable,
                fixed_clock_hz: 0,
            }
        ]
    );

    let status_request = Command::AudioStatus {
        stream_id: 0xb7,
        instance: 0,
    }
    .to_frame(44)
    .unwrap();
    let status = AudioStatus {
        accepted: 100,
        played: 80,
        pending: 20,
        underflows: 0,
        overflows: 1,
        rejected: 2,
        max_pending: 80,
        flags: StatusFlags::from_bits_retain(0x204),
        late_events: 3,
        max_event_lag: 4,
    };
    let response = Response::for_request(&status_request, Reply::AudioStatus(status)).unwrap();
    assert_eq!(
        Response::decode(&response.to_frame().unwrap())
            .unwrap()
            .reply,
        Reply::AudioStatus(status)
    );
}

#[test]
fn capability_entries_enforce_clock_modes_features_and_page_consistency() {
    use ndsif::{ClockMode, MAX_CAPABILITY_ENTRIES, SPEC_REVISION};

    assert_eq!(SPEC_REVISION, 39);
    assert_eq!(MAX_CAPABILITY_ENTRIES, 26);
    let fixed = [0x54, 0, 0, 0x03, 1, 0x99, 0x9e, 0x36, 0];
    let capabilities = Capabilities::new(256, 0, 0, 0, u16::MAX, &fixed).unwrap();
    assert_eq!(
        capabilities.entries().next().unwrap(),
        ndsif::CapabilityEntry {
            write_id: 0x54,
            instance: 0,
            page: 0,
            features: 0x03,
            clock_mode: ClockMode::Fixed,
            fixed_clock_hz: 3_579_545,
        }
    );

    for invalid in [
        [0x54, 0, 0, 0x07, 1, 0x99, 0x9e, 0x36, 0],
        [0x54, 0, 0, 0x03, 1, 0, 0, 0, 0],
        [0x54, 0, 0, 0x03, 2, 0, 0, 0, 0],
        [0x54, 0, 0, 0x07, 2, 1, 0, 0, 0],
    ] {
        assert_eq!(
            Capabilities::new(256, 0, 0, 0, u16::MAX, &invalid),
            Err(Error::InvalidArgument)
        );
    }

    let inconsistent_pages = [
        0x54, 0, 0, 0x07, 2, 0, 0, 0, 0, 0x54, 0, 1, 0x03, 1, 0x99, 0x9e, 0x36, 0,
    ];
    assert_eq!(
        Capabilities::new(256, 0, 0, 0, u16::MAX, &inconsistent_pages),
        Err(Error::InvalidArgument)
    );
}

#[test]
fn rejects_request_type_mismatches_and_invalid_vgm_register_ids() {
    for (message_type, command_id, payload) in [
        (MessageType::Request, 0x54, &[0, 0, 0x20, 0x01][..]),
        (MessageType::NoResponse, 0x01, b""),
        (MessageType::Request, 0x05, &[0][..]),
        (MessageType::NoResponse, 0x10, &[0; 6][..]),
        (MessageType::NoResponse, 0x11, &[0; 7][..]),
    ] {
        let frame = Frame::new(message_type, command_id, 0, payload).unwrap();
        assert!(Request::decode(&frame).is_err());
    }
    for id in [0x30, 0x50] {
        let write_id = WriteId::try_from(id).unwrap();
        assert!(
            Command::WriteRegisters {
                write_id,
                instance: 0,
                page: 0,
                writes: &[RegisterWrite::new(0, 0)],
            }
            .to_frame(0)
            .is_err()
        );
    }
    assert!(
        Command::SetOutputVolume {
            attenuation: 97,
            response: true,
        }
        .to_frame(0)
        .is_err()
    );
    assert!(
        Command::AudioData {
            stream_id: 0xb7,
            instance: 0,
            position: BytePosition::new(u32::MAX),
            adpcm: &[1],
        }
        .to_frame(0)
        .is_err()
    );
}

#[test]
fn rejection_responses_are_status_only_and_require_a_response_request() {
    let request = Command::GetInfo.to_frame(7).unwrap();
    let frame = Response::for_request(&request, Reply::Rejected)
        .unwrap()
        .to_frame()
        .unwrap();
    assert_eq!(frame.payload(), &[1]);
    assert_eq!(Response::decode(&frame).unwrap().status, Status::Rejected);

    let no_response = Command::WriteRegisters {
        write_id: WriteId::YM2151,
        instance: 0,
        page: 0,
        writes: &[RegisterWrite::new(0x20, 0xc7)],
    }
    .to_frame(8)
    .unwrap();
    assert_eq!(
        Response::for_request(&no_response, Reply::Rejected),
        Err(Error::ResponseMismatch)
    );
}

fn writes_item() -> RegisterWrite {
    RegisterWrite::new(0x28, 0xf0)
}

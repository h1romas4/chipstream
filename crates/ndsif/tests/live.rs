use ndsif::{
    Command, Error, Frame, LiveDacCapabilities, LiveDacRecord, MessageType,
    OrderedLiveCapabilities, OrderedLiveRecord, OrderedLiveStatus, OrderedLiveWrite, Reply,
    Request, Response,
};

#[test]
fn live_dac_vectors_and_roundtrip() {
    let records = [
        LiveDacRecord {
            wait_ticks: 0,
            value: 0x80,
        },
        LiveDacRecord {
            wait_ticks: 4,
            value: 0x84,
        },
        LiveDacRecord {
            wait_ticks: 0,
            value: 0x84,
        },
    ];
    let frame = Command::LiveDacData {
        instance: 0,
        records: &records,
    }
    .to_frame(12)
    .unwrap();
    assert_eq!(frame.command_id(), 0x1f);
    assert_eq!(frame.message_type(), MessageType::NoResponse);
    assert_eq!(
        frame.payload(),
        &[
            2, 0x52, 0, 0, 0, 0, 0, 0x80, 4, 0, 0, 0, 0x84, 0, 0, 0, 0, 0x84
        ]
    );
    let Request::LiveDacData {
        instance,
        records: decoded,
    } = Request::decode(&frame).unwrap()
    else {
        panic!("wrong request")
    };
    assert_eq!(instance, 0);
    assert_eq!(decoded.iter().collect::<Vec<_>>(), records);
    assert!(
        Command::LiveDacData {
            instance: 0,
            records: &[]
        }
        .to_frame(0)
        .is_err()
    );
    assert!(
        Command::LiveDacData {
            instance: 0,
            records: &[records[0]; 51]
        }
        .to_frame(0)
        .is_err()
    );
    for payload in [
        &[2, 0x52, 0][..],
        &[2, 0x53, 0, 0, 0, 0, 0, 1][..],
        &[2, 0x52, 0, 0][..],
    ] {
        assert!(
            Request::decode(&Frame::new(MessageType::NoResponse, 0x1f, 0, payload).unwrap())
                .is_err()
        );
    }
}

#[test]
fn ordered_vectors_cover_all_tags_and_preserve_order() {
    let records = [
        OrderedLiveRecord {
            wait_ticks: 0,
            write: OrderedLiveWrite::Dac(0x80),
        },
        OrderedLiveRecord {
            wait_ticks: 253,
            write: OrderedLiveWrite::Dac(0x80),
        },
        OrderedLiveRecord {
            wait_ticks: 254,
            write: OrderedLiveWrite::Dac(0x81),
        },
        OrderedLiveRecord {
            wait_ticks: 0x01020304,
            write: OrderedLiveWrite::Register {
                page: 1,
                address: 0x28,
                value: 0xf0,
            },
        },
        OrderedLiveRecord {
            wait_ticks: 0,
            write: OrderedLiveWrite::Psg(0x90),
        },
    ];
    let frame = Command::OrderedLiveData {
        sequence: u32::MAX,
        records: &records,
    }
    .to_frame(7)
    .unwrap();
    assert_eq!(
        frame.payload(),
        &[
            3, 0xff, 0xff, 0xff, 0xff, 0, 0x80, 0xfd, 0x80, 0xfe, 0xfe, 0, 0, 0, 0x81, 0xff, 4, 3,
            2, 1, 1, 0x28, 0xf0, 0xff, 0, 0, 0, 0, 2, 0, 0x90,
        ]
    );
    let Request::OrderedLiveData {
        sequence,
        records: decoded,
    } = Request::decode(&frame).unwrap()
    else {
        panic!("wrong request")
    };
    assert_eq!(sequence, u32::MAX);
    assert_eq!(decoded.len(), records.len());
    assert_eq!(decoded.iter().collect::<Vec<_>>(), records);
    let long_dac = Frame::new(
        MessageType::NoResponse,
        0x22,
        0,
        &[3, 0, 0, 0, 0, 0xff, 1, 0, 0, 0, 0, 0x2a, 0x80],
    )
    .unwrap();
    let Request::OrderedLiveData { records, .. } = Request::decode(&long_dac).unwrap() else {
        panic!("wrong request")
    };
    assert_eq!(
        records.iter().next().unwrap().write,
        OrderedLiveWrite::Dac(0x80)
    );
}

#[test]
fn ordered_rejects_malformed_and_oversized_records() {
    for tail in [
        &[][..],
        &[0xfe, 1, 2][..],
        &[0xff, 0, 0, 0, 0, 2, 1, 9][..],
        &[0xff, 0, 0, 0, 0, 3, 0, 9][..],
    ] {
        let mut payload = vec![3, 0, 0, 0, 0];
        payload.extend_from_slice(tail);
        assert!(
            Request::decode(&Frame::new(MessageType::NoResponse, 0x22, 0, &payload).unwrap())
                .is_err()
        );
    }
    let record = OrderedLiveRecord {
        wait_ticks: 0,
        write: OrderedLiveWrite::Psg(1),
    };
    assert!(
        Command::OrderedLiveData {
            sequence: 0,
            records: &[record; 32]
        }
        .to_frame(0)
        .is_err()
    );
    assert!(
        Command::OrderedLiveData {
            sequence: 0,
            records: &[record; 101]
        }
        .to_frame(0)
        .is_err()
    );
    assert!(
        Command::OrderedLiveData {
            sequence: 0,
            records: &[OrderedLiveRecord {
                wait_ticks: 0,
                write: OrderedLiveWrite::Register {
                    page: 0,
                    address: 0x2a,
                    value: 0
                }
            }]
        }
        .to_frame(0)
        .is_err()
    );
    let wrong_type = Frame::new(MessageType::Request, 0x22, 0, &[3, 0, 0, 0, 0, 0, 0]).unwrap();
    assert!(matches!(
        Request::decode(&wrong_type),
        Err(Error::InvalidMessageType(_))
    ));
}

#[test]
fn capabilities_and_status_response_vectors() {
    let entries = [0x52, 0, 0x52, 1];
    let cap = LiveDacCapabilities::new(2, 1, 44_100, 512, 50, u16::MAX, &entries).unwrap();
    let request = Command::GetLiveDacCapabilities { offset: 0 }
        .to_frame(9)
        .unwrap();
    let frame = Response::for_request(&request, Reply::LiveDacCapabilities(cap))
        .unwrap()
        .to_frame()
        .unwrap();
    assert_eq!(
        frame.payload(),
        &[
            0, 2, 1, 0x44, 0xac, 0, 0, 0, 2, 0, 0, 50, 0xff, 0xff, 2, 0x52, 0, 0x52, 1
        ]
    );
    let Reply::LiveDacCapabilities(decoded) = Response::decode(&frame).unwrap().reply else {
        panic!("wrong reply")
    };
    assert_eq!(decoded, cap);
    assert_eq!(
        decoded.entries().collect::<Vec<_>>(),
        [(0x52, 0), (0x52, 1)]
    );
    assert!(LiveDacCapabilities::new(2, 1, 44_100, 512, 50, 0, &[0x52, 0, 0x52, 0]).is_err());

    let cap = OrderedLiveCapabilities {
        profile: 3,
        revision: 5,
        ticks_per_second: 44_100,
        record_capacity: 512,
        max_records: 100,
    };
    let request = Command::GetOrderedLiveCapabilities.to_frame(10).unwrap();
    let frame = Response::for_request(&request, Reply::OrderedLiveCapabilities(cap))
        .unwrap()
        .to_frame()
        .unwrap();
    assert_eq!(frame.payload(), &[0, 3, 5, 0x44, 0xac, 0, 0, 0, 2, 100]);
    assert_eq!(
        Response::decode(&frame).unwrap().reply,
        Reply::OrderedLiveCapabilities(cap)
    );
    let status = OrderedLiveStatus {
        profile: 3,
        next_sequence: 4,
        accepted_records: 5,
        queued: 2,
        free: 510,
        missing_frames: 1,
    };
    let request = Command::GetOrderedLiveStatus.to_frame(11).unwrap();
    let frame = Response::for_request(&request, Reply::OrderedLiveStatus(status))
        .unwrap()
        .to_frame()
        .unwrap();
    assert_eq!(
        frame.payload(),
        &[0, 3, 4, 0, 0, 0, 5, 0, 0, 0, 2, 0, 0xfe, 1, 1, 0, 0, 0]
    );
    assert_eq!(
        Response::decode(&frame).unwrap().reply,
        Reply::OrderedLiveStatus(status)
    );
    let malformed = Frame::new(MessageType::Response, 0x21, 0, &[0, 3, 5, 0]).unwrap();
    assert!(Response::decode(&malformed).is_err());
}

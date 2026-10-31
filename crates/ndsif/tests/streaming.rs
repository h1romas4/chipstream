use ndsif::{
    AudioEvent, BytePosition, Command, CommandEncoder, Decoder, Error, Frame, MAX_AUDIO_DATA_BYTES,
    MAX_REGISTER_EVENT_WRITES, MAX_REGISTER_WRITES, MessageType, PARTIAL_FRAME_TIMEOUT_MS,
    RegisterWrite, Request, WriteId,
};

#[test]
fn batch_encoder_splits_register_events_and_audio_data() {
    let writes: Vec<_> = (0..MAX_REGISTER_WRITES * 2 + 1)
        .map(|index| RegisterWrite::new(index as u8, 0xaa))
        .collect();
    let event_writes: Vec<_> = (0..MAX_REGISTER_EVENT_WRITES * 2 + 1)
        .map(|index| RegisterWrite::new(index as u8, 0x55))
        .collect();
    let adpcm = vec![0x12; MAX_AUDIO_DATA_BYTES * 2 + 1];
    let mut wire = Vec::new();
    let count;
    {
        let mut encoder =
            CommandEncoder::new(u16::MAX, |frame: &[u8]| wire.extend_from_slice(frame));
        encoder
            .register_burst(WriteId::YM2151, 0, 0, &writes)
            .unwrap();
        encoder
            .register_event(
                0xb7,
                0,
                BytePosition::new(7),
                WriteId::YM2151,
                0,
                0,
                &event_writes,
            )
            .unwrap();
        assert_eq!(
            encoder
                .audio_data(0xb7, 0, BytePosition::new(7), &adpcm)
                .unwrap()
                .get(),
            (7 + adpcm.len()) as u32
        );
        assert_eq!(encoder.count(), 9);
        assert_eq!(encoder.next_transaction_id(), 8);
        count = encoder.count();
    }

    let frames = wire
        .split(|byte| *byte == 0)
        .filter(|body| !body.is_empty())
        .map(|body| Frame::decode(body).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), count);
    for (index, frame) in frames.iter().enumerate() {
        assert_eq!(frame.transaction_id(), u16::MAX.wrapping_add(index as u16));
    }
    assert!(frames[..3].iter().all(|frame| {
        frame.command_id() == 0x54
            && frame.message_type() == MessageType::NoResponse
            && matches!(Request::decode(frame), Ok(Request::WriteRegisters { .. }))
    }));
    assert!(frames[3..6].iter().all(|frame| {
        frame.command_id() == 0x11
            && matches!(
                Request::decode(frame),
                Ok(Request::AudioEvent {
                    event: ndsif::RequestEvent::Registers { .. },
                    ..
                })
            )
    }));
    assert!(frames[6..].iter().all(|frame| {
        frame.command_id() == 0x10
            && matches!(Request::decode(frame), Ok(Request::AudioData { .. }))
    }));
}

#[test]
fn decoder_handles_split_concatenated_and_expired_frames() {
    let first = Command::Ping(b"probe").encode(42).unwrap();
    let second = Command::Reset.encode(43).unwrap();
    let mut decoder = Decoder::new();
    let mut received = Vec::new();
    decoder.push(&[0xaa, 0xbb], |result| received.push(result));
    decoder.push(&first.as_bytes()[..2], |result| received.push(result));
    decoder.push(&first.as_bytes()[2..], |result| received.push(result));
    let mut joined = first.as_bytes().to_vec();
    joined.extend_from_slice(second.as_bytes());
    decoder.push(&joined, |result| received.push(result));
    assert_eq!(received.len(), 3);
    assert_eq!(
        Request::decode(received[0].as_ref().unwrap()).unwrap(),
        Request::Ping(b"probe")
    );
    assert_eq!(
        received[1].as_ref().unwrap().transaction_id(),
        42,
        "the split frame is not lost"
    );
    assert_eq!(
        Request::decode(&received[2].as_ref().unwrap()),
        Ok(Request::Reset)
    );

    decoder.push(&[0, 3, 4], |_| {});
    assert!(decoder.has_partial_frame());
    assert_eq!(PARTIAL_FRAME_TIMEOUT_MS, 500);
    assert!(decoder.expire_partial_frame());
    assert!(!decoder.has_partial_frame());
    assert!(!decoder.expire_partial_frame());
    decoder.push(&[0, 5, 0], |result| {
        assert_eq!(result, Err(Error::InvalidCobs));
    });
}

#[test]
fn encoder_does_not_consume_transaction_ids_on_invalid_commands() {
    let mut emitted = 0;
    let mut encoder = CommandEncoder::new(77, |_: &[u8]| emitted += 1);
    assert_eq!(
        encoder.push(Command::SetOutputVolume {
            attenuation: 97,
            response: false,
        }),
        Err(Error::InvalidArgument)
    );
    assert_eq!(encoder.count(), 0);
    assert_eq!(encoder.next_transaction_id(), 77);
    assert_eq!(emitted, 0);
}

#[test]
fn positioned_register_event_preserves_target_and_register_order() {
    let writes = [
        RegisterWrite::new(0x20, 0xc7),
        RegisterWrite::new(0x08, 0x78),
    ];
    let command = Command::AudioEvent {
        stream_id: 0xb7,
        instance: 1,
        position: BytePosition::new(15),
        event: AudioEvent::Registers {
            write_id: WriteId::YM2151,
            instance: 0,
            page: 0,
            writes: &writes,
        },
    };
    let frame = command.to_frame(1).unwrap();
    let Request::AudioEvent {
        stream_id,
        instance,
        position,
        event:
            ndsif::RequestEvent::Registers {
                write_id,
                instance: target_instance,
                page,
                writes: decoded,
            },
    } = Request::decode(&frame).unwrap()
    else {
        panic!("expected positioned register write");
    };
    assert_eq!((stream_id, instance, position.get()), (0xb7, 1, 15));
    assert_eq!((write_id, target_instance, page), (WriteId::YM2151, 0, 0));
    assert_eq!(decoded.iter().collect::<Vec<_>>(), writes);
}

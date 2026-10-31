use ndsif::{Error, Frame, MAX_ENCODED_SIZE, MessageType};

#[test]
fn crc_check_value() {
    assert_eq!(ndsif::frame::checksum(b"123456789"), 0x29b1);
}

#[test]
fn frames_round_trip_with_v1_header_and_delimiters() {
    for payload in [
        [0; 256],
        [0xff; 256],
        core::array::from_fn(|index| index as u8),
    ] {
        let frame = Frame::new(MessageType::Response, 0xa5, u16::MAX, &payload).unwrap();
        let encoded = frame.encode();
        let wire = encoded.as_bytes();
        assert!(wire.len() <= MAX_ENCODED_SIZE);
        assert_eq!(wire[0], 0);
        assert_eq!(wire[wire.len() - 1], 0);
        assert!(!wire[1..wire.len() - 1].contains(&0));
        assert_eq!(Frame::decode(&wire[1..wire.len() - 1]).unwrap(), frame);
    }
    assert_eq!(
        Frame::new(MessageType::Request, 0x01, 0, &[0; 257]),
        Err(Error::PayloadTooLong { length: 257 })
    );
    assert!(matches!(
        Frame::new(MessageType::Request, 0, 0, &[])
            .unwrap()
            .encode_into(&mut [0; 1]),
        Err(Error::BufferTooSmall { .. })
    ));
}

fn cobs_body(raw: &[u8]) -> Vec<u8> {
    let mut encoded = vec![0; 300];
    let length = cobs::encode(raw, &mut encoded);
    encoded.truncate(length);
    encoded
}

#[test]
fn rejects_corruption_at_each_frame_validation_layer() {
    let mut raw = [0x4e, 0x44, 1, 0, 0, 0, 0, 0, 0, 0, 0];
    let crc = ndsif::frame::checksum(&raw[..9]);
    raw[9..].copy_from_slice(&crc.to_le_bytes());
    assert!(Frame::decode(&cobs_body(&raw)).is_ok());

    let mut damaged = raw;
    damaged[0] ^= 1;
    rechecksum(&mut damaged);
    assert_eq!(
        Frame::decode(&cobs_body(&damaged)),
        Err(Error::InvalidMagic)
    );

    let mut damaged = raw;
    damaged[2] = 2;
    rechecksum(&mut damaged);
    assert_eq!(
        Frame::decode(&cobs_body(&damaged)),
        Err(Error::UnsupportedVersion(2))
    );

    let mut damaged = raw;
    damaged[3] = 3;
    rechecksum(&mut damaged);
    assert_eq!(
        Frame::decode(&cobs_body(&damaged)),
        Err(Error::InvalidMessageType(3))
    );

    let mut damaged = raw;
    damaged[7] = 1;
    rechecksum(&mut damaged);
    assert_eq!(
        Frame::decode(&cobs_body(&damaged)),
        Err(Error::InvalidLength)
    );

    let mut damaged = raw;
    damaged[9] ^= 1;
    assert!(matches!(
        Frame::decode(&cobs_body(&damaged)),
        Err(Error::CrcMismatch { .. })
    ));
    assert_eq!(
        Frame::decode(&cobs_body(&raw[..10])),
        Err(Error::InvalidLength)
    );
    for bytes in [&[][..], &[0], &[3, 1]] {
        assert_eq!(Frame::decode(bytes), Err(Error::InvalidCobs));
    }
    assert_eq!(Frame::decode(&[1; 270]), Err(Error::FrameTooLong));
}

fn rechecksum(raw: &mut [u8; 11]) {
    let crc = ndsif::frame::checksum(&raw[..9]);
    raw[9..].copy_from_slice(&crc.to_le_bytes());
}

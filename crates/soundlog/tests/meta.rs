use soundlog::meta::Gd3;
use soundlog::{VgmDocument, VgmHeader};

#[test]
fn test_gd3_to_bytes_fields() {
    let gd3 = Gd3 {
        track_name_en: Some("TrackX".to_string()),
        notes: Some("Note".to_string()),
        ..Default::default()
    };

    let bytes = gd3.to_bytes();
    assert_eq!(&bytes[..4], b"Gd3 ");

    let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    assert_eq!(version, 0x00000100);

    let gd3_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;

    let fields: [Option<&str>; 11] = [
        Some("TrackX"),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("Note"),
    ];

    let mut expected: Vec<u8> = Vec::new();
    for f in &fields {
        if let Some(s) = f {
            for code in s.encode_utf16() {
                expected.extend_from_slice(&code.to_le_bytes());
            }
        }
        expected.extend_from_slice(&0u16.to_le_bytes());
    }

    assert_eq!(gd3_len, expected.len());
    assert_eq!(&bytes[12..12 + expected.len()], &expected[..]);
}

#[test]
fn test_vgmdocument_includes_gd3_and_header_offset() {
    let gd3 = Gd3 {
        track_name_en: Some("TrackX".to_string()),
        notes: Some("Note".to_string()),
        ..Default::default()
    };

    let doc = VgmDocument {
        header: VgmHeader::default(),
        gd3: Some(gd3),
        ..Default::default()
    };

    let bytes: Vec<u8> = doc.into();

    let pos = bytes
        .windows(4)
        .position(|w| w == b"Gd3 ")
        .expect("Gd3 chunk not found");

    let hdr_off = u32::from_le_bytes(bytes[0x14..0x18].try_into().unwrap());
    assert_eq!(hdr_off, (pos as u32).wrapping_sub(0x14));
}

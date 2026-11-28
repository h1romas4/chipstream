use soundlog::meta::{Gd3, Gd3ParseLimits};
use soundlog::{VgmDocument, VgmHeader, VgmParseLimits};

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

    let bytes: Vec<u8> = doc.try_into().unwrap();

    let pos = bytes
        .windows(4)
        .position(|w| w == b"Gd3 ")
        .expect("Gd3 chunk not found");

    let hdr_off = u32::from_le_bytes(bytes[0x14..0x18].try_into().unwrap());
    assert_eq!(hdr_off, (pos as u32).wrapping_sub(0x14));
}

#[test]
fn gd3_and_vgm_document_apply_configurable_chunk_limits() {
    let gd3 = Gd3 {
        notes: Some("a".repeat((16 * 1024 - 34) / 2)),
        ..Default::default()
    };
    let exact = gd3.to_bytes();
    assert_eq!(exact.len(), 16 * 1024);
    assert_eq!(Gd3::try_from(exact.as_slice()).unwrap(), gd3);
    let exact_document = Vec::<u8>::try_from(VgmDocument {
        gd3: Some(gd3),
        ..Default::default()
    })
    .unwrap();
    assert!(VgmDocument::parse(&exact_document).is_ok());
    assert!(
        VgmDocument::parse_with_limits(
            &exact_document,
            VgmParseLimits {
                max_gd3_bytes: Some(exact.len() - 1)
            }
        )
        .is_err()
    );
    assert!(
        Gd3::parse_with_limits(
            &exact,
            Gd3ParseLimits {
                max_gd3_bytes: Some(exact.len() - 1)
            }
        )
        .is_err()
    );
    assert!(
        Gd3::parse_with_limits(
            &exact,
            Gd3ParseLimits {
                max_gd3_bytes: Some(0)
            }
        )
        .is_err()
    );

    let oversized = Gd3 {
        notes: Some("a".repeat((16 * 1024 - 34) / 2 + 1)),
        ..Default::default()
    };
    let raw = oversized.to_bytes();
    let error = Gd3::parse(&raw).unwrap_err();
    assert!(error.to_string().contains("16386") && error.to_string().contains("16384"));
    assert_eq!(
        Gd3::parse_with_limits(
            &raw,
            Gd3ParseLimits {
                max_gd3_bytes: None
            }
        )
        .unwrap(),
        oversized
    );
    let document = VgmDocument {
        gd3: Some(oversized.clone()),
        ..Default::default()
    };
    let bytes = Vec::<u8>::try_from(document).unwrap();
    assert!(VgmDocument::try_from(bytes.as_slice()).is_err());
    assert_eq!(
        VgmDocument::parse_with_limits(
            &bytes,
            VgmParseLimits {
                max_gd3_bytes: Some(raw.len())
            }
        )
        .unwrap()
        .gd3,
        Some(oversized.clone())
    );
    assert_eq!(
        VgmDocument::parse_with_limits(
            &bytes,
            VgmParseLimits {
                max_gd3_bytes: None
            }
        )
        .unwrap()
        .gd3,
        Some(oversized)
    );
    let no_gd3 = Vec::<u8>::try_from(VgmDocument::default()).unwrap();
    assert!(
        VgmDocument::parse_with_limits(
            &no_gd3,
            VgmParseLimits {
                max_gd3_bytes: Some(0)
            }
        )
        .is_ok()
    );
}

#[test]
fn rejects_overflowing_gd3_length_and_vgm_offset() {
    let mut gd3 = Gd3::default().to_bytes();
    gd3[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(
        Gd3::parse_with_limits(
            &gd3,
            Gd3ParseLimits {
                max_gd3_bytes: None
            }
        )
        .is_err()
    );
    let mut vgm = Vec::<u8>::try_from(VgmDocument::default()).unwrap();
    vgm[0x14..0x18].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(VgmDocument::parse(&vgm).is_err());
}

use soundlog::chip::*;
use soundlog::{Instance, VgmBuilder, VgmCommand};
use std::convert::TryInto;

/// Ensure we can pass a tag-only `Chip` to `register_chip` (existing test).
#[test]
fn add_chip_accepts_tag_only_chip() {
    let mut b = VgmBuilder::new();
    b.register_chip(Chip::Ym2612, 0, 8000000);
    let doc = b.finalize();
    assert_eq!(doc.header.ym2612_clock, 8000000);
}

/// Exercise chip spec encoding using only the public API:
/// - use `VgmBuilder::add_chip_write(Instance, spec)` to add a single write
/// - finalize and serialize the document
/// - locate the command stream start from the header data_offset
/// - compare the emitted command bytes against expected vgm bytes
#[test]
fn api_spec_encode_vgm_bytes_all() {
    // For specs that are chip-specific (need Instance), use add_chip_write path.
    // Helper for chip specs: build a doc with add_chip_write and return command bytes.
    fn cmd_bytes_for_chip_spec<C>(instance: Instance, spec: C) -> Vec<u8>
    where
        C: Clone,
        (Instance, C): Into<VgmCommand>,
    {
        let mut b = VgmBuilder::new();
        b.add_chip_write(instance, spec.clone());
        let doc = b.finalize();
        let bytes: Vec<u8> = (&doc).into();
        let data_offset = u32::from_le_bytes(bytes[0x34..0x38].try_into().unwrap());
        let header_len = 0x34usize + data_offset as usize;
        let available = bytes.len().saturating_sub(header_len);
        bytes[header_len..header_len + available].to_vec()
    }

    // Test PSG (PsgSpec) via a direct VgmCommand to avoid needing Instance wrapping.
    {
        let cmds = cmd_bytes_for_chip_spec(Instance::Primary, PsgSpec { value: 0xAA });
        assert_eq!(cmds[..2], [0x50, 0xAA]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2413Spec {
                register: 0x10,
                value: 0x22,
            },
        );
        assert_eq!(cmds[..3], [0x51, 0x10, 0x22]);
    }
    {
        let b0 = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2612Spec {
                port: 0,
                register: 0x2A,
                value: 0x55,
            },
        );
        let b1 = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2612Spec {
                port: 1,
                register: 0x2A,
                value: 0x66,
            },
        );
        assert_eq!(b0[..3], [0x52, 0x2A, 0x55]);
        assert_eq!(b1[..3], [0x53, 0x2A, 0x66]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2151Spec {
                register: 0x01,
                value: 0x02,
            },
        );
        assert_eq!(cmds[..3], [0x54, 0x01, 0x02]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            SegaPcmSpec {
                offset: 0x1234,
                value: 0x77,
            },
        );
        assert_eq!(cmds[..4], [0xC0, 0x12, 0x34, 0x77]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Rf5c68U16Spec {
                offset: 0x0201,
                value: 0x88,
            },
        );
        assert_eq!(cmds[..4], [0xC1, 0x02, 0x01, 0x88]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2203Spec {
                register: 0x05,
                value: 0x06,
            },
        );
        assert_eq!(cmds[..3], [0x55, 0x05, 0x06]);
    }
    {
        let b0 = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2608Spec {
                port: 0,
                register: 0x07,
                value: 0x08,
            },
        );
        let b1 = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2608Spec {
                port: 1,
                register: 0x07,
                value: 0x09,
            },
        );
        assert_eq!(b0[..3], [0x56, 0x07, 0x08]);
        assert_eq!(b1[..3], [0x57, 0x07, 0x09]);
    }
    {
        let b0 = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2610Spec {
                port: 0,
                register: 0x0A,
                value: 0x0B,
            },
        );
        let b1 = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym2610Spec {
                port: 1,
                register: 0x0A,
                value: 0x0C,
            },
        );
        assert_eq!(b0[..3], [0x58, 0x0A, 0x0B]);
        assert_eq!(b1[..3], [0x59, 0x0A, 0x0C]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym3812Spec {
                register: 0x11,
                value: 0x22,
            },
        );
        assert_eq!(cmds[..3], [0x5A, 0x11, 0x22]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ym3526Spec {
                register: 0x12,
                value: 0x23,
            },
        );
        assert_eq!(cmds[..3], [0x5B, 0x12, 0x23]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Y8950Spec {
                register: 0x13,
                value: 0x24,
            },
        );
        assert_eq!(cmds[..3], [0x5C, 0x13, 0x24]);
    }
    {
        let b0 = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ymf262Spec {
                port: 0,
                register: 0x20,
                value: 0x21,
            },
        );
        let b1 = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ymf262Spec {
                port: 1,
                register: 0x20,
                value: 0x22,
            },
        );
        assert_eq!(b0[..3], [0x5E, 0x20, 0x21]);
        assert_eq!(b1[..3], [0x5F, 0x20, 0x22]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ymf278bSpec {
                port: 0x01,
                register: 0x02,
                value: 0x03,
            },
        );
        assert_eq!(cmds[..4], [0xD0, 0x01, 0x02, 0x03]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ymf271Spec {
                port: 0x02,
                register: 0x03,
                value: 0x04,
            },
        );
        assert_eq!(cmds[..4], [0xD1, 0x02, 0x03, 0x04]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ymz280bSpec {
                register: 0x30,
                value: 0x31,
            },
        );
        assert_eq!(cmds[..3], [0x5D, 0x30, 0x31]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Rf5c164U8Spec {
                offset: 0x40,
                value: 0x41,
            },
        );
        assert_eq!(cmds[..3], [0xB1, 0x40, 0x41]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            PwmSpec {
                register: 0x01,
                value: 0x00112233,
            },
        );
        assert_eq!(cmds[..3], [0xB2, 0x12, 0x33]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ay8910Spec {
                register: 0x02,
                value: 0x03,
            },
        );
        assert_eq!(cmds[..3], [0xA0, 0x02, 0x03]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            GbDmgSpec {
                register: 0x04,
                value: 0x05,
            },
        );
        assert_eq!(cmds[..3], [0xB3, 0x04, 0x05]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            NesApuSpec {
                register: 0x06,
                value: 0x07,
            },
        );
        assert_eq!(cmds[..3], [0xB4, 0x06, 0x07]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            MultiPcmSpec {
                register: 0x08,
                value: 0x09,
            },
        );
        assert_eq!(cmds[..3], [0xB5, 0x08, 0x09]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Upd7759Spec {
                register: 0x0A,
                value: 0x0B,
            },
        );
        assert_eq!(cmds[..3], [0xB6, 0x0A, 0x0B]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Okim6258Spec {
                register: 0x0C,
                value: 0x0D,
            },
        );
        assert_eq!(cmds[..3], [0xB7, 0x0C, 0x0D]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Okim6295Spec {
                register: 0x0E,
                value: 0x0F,
            },
        );
        assert_eq!(cmds[..3], [0xB8, 0x0E, 0x0F]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            K054539Spec {
                register: 0x1122,
                value: 0x33,
            },
        );
        assert_eq!(cmds[..4], [0xD3, 0x11, 0x22, 0x33]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Huc6280Spec {
                register: 0x12,
                value: 0x13,
            },
        );
        assert_eq!(cmds[..3], [0xB9, 0x12, 0x13]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            C140Spec {
                register: 0x3344,
                value: 0x55,
            },
        );
        assert_eq!(cmds[..4], [0xD4, 0x33, 0x44, 0x55]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            K053260Spec {
                register: 0x14,
                value: 0x15,
            },
        );
        assert_eq!(cmds[..3], [0xBA, 0x14, 0x15]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            PokeySpec {
                register: 0x16,
                value: 0x17,
            },
        );
        assert_eq!(cmds[..3], [0xBB, 0x16, 0x17]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            QsoundSpec {
                register: 0x21,
                value: 0x1234,
            },
        );
        assert_eq!(cmds[..4], [0xC4, 0x21, 0x12, 0x34]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            ScspSpec {
                offset: 0x5566,
                value: 0x77,
            },
        );
        assert_eq!(cmds[..4], [0xC5, 0x55, 0x66, 0x77]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            WonderSwanSpec {
                offset: 0x2233,
                value: 0x44,
            },
        );
        assert_eq!(cmds[..4], [0xC6, 0x22, 0x33, 0x44]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            VsuSpec {
                offset: 0x4455,
                value: 0x66,
            },
        );
        assert_eq!(cmds[..4], [0xC7, 0x44, 0x55, 0x66]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Saa1099Spec {
                register: 0x18,
                value: 0x19,
            },
        );
        assert_eq!(cmds[..3], [0xBD, 0x18, 0x19]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Es5503Spec {
                register: 0x8899,
                value: 0xAA,
            },
        );
        assert_eq!(cmds[..4], [0xD5, 0x88, 0x99, 0xAA]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Es5506U16Spec {
                register: 0x1A,
                value: 0xBEEF,
            },
        );
        assert_eq!(cmds[..4], [0xD6, 0x1A, 0xBE, 0xEF]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            X1010Spec {
                offset: 0x7788,
                value: 0x99,
            },
        );
        assert_eq!(cmds[..4], [0xC8, 0x77, 0x88, 0x99]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            C352Spec {
                register: 0x3344,
                value: 0xCAFE,
            },
        );
        assert_eq!(cmds[..5], [0xE1, 0x33, 0x44, 0xCA, 0xFE]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Ga20Spec {
                register: 0x1B,
                value: 0x1C,
            },
        );
        assert_eq!(cmds[..3], [0xBF, 0x1B, 0x1C]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            MikeySpec {
                register: 0x1D,
                value: 0x1E,
            },
        );
        assert_eq!(cmds[..3], [0x40, 0x1D, 0x1E]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(Instance::Primary, GameGearPsgSpec { value: 0x7F });
        assert_eq!(cmds[..2], [0x4F, 0x7F]);
    }
    {
        let cmds = cmd_bytes_for_chip_spec(
            Instance::Primary,
            Scc1Spec {
                port: 0x05,
                register: 0x06,
                value: 0x07,
            },
        );
        assert_eq!(cmds[..4], [0xD2, 0x05, 0x06, 0x07]);
    }
}

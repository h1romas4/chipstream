#![cfg(feature = "mdx")]

use soundlog::mdx::opl4::{Opl4Voice, pdx_sample_to_vgm, pdx_voices_to_vgm};
use soundlog::mdx::pcm::{Pcm8aFormat, decode_adpcm, decode_pcm8a_with_pcm16_15khz};
use soundlog::mdx::pdx::{PdxBuilder, PdxDocument};
use soundlog::vgm::VgmDocument;
use soundlog::vgm::command::{Instance, VgmCommand};

fn sample_document(bytes: Vec<u8>) -> PdxDocument {
    let mut builder = PdxBuilder::new();
    builder.set_sample(1, 2, bytes).unwrap();
    builder.finalize()
}

fn rom_bytes(document: &VgmDocument) -> &[u8] {
    let VgmCommand::DataBlock(block) = &document.commands[0] else {
        panic!("ROM must precede register writes");
    };
    assert_eq!(block.data_type, 0x84);
    assert_eq!(block.chip_instance, 0);
    assert_eq!(block.size as usize, block.data.len());
    assert_eq!(
        u32::from_le_bytes(block.data[..4].try_into().unwrap()) as usize,
        block.data.len() - 8,
    );
    assert_eq!(&block.data[4..8], &[0; 4]);
    &block.data[8..]
}

#[test]
fn pdx_adpcm_is_stored_as_lossless_pcm12_with_a_silent_loop() {
    let source = [0x07, 0xf7, 0x88, 0x12, 0x34, 0xff];
    let document = pdx_sample_to_vgm(&sample_document(source.to_vec()), 1, 2).unwrap();
    let rom = rom_bytes(&document);
    let decoded = decode_adpcm(&source);
    assert_eq!(rom.len(), 12 + decoded.len() * 3 / 2 + 3);
    assert_eq!(&rom[..3], &[0x40, 0, 12]);
    assert_eq!(u16::from_be_bytes([rom[3], rom[4]]) as usize, decoded.len());
    assert_eq!(
        0u16.wrapping_sub(u16::from_be_bytes([rom[5], rom[6]])) as usize,
        decoded.len() + 2,
    );
    assert_eq!(&rom[7..12], &[0, 0xf0, 0, 0xff, 0]);
    assert_eq!(&rom[rom.len() - 3..], &[0; 3]);
    let unpacked: Vec<i16> = rom[12..rom.len() - 3]
        .chunks_exact(3)
        .flat_map(|pair| {
            [
                i16::from_be_bytes([pair[0], pair[1] << 4]) >> 4,
                i16::from_be_bytes([pair[2], pair[1] & 0xf0]) >> 4,
            ]
        })
        .collect();
    assert_eq!(unpacked, decoded);
}

#[test]
fn opl4_initialization_pitch_key_on_and_stop_survive_vgm_roundtrip() {
    let document = pdx_sample_to_vgm(&sample_document(vec![0x77; 32]), 1, 2).unwrap();
    assert_eq!(document.header.ymf278b_clock, 33_868_800);
    assert_eq!(document.header.okim6258_clock, 0);
    assert_eq!(document.header.ym2151_clock, 0);
    assert_eq!(document.header.loop_offset, 0);
    let writes: Vec<_> = document
        .commands
        .iter()
        .filter_map(|command| match command {
            VgmCommand::Ymf278bWrite(instance, spec) => {
                assert_eq!(*instance, Instance::Primary);
                Some((spec.port, spec.register, spec.value))
            }
            VgmCommand::DataBlock(_) | VgmCommand::WaitSamples(_) | VgmCommand::EndOfData(_) => {
                None
            }
            other => panic!("unexpected command: {other:?}"),
        })
        .collect();
    assert_eq!(
        writes,
        [
            (1, 0x05, 0x03),
            (2, 0x02, 0),
            (2, 0xf8, 0x3f),
            (2, 0xf9, 0),
            (2, 0x20, 0x56),
            (2, 0x08, 0),
            (2, 0x38, 0xf3),
            (2, 0x50, 1),
            (2, 0x68, 0x80),
            (2, 0x50, 0xff),
            (2, 0x68, 0),
        ]
    );
    for (index, command) in document.commands.iter().enumerate() {
        if matches!(command, VgmCommand::Ymf278bWrite(_, _)) {
            assert!(
                matches!(&document.commands[index + 1], VgmCommand::WaitSamples(wait) if wait.0 == 1)
            );
        }
    }
    assert!(
        document
            .commands
            .iter()
            .any(|command| matches!(command, VgmCommand::WaitSamples(wait) if wait.0 == 14))
    );
    assert_eq!(
        document.header.total_samples,
        14 + 11 + (64u32 * 4096).div_ceil(1451) + 256
    );
    assert!(matches!(
        document.commands.last(),
        Some(VgmCommand::EndOfData(_))
    ));
    let bytes: Vec<u8> = (&document).into();
    let reparsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    assert_eq!(reparsed.commands, document.commands);
    assert_eq!(reparsed.header.ymf278b_clock, document.header.ymf278b_clock);
    assert_eq!(reparsed.header.total_samples, document.header.total_samples);
}

#[test]
fn missing_entries_are_errors() {
    let pdx = sample_document(vec![0x12]);
    for (bank, note) in [(0, 0), (1, 1), (1, 96), (32, 2)] {
        let error = pdx_sample_to_vgm(&pdx, bank, note).unwrap_err();
        assert!(error.to_string().contains("missing or invalid PDX sample"));
    }
}

#[test]
fn length_limit_is_checked_and_long_waits_are_split() {
    for length in [1, 32_766] {
        let document = pdx_sample_to_vgm(&sample_document(vec![0x12; length]), 1, 2).unwrap();
        assert_eq!(rom_bytes(&document).len(), 12 + length * 3 + 3);
        assert_eq!(
            document.header.total_samples,
            14 + 11 + (length as u32 * 2 * 4096).div_ceil(1451) + 256,
        );
        if length == 32_766 {
            assert!(document.commands.iter().any(|command| {
                matches!(command, VgmCommand::WaitSamples(wait) if wait.0 == u16::MAX)
            }));
        }
    }
    let error = pdx_sample_to_vgm(&sample_document(vec![0; 32_767]), 1, 2).unwrap_err();
    assert!(error.to_string().contains("maximum is 32766"));
}

#[test]
fn repository_pdx_fixture_produces_parseable_opl4_vgm() {
    let pdx = PdxDocument::parse(include_bytes!("../assets/mdx/mdx_fm_loop.pdx")).unwrap();
    let document = pdx_sample_to_vgm(&pdx, 0, 0).unwrap();
    let bytes: Vec<u8> = (&document).into();
    let parsed = VgmDocument::try_from(bytes.as_slice()).unwrap();
    assert_eq!(parsed.commands, document.commands);
    assert!(parsed.header.total_samples > 15);
}

fn register(document: &VgmDocument, address: u8) -> u8 {
    document
        .commands
        .iter()
        .find_map(|command| match command {
            VgmCommand::Ymf278bWrite(_, spec) if spec.port == 2 && spec.register == address => {
                Some(spec.value)
            }
            _ => None,
        })
        .unwrap()
}

fn unpack_wave(rom: &[u8], wave: usize, count: usize) -> Vec<i16> {
    let header = &rom[wave * 12..wave * 12 + 12];
    let start = (usize::from(header[0] & 0x3f) << 16)
        | (usize::from(header[1]) << 8)
        | usize::from(header[2]);
    rom[start..start + count.div_ceil(2) * 3]
        .chunks_exact(3)
        .flat_map(|pair| {
            [
                i16::from_be_bytes([pair[0], pair[1] << 4]) >> 4,
                i16::from_be_bytes([pair[2], pair[1] & 0xf0]) >> 4,
            ]
        })
        .take(count)
        .collect()
}

#[test]
fn eight_slots_share_one_wave_but_have_independent_controls() {
    let pdx = sample_document(vec![0x12; 256]);
    let voices: Vec<_> = (0..8)
        .map(|slot| Opl4Voice {
            bank: 1,
            note: 2,
            mode: slot,
            total_level: slot * 8,
            pan: slot as i8 - 4,
        })
        .collect();
    // Use ADPCM-only modes here so only rate differs in the wave cache.
    let modes = [0, 1, 2, 3, 4, 7, 10, 4];
    let voices: Vec<_> = voices
        .into_iter()
        .zip(modes)
        .map(|(voice, mode)| Opl4Voice { mode, ..voice })
        .collect();
    let document = pdx_voices_to_vgm(&pdx, &voices).unwrap();
    assert_eq!(rom_bytes(&document).len(), 12 + 256 * 3 + 3);
    for (slot, voice) in voices.iter().enumerate() {
        let slot = slot as u8;
        assert_eq!(register(&document, 0x08 + slot), 0);
        assert_eq!(
            register(&document, 0x50 + slot),
            (voice.total_level << 1) | 1
        );
        assert_eq!(
            register(&document, 0x68 + slot),
            0x80 | (voice.pan as u8 & 0x0f)
        );
    }
    let mut elapsed = 0;
    let mut onsets = Vec::new();
    let mut stops = Vec::new();
    for command in &document.commands {
        match command {
            VgmCommand::WaitSamples(wait) => elapsed += u32::from(wait.0),
            VgmCommand::Ymf278bWrite(_, spec)
                if spec.port == 2 && (0x68..0x70).contains(&spec.register) =>
            {
                if spec.value & 0x80 != 0 {
                    onsets.push((spec.register - 0x68, elapsed));
                } else {
                    stops.push(spec.register - 0x68);
                }
            }
            _ => {}
        }
    }
    assert_eq!(onsets.len(), 8);
    assert_eq!(stops, (0..8).collect::<Vec<u8>>());
    for (slot, &(channel, time)) in onsets.iter().enumerate() {
        assert_eq!(channel, slot as u8);
        assert_eq!(time - onsets[0].1, slot as u32);
    }
    let bytes: Vec<u8> = (&document).into();
    assert_eq!(
        VgmDocument::try_from(bytes.as_slice()).unwrap().commands,
        document.commands
    );
}

#[test]
fn every_pcm8a_mode_preserves_decoded_samples_and_meets_pitch_tolerance() {
    let bytes = [0x07, 0xff, 0xf8, 0x00, 0, 0x10];
    let pdx = sample_document(bytes.to_vec());
    let rates = [
        3906.25,
        15625.0 / 3.0,
        7812.5,
        31250.0 / 3.0,
        15625.0,
        15625.0,
        15625.0,
        62500.0 / 3.0,
        62500.0 / 3.0,
        62500.0 / 3.0,
        31250.0,
        31250.0,
        31250.0,
    ];
    for mode in 0..=12 {
        let voice = Opl4Voice {
            bank: 1,
            note: 2,
            mode,
            ..Opl4Voice::default()
        };
        let document = pdx_voices_to_vgm(&pdx, &[voice]).unwrap();
        let format = match mode {
            5 | 8 | 11 => Pcm8aFormat::Pcm16,
            6 | 9 | 12 => Pcm8aFormat::Pcm8,
            _ => Pcm8aFormat::Adpcm,
        };
        let expected = decode_pcm8a_with_pcm16_15khz(format, &bytes, mode == 5).unwrap();
        assert_eq!(
            unpack_wave(rom_bytes(&document), 0, expected.len()),
            expected,
            "F{mode}"
        );
        let high = register(&document, 0x38);
        let octave = (high as i8) >> 4;
        let fnumber = u16::from(register(&document, 0x20) >> 1) | (u16::from(high & 7) << 7);
        let step = ((1024 + u32::from(fnumber)) << (octave + 7)) >> 2;
        let actual = 44100.0 * f64::from(step) / 65536.0;
        let target = rates[usize::from(mode)];
        assert!(
            (actual / target - 1.0).abs() < 0.0003,
            "F{mode}: {actual} vs {target}"
        );
        assert_eq!(
            document.header.total_samples,
            14 + 11 + (expected.len() as u32 * 65536).div_ceil(step) + 256
        );
    }
}

#[test]
fn wave_cache_distinguishes_formats_and_f5_scaling_but_shares_aliases() {
    let source = [0x07, 0xff, 0xf8, 0x00];
    let mut bytes = sample_document(source.to_vec()).to_bytes();
    let entry = bytes[(96 + 2) * 8..(96 + 3) * 8].to_vec();
    bytes[(96 + 3) * 8..(96 + 4) * 8].copy_from_slice(&entry);
    let pdx = PdxDocument::parse(&bytes).unwrap();
    let modes = [4, 7, 10, 5, 8, 11, 6, 12];
    let voices: Vec<_> = modes
        .into_iter()
        .enumerate()
        .map(|(index, mode)| Opl4Voice {
            bank: 1,
            note: if index % 2 == 0 { 2 } else { 3 },
            mode,
            ..Opl4Voice::default()
        })
        .collect();
    let document = pdx_voices_to_vgm(&pdx, &voices).unwrap();
    for (slot, expected_wave) in [0, 0, 0, 1, 2, 2, 3, 3].into_iter().enumerate() {
        assert_eq!(register(&document, 0x08 + slot as u8), expected_wave);
    }
    let rom = rom_bytes(&document);
    assert_eq!(unpack_wave(rom, 1, 2), [2047, -2048]);
    assert_eq!(unpack_wave(rom, 2, 2), [127, -128]);
    assert_eq!(rom.len(), 48 + (12 + 3) + (3 + 3) * 2 + (6 + 3));
}

#[test]
fn odd_pcm_lengths_are_padded_and_wave_addresses_cross_64k() {
    let pdx = sample_document(vec![0xff]);
    let document = pdx_voices_to_vgm(
        &pdx,
        &[Opl4Voice {
            bank: 1,
            note: 2,
            mode: 6,
            ..Opl4Voice::default()
        }],
    )
    .unwrap();
    let rom = rom_bytes(&document);
    assert_eq!(rom.len(), 12 + 3 + 3);
    assert_eq!(&rom[3..7], &[0, 2, 0xff, 0xfc]);
    assert_eq!(unpack_wave(rom, 0, 2), [-16, 0]);

    let mut builder = PdxBuilder::new();
    builder.set_sample(0, 0, vec![0x12; 32_766]).unwrap();
    builder.set_sample(0, 1, vec![0x34; 32_766]).unwrap();
    let document = pdx_voices_to_vgm(
        &builder.finalize(),
        &[
            Opl4Voice::default(),
            Opl4Voice {
                note: 1,
                ..Opl4Voice::default()
            },
        ],
    )
    .unwrap();
    let rom = rom_bytes(&document);
    let second_start = 24 + 32_766 * 3 + 3;
    assert_eq!(rom[12], 0x40 | (second_start >> 16) as u8);
    assert_eq!(
        unpack_wave(rom, 1, 65_532),
        decode_adpcm(&vec![0x34; 32_766])
    );
}

#[test]
fn polyphonic_invalid_inputs_are_explicit_errors() {
    let pdx = sample_document(vec![0x12]);
    let valid = Opl4Voice {
        bank: 1,
        note: 2,
        ..Opl4Voice::default()
    };
    for voices in [
        vec![],
        vec![valid; 9],
        vec![Opl4Voice { mode: 13, ..valid }],
        vec![Opl4Voice {
            total_level: 128,
            ..valid
        }],
        vec![Opl4Voice { pan: -9, ..valid }],
        vec![Opl4Voice { pan: 8, ..valid }],
        vec![Opl4Voice { mode: 5, ..valid }],
        vec![valid, Opl4Voice { note: 3, ..valid }],
    ] {
        assert!(pdx_voices_to_vgm(&pdx, &voices).is_err(), "{voices:?}");
    }
    for (mode, length) in [(6, 65_533), (11, 65_533 * 2)] {
        let pdx = sample_document(vec![0; length]);
        assert!(pdx_voices_to_vgm(&pdx, &[Opl4Voice { mode, ..valid }]).is_err());
    }
}

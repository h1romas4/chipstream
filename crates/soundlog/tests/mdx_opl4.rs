#![cfg(feature = "mdx")]

use soundlog::mdx::command::*;
use soundlog::mdx::convert::{
    AdpcmMode, MdxConvertError, MdxToVgmOptions, PcmOutput, to_vgm_document,
    to_vgm_stream_generator,
};
use soundlog::mdx::document::MdxBuilder;
use soundlog::mdx::package::MdxPackage;
use soundlog::mdx::pdx::PdxBuilder;
use soundlog::vgm::VgmDocument;
use soundlog::vgm::command::VgmCommand;
use soundlog::vgm::header::{ChipId, ChipVolume};

fn options() -> MdxToVgmOptions {
    MdxToVgmOptions {
        pcm_output: PcmOutput::Opl4,
        loop_count: Some(1),
        ..Default::default()
    }
}

fn package(builder: MdxBuilder, bytes: Vec<u8>) -> MdxPackage {
    let mut pdx = PdxBuilder::new();
    pdx.set_sample(0, 0, bytes).unwrap();
    MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: Some(pdx.finalize()),
    }
}

fn lazy(package: MdxPackage, options: MdxToVgmOptions) -> Vec<VgmCommand> {
    let mut generator = to_vgm_stream_generator(package, options).unwrap();
    let mut commands = Vec::new();
    while let Some(command) = generator.next_command().unwrap() {
        commands.push(command);
    }
    commands
}

fn assert_same_output(eager: Vec<VgmCommand>, lazy: Vec<VgmCommand>) {
    let split = |commands: Vec<VgmCommand>| {
        commands
            .into_iter()
            .partition::<Vec<_>, _>(|command| matches!(command, VgmCommand::DataBlock(_)))
    };
    let (eager_rom, eager_events) = split(eager);
    let (lazy_rom, lazy_events) = split(lazy);
    assert_eq!(eager_rom.len(), lazy_rom.len());
    for (index, (eager, lazy)) in eager_rom.iter().zip(&lazy_rom).enumerate() {
        assert!(eager == lazy, "ROM block {index} differs");
    }
    assert_eq!(eager_events.len(), lazy_events.len());
    for (index, (eager, lazy)) in eager_events.iter().zip(&lazy_events).enumerate() {
        assert_eq!(eager, lazy, "event {index}");
    }
}

fn wave_count(commands: &[VgmCommand]) -> usize {
    commands
        .iter()
        .filter(|command| {
            matches!(command,
                VgmCommand::DataBlock(block) if block.data_type == 0x84
                && u32::from_le_bytes(block.data[4..8].try_into().unwrap()) < 0x2000
            )
        })
        .count()
}

fn transitions(commands: &[VgmCommand], slot: u8) -> Vec<(u32, bool)> {
    let mut time = 0;
    let mut active = false;
    let mut changes = Vec::new();
    for command in commands {
        match command {
            VgmCommand::WaitSamples(wait) => time += u32::from(wait.0),
            VgmCommand::Ymf278bWrite(_, spec) if spec.port == 2 && spec.register == 0x68 + slot => {
                let next = spec.value & 0x80 != 0;
                if next != active {
                    changes.push((time, next));
                    active = next;
                }
            }
            _ => {}
        }
    }
    changes
}

#[test]
fn eight_mdx_tracks_use_independent_slots_and_one_shared_rom_wave() {
    let mut builder = MdxBuilder::new();
    for slot in 0..8 {
        builder
            .add_mdx_command(8 + slot, MdxPan::from_raw((slot % 4) as u8))
            .add_mdx_command(
                8 + slot,
                MdxVolume {
                    value: slot as u8 + 4,
                },
            )
            .add_mdx_command(
                8 + slot,
                MdxAdpcmOrNoiseFrequency {
                    value: [0, 1, 2, 3, 4, 7, 10, 4][slot],
                },
            )
            .add_mdx_command(
                8 + slot,
                MdxNote {
                    note: 0x80,
                    length: 8,
                },
            );
    }
    let package = package(builder, vec![0x12; 2048]);
    let document = to_vgm_document(&package, &options()).unwrap();
    assert_eq!(document.header.ymf278b_clock, 33_868_800);
    assert_eq!(document.header.okim6258_clock, 0);
    assert_eq!(document.header.volume_modifier, 0x50);
    let volume = ChipVolume::new_relative(
        ChipId::Ymf278b,
        soundlog::vgm::command::Instance::Primary,
        soundlog::mdx::opl4::VGM_VOLUME,
    );
    assert_eq!(
        document
            .extra_header
            .as_ref()
            .unwrap()
            .chip_volumes
            .as_slice(),
        std::slice::from_ref(&volume)
    );
    assert_eq!(volume.volume_multiplier(), Some(20.0));
    assert!(
        !document
            .commands
            .iter()
            .any(|command| matches!(command, VgmCommand::Okim6258Write(..)))
    );
    assert_eq!(wave_count(&document.commands), 1);
    for slot in 0..8 {
        let changes = transitions(&document.commands, slot);
        assert_eq!(changes.iter().filter(|(_, active)| *active).count(), 1);
        assert!(!changes.last().unwrap().1);
        assert!(document.commands.iter().any(|command| matches!(command,
            VgmCommand::Ymf278bWrite(_, spec) if spec.register == 0x68 + slot
            && spec.value == 0x80 | [8,9,7,0][usize::from(slot % 4)]
        )));
    }
    let bytes: Vec<u8> = (&document).into();
    assert_eq!(bytes[0x7c], 0x50);
    assert_eq!(
        VgmDocument::try_from(bytes.as_slice())
            .unwrap()
            .header
            .volume_modifier,
        0x50
    );
    assert_eq!(
        VgmDocument::try_from(bytes.as_slice())
            .unwrap()
            .extra_header
            .unwrap()
            .chip_volumes,
        [volume]
    );
    assert_eq!(
        VgmDocument::try_from(bytes.as_slice()).unwrap().commands,
        document.commands
    );
}

#[test]
fn eager_and_lazy_match_timing_writes_and_rom_contents_after_late_sample_loads() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 4,
            },
        )
        .add_mdx_command(8, MdxAdpcmOrNoiseFrequency { value: 6 })
        .add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 4,
            },
        )
        .add_mdx_command(15, MdxRest { ticks: 16 });
    let package = package(builder, vec![0x12; 256]);
    let eager = to_vgm_document(&package, &options()).unwrap().commands;
    let lazy = lazy(package, options());
    assert_eq!(wave_count(&eager), 2);
    assert_same_output(eager, lazy);
}

#[test]
fn gates_stop_the_voice_and_ties_do_not_retrigger_even_after_natural_end() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(8, MdxGate { value: 4 })
        .add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 8,
            },
        )
        .add_mdx_command(8, MdxRest { ticks: 4 });
    let document = to_vgm_document(&package(builder, vec![0x77; 10_000]), &options()).unwrap();
    let changes = transitions(&document.commands, 0);
    assert_eq!(changes.len(), 2);
    assert!(changes[1].0 > 0 && changes[1].0 < document.header.total_samples / 2);

    for length in [2, 10_000] {
        let mut builder = MdxBuilder::new();
        builder
            .add_mdx_command(8, MdxKeyOffDisable)
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 4,
                },
            )
            .add_mdx_command(8, MdxKeyOffDisable)
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 4,
                },
            )
            .add_mdx_command(8, MdxRest { ticks: 4 })
            .add_mdx_command(
                8,
                MdxNote {
                    note: 0x80,
                    length: 4,
                },
            );
        let document = to_vgm_document(&package(builder, vec![0x12; length]), &options()).unwrap();
        assert_eq!(
            transitions(&document.commands, 0)
                .iter()
                .filter(|(_, on)| *on)
                .count(),
            2
        );
        assert_eq!(wave_count(&document.commands), 1);
    }
}

#[test]
fn live_gain_pan_and_rate_changes_preserve_the_held_voice() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(8, MdxKeyOffDisable)
        .add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 4,
            },
        )
        .add_mdx_command(8, MdxVolume { value: 15 })
        .add_mdx_command(8, MdxPan::Right)
        .add_mdx_command(8, MdxAdpcmOrNoiseFrequency { value: 2 })
        .add_mdx_command(8, MdxKeyOffDisable)
        .add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 4,
            },
        );
    let document = to_vgm_document(&package(builder, vec![0x12; 10_000]), &options()).unwrap();
    assert_eq!(
        transitions(&document.commands, 0)
            .iter()
            .filter(|(_, on)| *on)
            .count(),
        1
    );
    assert_eq!(wave_count(&document.commands), 1);
    let mut time = 0;
    let mut later_pan = false;
    let mut later_pitch = false;
    let mut later_gain = false;
    for command in &document.commands {
        match command {
            VgmCommand::WaitSamples(wait) => time += u32::from(wait.0),
            VgmCommand::Ymf278bWrite(_, spec) if time > 0 => {
                later_pan |= spec.register == 0x68 && spec.value == 0x89;
                later_pitch |= spec.register == 0x38 && spec.value >> 4 == 0x0e;
                later_gain |= spec.register == 0x50 && spec.value == 1;
            }
            _ => {}
        }
    }
    assert!(later_pan && later_pitch && later_gain);
}

#[test]
fn opl4_uses_pcm8a_decoders_and_distinguishes_f5_scaling() {
    let mut builder = MdxBuilder::new();
    for (slot, mode) in [4, 5, 6, 8, 11, 12].into_iter().enumerate() {
        builder
            .add_mdx_command(8 + slot, MdxAdpcmOrNoiseFrequency { value: mode })
            .add_mdx_command(
                8 + slot,
                MdxNote {
                    note: 0x80,
                    length: 4,
                },
            );
    }
    builder.add_mdx_command(15, MdxRest { ticks: 8 });
    let document =
        to_vgm_document(&package(builder, vec![0x07, 0xff, 0xf8, 0]), &options()).unwrap();
    assert_eq!(wave_count(&document.commands), 4);
}

#[test]
fn no_pdx_is_fm_only_and_invalid_options_or_resource_limits_are_errors() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(8, MdxPan::Right)
        .add_mdx_command(8, MdxAdpcmOrNoiseFrequency { value: 0 })
        .add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 2,
            },
        );
    let no_pdx = MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: None,
    };
    let document = to_vgm_document(&no_pdx, &options()).unwrap();
    assert_eq!(document.header.ymf278b_clock, 0);
    assert_eq!(document.header.volume_modifier, 0);
    assert!(document.extra_header.is_none());
    assert!(
        !document
            .commands
            .iter()
            .any(|command| matches!(command, VgmCommand::Ymf278bWrite(..)))
    );
    assert!(
        !document
            .commands
            .iter()
            .any(|command| matches!(command, VgmCommand::Okim6258Write(..)))
    );
    for adpcm_mode in [AdpcmMode::Resample, AdpcmMode::Lpf] {
        let invalid = MdxToVgmOptions {
            adpcm_mode,
            ..options()
        };
        assert!(to_vgm_document(&no_pdx, &invalid).is_err());
        let mut generator = to_vgm_stream_generator(no_pdx.clone(), invalid).unwrap();
        assert!(generator.next_command().is_err());
    }

    let mut builder = MdxBuilder::new();
    builder.add_mdx_command(
        8,
        MdxNote {
            note: 0x80,
            length: 4,
        },
    );
    let large = package(builder, vec![0x12; 32767]);
    assert!(
        to_vgm_document(&large, &options())
            .unwrap_err()
            .to_string()
            .contains("segmentation")
    );
    let limited = MdxToVgmOptions {
        max_decoded_pcm_samples: Some(4),
        ..options()
    };
    assert!(matches!(
        to_vgm_document(&large, &limited),
        Err(MdxConvertError::DecodedPcmSampleLimitExceeded { .. })
    ));
    let limited = MdxToVgmOptions {
        max_output_commands: Some(1),
        ..options()
    };
    assert!(matches!(
        to_vgm_document(&no_pdx, &limited),
        Err(MdxConvertError::OutputCommandLimitExceeded { .. })
    ));
}

#[test]
fn fadeout_reaches_a_muted_opl4_voice_in_both_paths() {
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(
            8,
            MdxCommand::Extended(MdxExtendedCommand::Fadeout { value: 2 }),
        )
        .add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 1000,
            },
        );
    let package = package(builder, vec![0x12; 32_000]);
    let document = to_vgm_document(&package, &options()).unwrap();
    assert!(document.commands.iter().any(|command| matches!(command,
        VgmCommand::Ymf278bWrite(_, spec) if spec.register == 0x68 && spec.value == 0x88
    )));
    assert_same_output(document.commands, lazy(package, options()));
}

#[test]
fn upper_wave_index_survives_live_rate_changes_and_wave_table_limit_is_enforced() {
    fn build(count: usize) -> MdxPackage {
        let mut builder = MdxBuilder::new();
        let mut pdx = PdxBuilder::new();
        for index in 0..count {
            let bank = index / 96;
            let note = index % 96;
            pdx.set_sample(bank, note, vec![0x12]).unwrap();
            builder
                .add_mdx_command(8, MdxVoiceOrPcmBank { value: bank as u8 })
                .add_mdx_command(
                    8,
                    MdxNote {
                        note: 0x80 + note as u8,
                        length: 1,
                    },
                );
        }
        builder
            .add_mdx_command(8, MdxAdpcmOrNoiseFrequency { value: 2 })
            .add_mdx_command(8, MdxRest { ticks: 2 });
        MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: Some(pdx.finalize()),
        }
    }
    let document = to_vgm_document(&build(257), &options()).unwrap();
    assert_eq!(wave_count(&document.commands), 257);
    let rate_write = document
        .commands
        .iter()
        .rev()
        .find_map(|command| match command {
            VgmCommand::Ymf278bWrite(_, spec) if spec.register == 0x20 => Some(spec.value),
            _ => None,
        })
        .unwrap();
    assert_eq!(rate_write & 1, 1);
    assert!(
        to_vgm_document(&build(513), &options())
            .unwrap_err()
            .to_string()
            .contains("512")
    );
}

#[test]
fn decoded_rom_address_limit_returns_an_explicit_error() {
    let mut builder = MdxBuilder::new();
    let mut pdx = PdxBuilder::new();
    for note in 0..44 {
        pdx.set_sample(0, note, vec![0x12; 32_766]).unwrap();
        builder.add_mdx_command(
            8,
            MdxNote {
                note: 0x80 + note as u8,
                length: 1,
            },
        );
    }

    let package = MdxPackage {
        mdx: builder.finalize().unwrap(),
        pdx: Some(pdx.finalize()),
    };
    assert!(
        to_vgm_document(&package, &options())
            .unwrap_err()
            .to_string()
            .contains("4 MiB")
    );
}

#[test]
fn repeated_notes_reuse_pcm_and_produce_a_measurably_smaller_vgm() {
    let mut builder = MdxBuilder::new();
    for _ in 0..32 {
        builder.add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 8,
            },
        );
    }
    builder.add_mdx_command(15, MdxRest { ticks: 256 });
    let package = package(builder, vec![0x12; 256]);
    let opl4 = to_vgm_document(&package, &options()).unwrap();
    let oki = to_vgm_document(
        &package,
        &MdxToVgmOptions {
            pcm_output: PcmOutput::Okim6258,
            ..options()
        },
    )
    .unwrap();
    assert!(oki.extra_header.is_none());
    assert_eq!(oki.header.volume_modifier, 0);
    assert_eq!(wave_count(&opl4.commands), 1);
    assert_eq!(
        transitions(&opl4.commands, 0)
            .iter()
            .filter(|(_, on)| *on)
            .count(),
        32
    );
    let opl4_bytes: Vec<u8> = (&opl4).into();
    let oki_bytes: Vec<u8> = (&oki).into();
    assert!(
        opl4_bytes.len() * 10 < oki_bytes.len(),
        "OPL4 {} bytes vs OKIM6258 {} bytes",
        opl4_bytes.len(),
        oki_bytes.len()
    );
}
#[test]
fn real_mdx_pcm_fixture_has_native_loop_and_finite_eager_lazy_agreement() {
    let fixture = MdxPackage::parse(
        include_bytes!("../assets/mdx/mdx_pcm_reference.mdx"),
        Some(include_bytes!("../assets/mdx/mdx_fm_loop.pdx")),
    )
    .unwrap();
    let mut builder = MdxBuilder::new();
    builder
        .add_mdx_command(
            8,
            MdxNote {
                note: 0x80,
                length: 4,
            },
        )
        .add_mdx_command(
            8,
            MdxCommand::Jump(MdxRelativeOffset {
                opcode: 0xf1,
                offset: -5,
            }),
        );
    let looping = package(builder, vec![0x12; 4096]);
    let native = to_vgm_document(
        &looping,
        &MdxToVgmOptions {
            loop_count: None,
            ..options()
        },
    )
    .unwrap();
    assert!(native.header.loop_offset > 0);
    assert!(native.header.loop_samples > 0);
    assert_eq!(wave_count(&native.commands), 1);
    let finite_loop = to_vgm_document(
        &looping,
        &MdxToVgmOptions {
            loop_count: Some(2),
            ..options()
        },
    )
    .unwrap();
    assert_eq!(wave_count(&finite_loop.commands), 1);
    assert_eq!(
        transitions(&finite_loop.commands, 0)
            .iter()
            .filter(|(_, on)| *on)
            .count(),
        2
    );
    assert_same_output(
        finite_loop.commands,
        lazy(
            looping,
            MdxToVgmOptions {
                loop_count: Some(2),
                ..options()
            },
        ),
    );
    let finite = to_vgm_document(
        &fixture,
        &MdxToVgmOptions {
            loop_count: Some(2),
            ..options()
        },
    )
    .unwrap();
    let streamed = lazy(
        fixture,
        MdxToVgmOptions {
            loop_count: Some(2),
            ..options()
        },
    );
    assert_same_output(finite.commands, streamed);
}

#[test]
fn per_track_f1_loops_keep_shared_rom_and_synchronize_both_paths() {
    let mut builder = MdxBuilder::new();
    for (track, length) in [(8, 8), (9, 4)] {
        builder
            .add_mdx_command(track, MdxNote { note: 0x80, length })
            .add_mdx_command(
                track,
                MdxCommand::EndOfTrackLoop(MdxRelativeOffset {
                    opcode: 0xf1,
                    offset: -5,
                }),
            );
    }
    builder.add_mdx_command(15, MdxRest { ticks: 1 });
    let package = package(builder, vec![0x12; 2048]);
    let finite_options = MdxToVgmOptions {
        loop_count: Some(2),
        ..options()
    };
    let finite = to_vgm_document(&package, &finite_options).unwrap();
    // The shorter F1 track repeats until the longer track reaches the shared boundary.
    for slot in 0..2 {
        assert_eq!(
            transitions(&finite.commands, slot)
                .iter()
                .filter(|(_, on)| *on)
                .count(),
            if slot == 0 { 2 } else { 4 }
        );
    }
    assert_eq!(wave_count(&finite.commands), 1);
    assert_same_output(finite.commands, lazy(package.clone(), finite_options));
    let native = to_vgm_document(
        &package,
        &MdxToVgmOptions {
            loop_count: None,
            ..options()
        },
    )
    .unwrap();
    assert!(native.header.loop_offset > 0 && native.header.loop_samples > 0);
    assert_eq!(wave_count(&native.commands), 1);
}

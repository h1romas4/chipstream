use std::sync::{Arc, Mutex};

use chipstream::chip::{ChipError, ChipRegisterWrite, ScheduledChip, SoundChip, SoundChipState};
use chipstream::sequencer::{
    Ay8910Variant, C140Variant, Chip, ChipBackendSettings, ChipFactoryRegistry, GeneratorConfig,
    Instance, MdxSequencer, NativeOutput, Okim6258Divider, VgmSequencer, VgmSequencerError,
};
use soundlog::chip::{GameGearPsgSpec, Ym2151Spec, Ym2612Spec};
use soundlog::vgm::command::{VgmCommand, WaitSamples};
use soundlog::vgm::header::{Ay8910ChipType, ChipClock, ChipId};
use soundlog::vgm::{VgmBuilder, VgmDocument, VgmExtraHeader};

const MDX: &[u8] = include_bytes!("../assets/mdx/mdx_fm_loop.mdx");
const PDX: &[u8] = include_bytes!("../assets/mdx/mdx_fm_loop.pdx");

#[derive(Debug)]
struct Commands(std::vec::IntoIter<VgmCommand>);

impl soundlog::VgmCommandGenerator for Commands {
    fn next_command(&mut self) -> Result<Option<VgmCommand>, soundlog::ParseError> {
        Ok(self.0.next())
    }
}

fn generator(commands: Vec<VgmCommand>) -> Box<dyn soundlog::VgmCommandGenerator> {
    Box::new(Commands(commands.into_iter()))
}

#[test]
fn ay_output_layout_matches_each_custom_backend() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ay8910, Instance::Primary, 2_000_000);
    builder.register_chip(Chip::Ay8910, Instance::Secondary, 2_000_000);
    builder.add_vgm_command(WaitSamples(10));
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Ay8910, |config| {
        let mut chip = FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::new(Mutex::new(Vec::new())),
        );
        if config.instance() == Instance::Primary {
            chip.channels = 3;
        }
        Ok(Box::new(chip))
    });
    let mut player = VgmSequencer::new_with_factories(
        builder.finalize(),
        44_100,
        64,
        Default::default(),
        factories,
    )
    .unwrap();
    assert_eq!(
        player
            .output_layout(Chip::Ay8910, Instance::Primary)
            .unwrap(),
        &[
            NativeOutput::SsgChannel(0),
            NativeOutput::SsgChannel(1),
            NativeOutput::SsgChannel(2)
        ]
    );
    assert_eq!(
        player
            .output_layout(Chip::Ay8910, Instance::Secondary)
            .unwrap(),
        &[NativeOutput::Mono]
    );
    player
        .set_output_gains(Chip::Ay8910, Instance::Primary, &[1.0, 0.5, 0.0])
        .unwrap();
    player
        .set_output_gains(Chip::Ay8910, Instance::Secondary, &[0.5])
        .unwrap();
}

type DacNotifications = (Instance, Arc<Mutex<Vec<(Instance, bool)>>>);
type ScheduleTrace = (Instance, Arc<Mutex<Vec<(&'static str, Instance, usize)>>>);

struct FakeChip {
    chip: Chip,
    clock: u32,
    channels: usize,
    writes: Arc<Mutex<Vec<Chip>>>,
    dac_notifications: Option<DacNotifications>,
    scheduled_checkpoint: Option<bool>,
    restore_fails: bool,
    stop_dac: bool,
    schedule_trace: Option<ScheduleTrace>,
}

impl FakeChip {
    fn new(chip: Chip, clock: u32, writes: Arc<Mutex<Vec<Chip>>>) -> Self {
        let channels = match &chip {
            Chip::Ym2203 | Chip::Ymf262 => 4,
            Chip::Ym2608 | Chip::Ym2610b => 3,
            Chip::Ymf278b => 6,
            Chip::Ym2413
            | Chip::Ym2151
            | Chip::Ym2612
            | Chip::Pwm
            | Chip::Sn76489
            | Chip::C140
            | Chip::SegaPcm => 2,
            _ => 1,
        };
        Self {
            chip,
            clock,
            channels,
            writes,
            dac_notifications: None,
            scheduled_checkpoint: None,
            restore_fails: false,
            stop_dac: false,
            schedule_trace: None,
        }
    }
}

impl SoundChip for FakeChip {
    fn chip_type(&self) -> &Chip {
        &self.chip
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        if self.chip == Chip::Okim6258 {
            7_812
        } else {
            44_100
        }
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        if write.chip() != self.chip {
            return Err(ChipError::ChipMismatch {
                expected: write.chip(),
                actual: self.chip.clone(),
            });
        }
        self.writes.lock().unwrap().push(self.chip.clone());
        Ok(())
    }

    fn scheduled(&mut self) -> Option<&mut dyn ScheduledChip> {
        if self.dac_notifications.is_some()
            || self.scheduled_checkpoint.is_some()
            || self.schedule_trace.is_some()
        {
            Some(self)
        } else {
            None
        }
    }

    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        samples.fill(0);
        Ok(())
    }

    fn reset(&mut self) {}

    fn save_state(&mut self) -> Result<Box<dyn SoundChipState>, ChipError> {
        Ok(Box::new(0_u8))
    }

    fn restore_state(&mut self, state: &dyn SoundChipState) -> Result<(), ChipError> {
        if !self.restore_fails && state.as_any().is::<u8>() {
            Ok(())
        } else {
            Err(ChipError::InvalidStateSnapshot(self.chip.clone()))
        }
    }
}

impl ScheduledChip for FakeChip {
    fn prepare_scheduled(&mut self) -> Result<(), ChipError> {
        if let Some((instance, trace)) = &self.schedule_trace {
            trace.lock().unwrap().push(("prepare", *instance, 0));
        }
        Ok(())
    }

    fn flush_scheduled_until(&mut self, frame: usize, _rate: u32) -> Result<(), ChipError> {
        if let Some((instance, trace)) = &self.schedule_trace {
            trace.lock().unwrap().push(("flush", *instance, frame));
        }
        Ok(())
    }

    fn supports_checkpoint(&self) -> bool {
        self.scheduled_checkpoint.unwrap_or(false)
    }

    fn stops_dac_stream_on_write(&self, write: &ChipRegisterWrite) -> bool {
        self.stop_dac
            && matches!(write, ChipRegisterWrite::Okim6258(spec) if spec.register == 0 && spec.value & 1 != 0)
    }

    fn set_dac_stream_playing_at(
        &mut self,
        playing: bool,
        _sample_frame: usize,
        _output_rate: u32,
    ) -> Result<(), ChipError> {
        if let Some((instance, notifications)) = &self.dac_notifications {
            notifications.lock().unwrap().push((*instance, playing));
        }
        Ok(())
    }
}

fn ymfm_chips() -> [Chip; 11] {
    [
        Chip::Ym2413,
        Chip::Ym2612,
        Chip::Ym2151,
        Chip::Ym2203,
        Chip::Ym2608,
        Chip::Ym2610b,
        Chip::Ym3812,
        Chip::Ym3526,
        Chip::Y8950,
        Chip::Ymf262,
        Chip::Ymf278b,
    ]
}

#[test]
fn generator_uses_configuration_factories_and_callbacks() {
    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::Sn76489, instance, 3_579_545);
        builder.add_chip_write(instance, GameGearPsgSpec { value: 0x10 });
    }
    builder.add_vgm_command(WaitSamples(100));
    builder.set_extra_header(VgmExtraHeader {
        chip_clocks: vec![ChipClock::new(
            ChipId::Sn76489,
            Instance::Secondary,
            4_000_000,
        )],
        ..Default::default()
    });
    let mut document = builder.finalize();
    document.header.sn76489_flags.gamegear_stereo = true;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    let created = Arc::clone(&seen);
    let fake_writes = Arc::clone(&writes);
    factories.register(Chip::Sn76489, move |config| {
        let ChipBackendSettings::Sn76489(settings) = config.settings() else {
            panic!("missing PSG settings");
        };
        created
            .lock()
            .unwrap()
            .push((config.instance(), config.clock(), settings.stereo_used));
        Ok(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::clone(&fake_writes),
        )))
    });
    let config = GeneratorConfig {
        header: document.header,
        extra_header: document.extra_header,
        sn76489_stereo_used: [false, true],
        ..Default::default()
    };
    let mut player = VgmSequencer::from_generator_with_factories(
        generator(document.commands),
        config,
        44_100,
        64,
        factories,
    )
    .unwrap();
    assert!(matches!(
        player.checkpoint(),
        Err(VgmSequencerError::CheckpointUnsupported)
    ));
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    player.on_write::<GameGearPsgSpec, _>(move |instance, _, _, _| {
        observed.lock().unwrap().push(instance);
    });
    while player.render().unwrap().is_some() {}
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (Instance::Primary, 3_579_545, false),
            (Instance::Secondary, 4_000_000, true)
        ]
    );
    assert_eq!(events.lock().unwrap().len(), 2);
    assert_eq!(writes.lock().unwrap().len(), 2);
}

#[test]
fn generator_with_builtin_chip_matches_document() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    for (register, value) in [(0x28, 0x4c), (0x30, 0), (0x08, 0x78)] {
        builder.add_chip_write(Instance::Primary, Ym2151Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(1000));
    let document = builder.finalize();
    let config = GeneratorConfig {
        header: document.header.clone(),
        ..Default::default()
    };
    let mut generated =
        VgmSequencer::from_generator(generator(document.commands.clone()), config, 48_000, 128)
            .unwrap();
    let mut parsed = VgmSequencer::new(document, 48_000, 128, Default::default()).unwrap();
    let collect = |player: &mut VgmSequencer<'_>| {
        let mut frames = Vec::new();
        while let Some(block) = player.render().unwrap() {
            frames.extend_from_slice(block.frames());
        }
        frames
    };
    let expected = collect(&mut parsed);
    assert!(!expected.is_empty());
    assert_eq!(collect(&mut generated), expected);
}

#[test]
fn generator_reports_missing_end_and_generation_errors() {
    let mut player = VgmSequencer::from_generator(
        generator(vec![VgmCommand::WaitSamples(WaitSamples(10))]),
        GeneratorConfig::default(),
        44_100,
        64,
    )
    .unwrap();
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::UnexpectedEnd)
    ));
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::StreamFailed)
    ));

    #[derive(Debug)]
    struct Fails;
    impl soundlog::VgmCommandGenerator for Fails {
        fn next_command(&mut self) -> Result<Option<VgmCommand>, soundlog::ParseError> {
            Err(soundlog::ParseError::UnexpectedEof)
        }
    }
    let mut player =
        VgmSequencer::from_generator(Box::new(Fails), GeneratorConfig::default(), 44_100, 64)
            .unwrap();
    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::Parse(
            soundlog::ParseError::UnexpectedEof
        ))
    ));
}

#[test]
fn generator_rejects_invalid_clock_ranges() {
    for (chip, instance, range) in [
        (Chip::Ym2151, Instance::Primary, 1..=4),
        (Chip::Okim6258, Instance::Secondary, 1..=4),
        (Chip::Okim6258, Instance::Primary, 0..=4),
        (Chip::Okim6258, Instance::Primary, 4..=1),
    ] {
        let config = GeneratorConfig {
            header: soundlog::vgm::VgmHeader {
                okim6258_clock: 4_000_000,
                ..Default::default()
            },
            clock_ranges: vec![(chip, instance, range)],
            ..Default::default()
        };
        assert!(matches!(
            VgmSequencer::from_generator(generator(vec![]), config, 44_100, 64),
            Err(VgmSequencerError::InvalidGeneratorClockRange { .. })
        ));
    }
}

#[test]
#[cfg(feature = "okim6258")]
fn generator_declared_clock_range_matches_document_playback() {
    use soundlog::chip::Okim6258Spec;
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    for (register, value) in [(0, 2), (1, 0x71)] {
        builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(1000));
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 0x0a,
            value: 0x60,
        },
    );
    builder.add_vgm_command(WaitSamples(2000));
    let document = builder.finalize();
    let config = GeneratorConfig {
        header: document.header.clone(),
        clock_ranges: vec![(Chip::Okim6258, Instance::Primary, 0x601200..=8_000_000)],
        ..Default::default()
    };
    let mut generated =
        VgmSequencer::from_generator(generator(document.commands.clone()), config, 48_000, 257)
            .unwrap();
    let mut parsed = VgmSequencer::new(document, 48_000, 257, Default::default()).unwrap();
    let collect = |player: &mut VgmSequencer<'_>| {
        let mut frames = Vec::new();
        while let Some(block) = player.render().unwrap() {
            frames.extend_from_slice(block.frames());
        }
        frames
    };
    let expected = collect(&mut parsed);
    assert!(!expected.is_empty());
    assert_eq!(collect(&mut generated), expected);
}

#[test]
#[cfg(feature = "okim6258")]
fn generator_fails_when_clock_exceeds_resampler_range() {
    use chipstream::audio::AudioError;
    use soundlog::chip::Okim6258Spec;
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 0x0b,
            value: 1,
        },
    );
    builder.add_vgm_command(WaitSamples(1000));
    let document = builder.finalize();
    let config = GeneratorConfig {
        header: document.header,
        ..Default::default()
    };
    let mut player =
        VgmSequencer::from_generator(generator(document.commands), config, 48_000, 256).unwrap();
    let error = loop {
        match player.render() {
            Err(error) => break error,
            result => assert!(result.unwrap().is_some(), "expected rate error"),
        }
    };
    assert!(matches!(
        error,
        VgmSequencerError::Audio(AudioError::RateOutOfRange { .. })
    ));
}

#[test]
fn factory_fallback_only_handles_declined_instances_and_propagates_errors() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ay8910, Instance::Primary, 2_000_000);
    builder.register_chip(Chip::Ay8910, Instance::Secondary, 2_000_000);
    builder.add_vgm_command(WaitSamples(10));
    let document = builder.finalize();
    let called = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    factories.register_optional(Chip::Ay8910, |config| {
        if config.instance() == Instance::Primary {
            Ok(Some(Box::new(FakeChip::new(
                config.chip().clone(),
                config.clock(),
                Arc::new(Mutex::new(Vec::new())),
            ))))
        } else {
            Ok(None)
        }
    });
    let seen = Arc::clone(&called);
    factories.register_fallback(Chip::Ay8910, move |config| {
        seen.lock().unwrap().push(config.instance());
        Ok(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::new(Mutex::new(Vec::new())),
        )))
    });
    let player = VgmSequencer::new_with_factories(
        document.clone(),
        44_100,
        64,
        Default::default(),
        factories,
    )
    .unwrap();
    assert_eq!(player.chip_instances().count(), 2);
    assert_eq!(*called.lock().unwrap(), vec![Instance::Secondary]);

    let mut factories = ChipFactoryRegistry::new();
    factories.register_optional(Chip::Ay8910, |_| {
        Err(ChipError::External {
            chip: Chip::Ay8910,
            message: "route failed".into(),
        })
    });
    factories.register_fallback(Chip::Ay8910, |_| panic!("errors must not fall back"));
    assert!(matches!(
        VgmSequencer::new_with_factories(document, 44_100, 64, Default::default(), factories),
        Err(VgmSequencerError::Chip(ChipError::External { .. }))
    ));
}

#[test]
fn state_callbacks_work_with_external_chip_factories() {
    use chipstream::sequencer::StateEvent;
    use soundlog::chip::state::Ym2151State;

    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    let fake_writes = Arc::clone(&writes);
    factories.register(Chip::Ym2151, move |config| {
        Ok(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::clone(&fake_writes),
        )))
    });
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    for (register, value) in [(0x28, 0x4c), (0x30, 0), (0x08, 0x78)] {
        builder.add_chip_write(Instance::Primary, Ym2151Spec { register, value });
    }
    builder.add_vgm_command(WaitSamples(100));
    let mut player = VgmSequencer::new_with_factories(
        builder.finalize(),
        44_100,
        64,
        Default::default(),
        factories,
    )
    .unwrap();
    player.track_state_with_voice_clocks::<Ym2151State>();
    let events = Arc::new(Mutex::new(Vec::new()));
    let output = Arc::clone(&events);
    player.on_state_change::<Ym2151Spec, _>(move |instance, frame, changes| {
        output.lock().unwrap().push((instance, frame, changes));
    });
    while player.render().unwrap().is_some() {}
    assert_eq!(writes.lock().unwrap().len(), 3);
    assert!(matches!(
        events.lock().unwrap()[0].2.as_slice(),
        [StateEvent::KeyOn { channel: 0, .. }]
    ));
}

#[test]
fn factory_receives_per_instance_clocks_and_stereo_for_both_vgm_inputs() {
    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::Sn76489, instance, 3_579_545);
    }
    builder.add_chip_write(Instance::Primary, GameGearPsgSpec { value: 0xff });
    builder.add_chip_write(Instance::Secondary, GameGearPsgSpec { value: 0x10 });
    builder.add_vgm_command(WaitSamples(10));
    builder.set_extra_header(VgmExtraHeader {
        chip_clocks: vec![ChipClock::new(
            ChipId::Sn76489,
            Instance::Secondary,
            4_000_000,
        )],
        ..Default::default()
    });
    let mut document = builder.finalize();
    document.header.sn76489_flags.gamegear_stereo = true;
    let bytes: Vec<u8> = document.into();
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();

    for from_bytes in [false, true] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut factories = ChipFactoryRegistry::new();
        let created = Arc::clone(&seen);
        factories.register(Chip::Sn76489, move |config| {
            let ChipBackendSettings::Sn76489(settings) = config.settings() else {
                return Err(ChipError::UnsupportedChip(config.chip().clone()));
            };
            created
                .lock()
                .unwrap()
                .push((config.instance(), config.clock(), settings.stereo_used));
            Ok(Box::new(FakeChip::new(
                config.chip().clone(),
                config.clock(),
                Arc::new(Mutex::new(Vec::new())),
            )))
        });
        if from_bytes {
            VgmSequencer::from_vgm_with_factories(
                bytes.clone(),
                44_100,
                64,
                Default::default(),
                factories,
            )
            .unwrap();
        } else {
            VgmSequencer::new_with_factories(
                document.clone(),
                44_100,
                64,
                Default::default(),
                factories,
            )
            .unwrap();
        }
        assert_eq!(
            *seen.lock().unwrap(),
            [
                (Instance::Primary, 3_579_545, false),
                (Instance::Secondary, 4_000_000, true),
            ]
        );
    }
}

#[test]
fn legacy_vgm_clock_aliases_reach_the_correct_factories() {
    for (clock, alias, opcode) in [
        (4_000_000_u32, Chip::Ym2151, 0x54),
        (7_670_454_u32, Chip::Ym2612, 0x52),
    ] {
        let mut bytes = vec![0_u8; 0x40];
        bytes[..4].copy_from_slice(b"Vgm ");
        bytes[8..12].copy_from_slice(&0x0000_0101_u32.to_le_bytes());
        bytes[0x10..0x14].copy_from_slice(&clock.to_le_bytes());
        bytes.extend_from_slice(&[opcode, 0x28, 0x4c, 0x61, 100, 0, 0x66]);
        let eof = (bytes.len() as u32 - 4).to_le_bytes();
        bytes[4..8].copy_from_slice(&eof);
        let document = VgmDocument::try_from(bytes.as_slice()).unwrap();

        for from_bytes in [false, true] {
            let created = Arc::new(Mutex::new(Vec::new()));
            let writes = Arc::new(Mutex::new(Vec::new()));
            let mut factories = ChipFactoryRegistry::new();
            for chip in [Chip::Ym2413, alias.clone()] {
                let created = Arc::clone(&created);
                let writes = Arc::clone(&writes);
                factories.register(chip, move |config| {
                    created.lock().unwrap().push((
                        config.chip().clone(),
                        config.instance(),
                        config.clock(),
                    ));
                    Ok(Box::new(FakeChip::new(
                        config.chip().clone(),
                        config.clock(),
                        Arc::clone(&writes),
                    )))
                });
            }
            let mut player = if from_bytes {
                VgmSequencer::from_vgm_with_factories(
                    bytes.clone(),
                    44_100,
                    64,
                    Default::default(),
                    factories,
                )
                .unwrap()
            } else {
                VgmSequencer::new_with_factories(
                    document.clone(),
                    44_100,
                    64,
                    Default::default(),
                    factories,
                )
                .unwrap()
            };
            assert_eq!(
                *created.lock().unwrap(),
                [
                    (Chip::Ym2413, Instance::Primary, clock),
                    (alias.clone(), Instance::Primary, clock)
                ]
            );
            while player.render().unwrap().is_some() {}
            assert_eq!(*writes.lock().unwrap(), [alias.clone()]);
        }
    }
}

#[test]
fn psg_factory_scan_reports_malformed_commands_before_creating_a_backend() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
    let mut document = builder.finalize();
    document.header.sn76489_flags.gamegear_stereo = true;
    let mut bytes: Vec<u8> = document.into();
    let header = soundlog::vgm::VgmHeader::from_bytes(&bytes).unwrap();
    let start = soundlog::vgm::VgmHeader::command_start(header.version, header.data_offset);
    bytes.truncate(start);
    bytes.push(0x61);

    let called = Arc::new(Mutex::new(false));
    let mut factories = ChipFactoryRegistry::new();
    let created = Arc::clone(&called);
    factories.register(Chip::Sn76489, move |config| {
        *created.lock().unwrap() = true;
        Ok(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::new(Mutex::new(Vec::new())),
        )))
    });
    assert!(matches!(
        VgmSequencer::from_vgm_with_factories(bytes, 44_100, 64, Default::default(), factories),
        Err(VgmSequencerError::Parse(_))
    ));
    assert!(!*called.lock().unwrap());
}

#[test]
fn vgm_factories_receive_chip_settings_and_source_timing_without_headers() {
    let chips = [
        (Chip::Ay8910, 2_000_000),
        (Chip::Sn76489, 3_579_545),
        (Chip::Okim6258, 8_000_000),
        (Chip::Okim6295, 1_000_000),
        (Chip::C140, 44_100),
        (Chip::SegaPcm, 4_000_000),
        (Chip::Ym2203, 4_000_000),
        (Chip::Ym2608, 8_000_000),
        (Chip::Ym2610b, 8_000_000),
    ];
    let mut builder = VgmBuilder::new();
    for (chip, clock) in &chips {
        builder.register_chip(chip.clone(), Instance::Primary, *clock);
    }
    builder.add_chip_write(Instance::Primary, GameGearPsgSpec { value: 0x10 });
    builder.add_vgm_command(WaitSamples(10));
    builder.set_extra_header(VgmExtraHeader {
        chip_clocks: vec![ChipClock::new(
            ChipId::Okim6258,
            Instance::Primary,
            4_000_000,
        )],
        ..Default::default()
    });
    let mut document = builder.finalize();
    document.header.ay_chip_type = Ay8910ChipType::Ym2149;
    document.header.ay8910_flags.ym_pin26_low = true;
    document.header.sn76489_flags.gamegear_stereo = true;
    document.header.sn76489_flags.output_negate = true;
    document.header.okim6258_flags.clock_divider = 1;
    document.header.okim6258_flags.output_12bit = true;
    document.header.okim6295_clock |= 0x8000_0000;
    document.header.c140_chip_type = 2.into();
    document.header.spcm_interface = 0x0003_000c;
    document.header.ym2203_ay8910_flags.discrete_output = true;
    document.header.ym2608_ay8910_flags.raw_output = true;
    document.header.ym2610b_clock |= 0x8000_0000;
    let bytes: Vec<u8> = document.into();
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();

    for from_bytes in [false, true] {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut factories = ChipFactoryRegistry::new();
        for (chip, _) in &chips {
            let seen = Arc::clone(&seen);
            factories.register(chip.clone(), move |config| {
                seen.lock().unwrap().push((
                    config.chip().clone(),
                    config.clock(),
                    *config.settings(),
                    config.source_okim6258_clock(),
                ));
                Ok(Box::new(FakeChip::new(
                    config.chip().clone(),
                    config.clock(),
                    Arc::new(Mutex::new(Vec::new())),
                )))
            });
        }
        if from_bytes {
            VgmSequencer::from_vgm_with_factories(
                bytes.clone(),
                44_100,
                64,
                Default::default(),
                factories,
            )
            .unwrap();
        } else {
            VgmSequencer::new_with_factories(
                document.clone(),
                44_100,
                64,
                Default::default(),
                factories,
            )
            .unwrap();
        }
        let seen = seen.lock().unwrap();
        let settings = |chip| seen.iter().find(|entry| entry.0 == chip).unwrap();
        assert!(seen.iter().all(|entry| entry.3 == Some(8_000_000)));
        let ChipBackendSettings::Ay8910(ay) = settings(Chip::Ay8910).2 else {
            panic!("missing AY settings");
        };
        assert_eq!(ay.variant, Ay8910Variant::Ym2149);
        assert_eq!(ay.ym2149_clock_divider(), Some(true));
        let ChipBackendSettings::Sn76489(psg) = settings(Chip::Sn76489).2 else {
            panic!("missing SN76489 settings");
        };
        assert_eq!((psg.feedback, psg.shift_width), (9, 16));
        assert!(psg.stereo && psg.stereo_used && psg.negate);
        let oki = settings(Chip::Okim6258);
        assert_eq!(oki.1, 4_000_000);
        let ChipBackendSettings::Okim6258(oki) = oki.2 else {
            panic!("missing OKIM6258 settings");
        };
        assert_eq!(oki.divider, Okim6258Divider::Div768);
        assert!(oki.output_12bit);
        assert_eq!(
            settings(Chip::Okim6295).2,
            ChipBackendSettings::Okim6295 { pin7_high: true }
        );
        let c140 = settings(Chip::C140);
        assert_eq!(
            (c140.1, c140.2),
            (25_056_500, ChipBackendSettings::C140(C140Variant::Asic219))
        );
        assert_eq!(
            settings(Chip::SegaPcm).2,
            ChipBackendSettings::SegaPcm {
                bank_shift: 12,
                bank_mask: 3
            }
        );
        let ChipBackendSettings::Ym2203 { ssg } = settings(Chip::Ym2203).2 else {
            panic!("missing YM2203 PSG settings");
        };
        assert!(ssg.discrete_output);
        let ChipBackendSettings::Ym2608 { ssg } = settings(Chip::Ym2608).2 else {
            panic!("missing YM2608 PSG settings");
        };
        assert!(ssg.raw_output);
        assert_eq!(
            settings(Chip::Ym2610b).2,
            ChipBackendSettings::Ym2610 { is_b: true }
        );
    }
}

#[test]
fn mdx_factories_receive_the_effective_pcm_settings() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    for chip in [Chip::Ym2151, Chip::Okim6258] {
        let seen = Arc::clone(&seen);
        factories.register(chip, move |config| {
            seen.lock().unwrap().push((
                config.chip().clone(),
                config.clock(),
                *config.settings(),
                config.source_okim6258_clock(),
            ));
            Ok(Box::new(FakeChip::new(
                config.chip().clone(),
                config.clock(),
                Arc::new(Mutex::new(Vec::new())),
            )))
        });
    }
    MdxSequencer::from_mdx_with_factories(
        MDX.to_vec(),
        Some(PDX.to_vec()),
        44_100,
        64,
        Default::default(),
        factories,
    )
    .unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|entry| entry.3 == Some(8_000_000)));
    let oki = seen.iter().find(|entry| entry.0 == Chip::Okim6258).unwrap();
    assert_eq!(oki.1, 8_000_000);
    let ChipBackendSettings::Okim6258(settings) = oki.2 else {
        panic!("missing OKIM6258 settings");
    };
    assert_eq!(settings.divider, Okim6258Divider::Div512);
    assert!(!settings.output_12bit);
}

#[test]
fn failed_chip_restore_prevents_playback_and_further_restores() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.register_chip(Chip::Ym2151, Instance::Secondary, 4_000_000);
    for _ in 0..10 {
        builder.add_vgm_command(WaitSamples(128));
    }
    let document = builder.finalize();
    for from_file in [false, true] {
        let mut factories = ChipFactoryRegistry::new();
        factories.register(Chip::Ym2151, |config| {
            let mut chip = FakeChip::new(
                config.chip().clone(),
                config.clock(),
                Arc::new(Mutex::new(Vec::new())),
            );
            chip.restore_fails = config.instance() == Instance::Secondary;
            Ok(Box::new(chip))
        });
        let mut player = if from_file {
            VgmSequencer::from_vgm_with_factories(
                Vec::<u8>::from(document.clone()),
                44_100,
                64,
                Default::default(),
                factories,
            )
        } else {
            VgmSequencer::new_with_factories(
                document.clone(),
                44_100,
                64,
                Default::default(),
                factories,
            )
        }
        .unwrap();
        let checkpoint = player.checkpoint().unwrap();
        assert!(player.render().unwrap().is_some());
        assert!(matches!(
            player.restore_checkpoint(&checkpoint),
            Err(VgmSequencerError::Chip(ChipError::InvalidStateSnapshot(
                Chip::Ym2151
            )))
        ));
        assert!(matches!(
            player.render(),
            Err(VgmSequencerError::StreamFailed)
        ));
        assert!(matches!(
            player.checkpoint(),
            Err(VgmSequencerError::StreamFailed)
        ));
        assert!(matches!(
            player.restore_checkpoint(&checkpoint),
            Err(VgmSequencerError::StreamFailed)
        ));
        assert!(matches!(
            player.seek_to_frame(&checkpoint, 0),
            Err(VgmSequencerError::StreamFailed)
        ));
    }
}

#[test]
fn scheduled_voices_prepare_before_shared_flush() {
    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::Ym2151, instance, 4_000_000);
    }
    builder.add_vgm_command(WaitSamples(200));
    let trace = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&trace);
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Ym2151, move |config| {
        let mut chip = FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::new(Mutex::new(Vec::new())),
        );
        chip.schedule_trace = Some((config.instance(), Arc::clone(&captured)));
        Ok(Box::new(chip))
    });
    let mut player = VgmSequencer::new_with_factories(
        builder.finalize(),
        44_100,
        64,
        Default::default(),
        factories,
    )
    .unwrap();
    assert!(player.render().unwrap().is_some());
    let trace = trace.lock().unwrap();
    assert_eq!(trace.len(), 4);
    assert!(trace[..2].iter().all(|(event, _, _)| *event == "prepare"));
    assert!(
        trace[2..]
            .iter()
            .all(|(event, _, frame)| *event == "flush" && *frame == 64)
    );
    assert_ne!(trace[0].1, trace[1].1);
    assert_ne!(trace[2].1, trace[3].1);
}

#[test]
fn scheduled_checkpoint_requires_explicit_support_and_replayable_dac_state() {
    for (supported, stops_dac, expected) in [
        (false, false, false),
        (true, false, true),
        (true, true, false),
    ] {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Okim6258, Instance::Primary, 4_000_000);
        builder.add_vgm_command(WaitSamples(200));
        let mut factories = ChipFactoryRegistry::new();
        factories.register(Chip::Okim6258, move |config| {
            let mut chip = FakeChip::new(
                config.chip().clone(),
                config.clock(),
                Arc::new(Mutex::new(Vec::new())),
            );
            chip.scheduled_checkpoint = Some(supported);
            chip.stop_dac = stops_dac;
            Ok(Box::new(chip))
        });
        let mut player = VgmSequencer::new_with_factories(
            builder.finalize(),
            44_100,
            64,
            Default::default(),
            factories,
        )
        .unwrap();
        let checkpoint = player.checkpoint();
        if expected {
            let checkpoint = checkpoint.unwrap();
            assert!(player.render().unwrap().is_some());
            player.restore_checkpoint(&checkpoint).unwrap();
            assert_eq!(player.position_frames(), 0);
        } else {
            assert!(matches!(
                checkpoint,
                Err(VgmSequencerError::Chip(
                    ChipError::UnsupportedStateSnapshot(Chip::Okim6258)
                ))
            ));
        }
    }
}

#[test]
fn dac_notifications_use_exact_start_ids_and_all_stop_reaches_all_voices() {
    use soundlog::vgm::command::{
        DacStreamChipType, LengthMode, SetStreamData, SetupStreamControl, StartStream,
        StartStreamFastCall, StartStreamFastCallFlags, StopStream,
    };
    use soundlog::vgm::detail::{StreamChipType, UncompressedStream};
    use soundlog::vgm::header::ChipId;

    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::Okim6258, instance, 4_000_000);
    }
    builder.attach_data_block(UncompressedStream {
        chip_type: StreamChipType::Okim6258Adpcm,
        data: vec![0x77],
    });
    builder.add_vgm_command(StopStream { stream_id: 0xff });
    for (stream_id, instance) in [(0, Instance::Primary), (0xff, Instance::Secondary)] {
        builder.add_vgm_command(SetupStreamControl {
            stream_id,
            chip_type: DacStreamChipType::new(ChipId::Okim6258, instance),
            write_port: 0,
            write_command: 1,
        });
        builder.add_vgm_command(SetStreamData {
            stream_id,
            data_bank_id: 4,
            step_size: 1,
            step_base: 0,
        });
    }
    builder.add_vgm_command(StartStream {
        stream_id: 0xff,
        data_start_offset: 0,
        length_mode: LengthMode::CommandCount {
            reverse: false,
            looped: false,
        },
        data_length: 1,
    });
    builder.add_vgm_command(StopStream { stream_id: 0 });
    builder.add_vgm_command(StartStreamFastCall {
        stream_id: 0xff,
        block_id: 0,
        flags: StartStreamFastCallFlags {
            reverse: false,
            looped: false,
        },
    });
    builder.add_vgm_command(StopStream { stream_id: 0xff });
    builder.add_vgm_command(WaitSamples(1));

    let notifications = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&notifications);
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Okim6258, move |config| {
        let mut chip = FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::new(Mutex::new(Vec::new())),
        );
        chip.dac_notifications = Some((config.instance(), Arc::clone(&captured)));
        Ok(Box::new(chip))
    });
    let mut player = VgmSequencer::new_with_factories(
        builder.finalize(),
        44_100,
        64,
        Default::default(),
        factories,
    )
    .unwrap();
    while player.render().unwrap().is_some() {}
    let notifications = notifications.lock().unwrap();
    for (instance, expected) in [
        (Instance::Primary, vec![false, false, false]),
        (Instance::Secondary, vec![false, true, true, false]),
    ] {
        assert_eq!(
            notifications
                .iter()
                .filter_map(|(target, playing)| (*target == instance).then_some(*playing))
                .collect::<Vec<_>>(),
            expected
        );
    }
}

#[test]
fn replaces_ymfm_backends_independently_by_chip_type() {
    let chips = ymfm_chips();
    let mut builder = VgmBuilder::new();
    for chip in &chips {
        builder.register_chip(chip.clone(), Instance::Primary, 4_000_000);
    }
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x28,
            value: 0x50,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x2a,
            value: 0x80,
        },
    );
    builder.add_vgm_command(WaitSamples(441));

    let created = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    for chip in &chips {
        let expected = chip.clone();
        let created = Arc::clone(&created);
        let writes = Arc::clone(&writes);
        factories.register(chip.clone(), move |config| {
            assert_eq!(config.chip(), &expected);
            assert_eq!(config.instance(), Instance::Primary);
            assert_eq!(config.clock(), 4_000_000);
            created.lock().unwrap().push(expected.clone());
            Ok(Box::new(FakeChip::new(
                config.chip().clone(),
                config.clock(),
                Arc::clone(&writes),
            )))
        });
    }

    let mut player = VgmSequencer::new_with_factories(
        builder.finalize(),
        44_100,
        256,
        Default::default(),
        factories,
    )
    .unwrap();
    while player.render().unwrap().is_some() {}

    assert_eq!(*created.lock().unwrap(), chips);
    let writes = writes.lock().unwrap();
    assert_eq!(writes.len(), 2);
    assert!(writes.contains(&Chip::Ym2151));
    assert!(writes.contains(&Chip::Ym2612));
}

#[test]
fn optional_factory_routes_instances_independently_and_keeps_builtin_backend() {
    let mut builder = VgmBuilder::new();
    for instance in [Instance::Primary, Instance::Secondary] {
        builder.register_chip(Chip::Ym2151, instance, 4_000_000);
        builder.add_chip_write(
            instance,
            Ym2151Spec {
                register: 0x28,
                value: 0x50,
            },
        );
    }
    builder.add_vgm_command(WaitSamples(441));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    let created = Arc::clone(&seen);
    let fake_writes = Arc::clone(&writes);
    factories.register_optional(Chip::Ym2151, move |config| {
        created.lock().unwrap().push(config.instance());
        if config.instance() == Instance::Secondary {
            return Ok(None);
        }
        Ok(Some(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::clone(&fake_writes),
        ))))
    });
    let mut player = VgmSequencer::new_with_factories(
        builder.finalize(),
        44_100,
        256,
        Default::default(),
        factories,
    )
    .unwrap();
    while player.render().unwrap().is_some() {}
    assert_eq!(
        *seen.lock().unwrap(),
        [Instance::Primary, Instance::Secondary]
    );
    assert_eq!(*writes.lock().unwrap(), [Chip::Ym2151]);
}

#[test]
fn optional_factory_errors_do_not_fall_back() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    let mut factories = ChipFactoryRegistry::new();
    factories.register_optional(Chip::Ym2151, |_| {
        Err(ChipError::External {
            chip: Chip::Ym2151,
            message: "requested device unavailable".into(),
        })
    });
    assert!(matches!(
        VgmSequencer::new_with_factories(
            builder.finalize(),
            44_100,
            256,
            Default::default(),
            factories
        ),
        Err(VgmSequencerError::Chip(ChipError::External { .. }))
    ));
}

#[test]
fn rejects_factory_chip_type_mismatch() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Ym2151, |_config| {
        Ok(Box::new(FakeChip::new(
            Chip::Ym2612,
            4_000_000,
            Arc::new(Mutex::new(Vec::new())),
        )))
    });

    assert!(matches!(
        VgmSequencer::new_with_factories(
            builder.finalize(),
            44_100,
            256,
            Default::default(),
            factories
        ),
        Err(VgmSequencerError::Chip(ChipError::ChipMismatch {
            expected: Chip::Ym2151,
            actual: Chip::Ym2612,
        }))
    ));
}

#[test]
fn rejects_factory_clock_mismatch_against_extra_header_clock() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.set_extra_header(VgmExtraHeader {
        chip_clocks: vec![ChipClock::new(ChipId::Ym2151, Instance::Primary, 3_579_545)],
        ..Default::default()
    });
    let document = builder.finalize();
    let bytes: Vec<u8> = document.clone().into();
    for from_bytes in [false, true] {
        let mut factories = ChipFactoryRegistry::new();
        factories.register(Chip::Ym2151, |config| {
            Ok(Box::new(FakeChip::new(
                config.chip().clone(),
                4_000_000,
                Arc::new(Mutex::new(Vec::new())),
            )))
        });
        let result = if from_bytes {
            VgmSequencer::from_vgm_with_factories(
                bytes.clone(),
                44_100,
                64,
                Default::default(),
                factories,
            )
        } else {
            VgmSequencer::new_with_factories(
                document.clone(),
                44_100,
                64,
                Default::default(),
                factories,
            )
        };
        assert!(matches!(
            result,
            Err(VgmSequencerError::Chip(ChipError::ClockMismatch {
                chip: Chip::Ym2151,
                expected: 3_579_545,
                actual: 4_000_000,
            }))
        ));
    }
}

#[test]
fn mdx_sequencer_uses_registered_factories() {
    let created = Arc::new(Mutex::new(Vec::new()));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let mut factories = ChipFactoryRegistry::new();
    let seen = Arc::clone(&created);
    factories.register(Chip::Ym2151, move |config| {
        seen.lock().unwrap().push(config.chip().clone());
        Ok(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::clone(&writes),
        )))
    });

    let mut player = MdxSequencer::from_mdx_with_factories(
        MDX.to_vec(),
        None,
        44_100,
        256,
        Default::default(),
        factories,
    )
    .unwrap();
    assert_eq!(*created.lock().unwrap(), [Chip::Ym2151]);
    while player.render().unwrap().is_some() {}
}

#[test]
#[cfg(not(feature = "pwm"))]
fn factory_can_replace_a_chip_with_its_builtin_feature_disabled() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Pwm, Instance::Primary, 4_000_000);
    builder.add_vgm_command(WaitSamples(441));

    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Pwm, |config| {
        Ok(Box::new(FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::new(Mutex::new(Vec::new())),
        )))
    });

    let mut player = VgmSequencer::new_with_factories(
        builder.finalize(),
        44_100,
        256,
        Default::default(),
        factories,
    )
    .unwrap();
    assert!(player.render().unwrap().is_some());
}

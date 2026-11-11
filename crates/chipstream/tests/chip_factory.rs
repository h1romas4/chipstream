use std::sync::{Arc, Mutex};

use chipstream::chip::{ChipError, ChipRegisterWrite, ScheduledChip, SoundChip, SoundChipState};
use chipstream::sequencer::{
    Chip, ChipFactoryRegistry, Instance, MdxSequencer, VgmSequencer, VgmSequencerError,
};
use soundlog::chip::{Ym2151Spec, Ym2612Spec};
use soundlog::vgm::VgmBuilder;
use soundlog::vgm::command::WaitSamples;

const MDX: &[u8] = include_bytes!("../../soundlog/assets/mdx/mdx_fm_loop.mdx");

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
    let player = VgmSequencer::new_with_factories(document.clone(), 44_100, 64, factories).unwrap();
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
        VgmSequencer::new_with_factories(document, 44_100, 64, factories),
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
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 64, factories).unwrap();
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
            Chip::Ym2413 | Chip::Ym2151 | Chip::Ym2612 | Chip::Pwm => 2,
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

#[test]
fn failed_chip_restore_preserves_vgm_stream_position() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    for _ in 0..10 {
        builder.add_vgm_command(WaitSamples(128));
    }
    let bytes: Vec<u8> = builder.finalize().into();
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Ym2151, |config| {
        let mut chip = FakeChip::new(
            config.chip().clone(),
            config.clock(),
            Arc::new(Mutex::new(Vec::new())),
        );
        chip.restore_fails = true;
        Ok(Box::new(chip))
    });
    let mut player =
        VgmSequencer::from_vgm_with_factories(bytes.clone(), 44_100, 64, factories).unwrap();
    let mut expected = VgmSequencer::from_vgm(bytes, 44_100, 64).unwrap();
    let checkpoint = player.checkpoint().unwrap();
    for _ in 0..4 {
        assert_eq!(player.render().unwrap(), expected.render().unwrap());
    }
    let sample = player.current_sample();
    let position = player.position_frames();
    assert!(matches!(
        player.restore_checkpoint(&checkpoint),
        Err(VgmSequencerError::Chip(_))
    ));
    assert_eq!(player.current_sample(), sample);
    assert_eq!(player.position_frames(), position);
    assert_eq!(player.render().unwrap(), expected.render().unwrap());
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
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 64, factories).unwrap();
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
        let mut player =
            VgmSequencer::new_with_factories(builder.finalize(), 44_100, 64, factories).unwrap();
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
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 64, factories).unwrap();
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

    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
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
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
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
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories),
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
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories),
        Err(VgmSequencerError::Chip(ChipError::ChipMismatch {
            expected: Chip::Ym2151,
            actual: Chip::Ym2612,
        }))
    ));
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

    let mut player =
        MdxSequencer::from_mdx_with_factories(MDX.to_vec(), None, 44_100, 256, factories).unwrap();
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

    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
    assert!(player.render().unwrap().is_some());
}

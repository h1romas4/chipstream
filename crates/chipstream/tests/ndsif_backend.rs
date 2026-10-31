#![cfg(feature = "ndsif")]

use std::{cell::RefCell, rc::Rc};

use chipstream::{
    chip::{
        ChipError, NdsifAdpcmEvent, NdsifAudioControlEvent, NdsifRegisterEvent, NdsifSoundChip,
        NdsifTransport, SoundChip,
        ndsif::{NdsifAudioControl, register_okim6258_factory, register_ymfm_factories},
    },
    sequencer::{Chip, ChipFactoryRegistry, Instance, VgmSequencer, VgmSequencerError},
};
use ndsif::{Divider, RegisterWrite, WriteId, ZeroPair};
use soundlog::{
    chip::{Okim6258Spec, Ym2151Spec, Ym2608Spec, Ym2610Spec, Ym2612Spec, Ymf262Spec, Ymf278bSpec},
    vgm::{VgmBuilder, command::WaitSamples},
};

#[derive(Default)]
struct FakeTransport {
    configured: Vec<(WriteId, u8, u32)>,
    events: Vec<NdsifRegisterEvent>,
    audio_streams: Vec<(u8, u32, Divider, u32)>,
    adpcm_events: Vec<NdsifAdpcmEvent>,
    audio_controls: Vec<NdsifAudioControlEvent>,
    flushed: Vec<(usize, u32)>,
    finished: Vec<(u8, usize, u32)>,
    fail_writes: bool,
}

impl NdsifTransport for FakeTransport {
    fn configure_chip(
        &mut self,
        write_id: WriteId,
        instance: u8,
        clock_hz: u32,
    ) -> Result<(), String> {
        self.configured.push((write_id, instance, clock_hz));
        Ok(())
    }

    fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
        if self.fail_writes {
            return Err("simulated scheduler failure".to_owned());
        }
        self.events.push(event);
        Ok(())
    }

    fn configure_audio_stream(
        &mut self,
        instance: u8,
        clock_hz: u32,
        divider: Divider,
        output_rate: u32,
    ) -> Result<(), String> {
        self.audio_streams
            .push((instance, clock_hz, divider, output_rate));
        Ok(())
    }

    fn schedule_adpcm_data(&mut self, event: NdsifAdpcmEvent) -> Result<(), String> {
        self.adpcm_events.push(event);
        Ok(())
    }

    fn schedule_audio_control(&mut self, event: NdsifAudioControlEvent) -> Result<(), String> {
        self.audio_controls.push(event);
        Ok(())
    }

    fn flush_scheduled_until(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        self.flushed.push((sample_frame, output_rate));
        Ok(())
    }

    fn finish_audio_stream(
        &mut self,
        instance: u8,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        self.finished.push((instance, sample_frame, output_rate));
        Ok(())
    }
}

#[test]
fn factory_schedules_absolute_frames_and_protocol_registers() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
    builder.register_chip(Chip::Ym2608, Instance::Primary, 8_000_000);
    builder.register_chip(Chip::Ym2610b, Instance::Primary, 8_000_000);
    builder.register_chip(Chip::Ymf262, Instance::Primary, 14_318_180);
    builder.register_chip(Chip::Ymf278b, Instance::Primary, 33_868_800);
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x20,
            value: 0xc7,
        },
    );
    builder.add_vgm_command(WaitSamples(441));
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 1,
            register: 0x2a,
            value: 0x80,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2608Spec {
            port: 1,
            register: 0x08,
            value: 0x78,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2610Spec {
            port: 1,
            register: 0x10,
            value: 0x34,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymf262Spec {
            port: 1,
            register: 0x20,
            value: 0x56,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymf278bSpec {
            port: 2,
            register: 0x05,
            value: 0x67,
        },
    );

    let transport = Rc::new(RefCell::new(FakeTransport::default()));
    let mut factories = ChipFactoryRegistry::new();
    register_ymfm_factories(&mut factories, Rc::clone(&transport)).unwrap();
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
    while let Some(block) = player.render().unwrap() {
        assert!(block.frames().iter().all(|frame| *frame == [0.0, 0.0]));
    }

    let transport = transport.borrow();
    assert!(
        transport
            .configured
            .contains(&(WriteId::YM2151, 0, 4_000_000))
    );
    assert_eq!(transport.events.len(), 6);
    assert_eq!(
        transport.events[0],
        NdsifRegisterEvent {
            chip: Chip::Ym2151,
            write_id: WriteId::YM2151,
            instance: 0,
            page: 0,
            write: RegisterWrite::new(0x20, 0xc7),
            sample_frame: 0,
            sample_rate: 44_100,
        }
    );
    assert_eq!(
        transport.events[1].write_id,
        WriteId::try_from(0x53).unwrap()
    );
    assert_eq!(transport.events[1].write, RegisterWrite::new(0x2a, 0x80));
    assert_eq!(transport.events[1].sample_frame, 441);
    let writes = transport
        .events
        .iter()
        .map(|event| (event.write_id.get(), event.page, event.write))
        .collect::<Vec<_>>();
    assert!(writes.contains(&(0x57, 0, RegisterWrite::new(0x08, 0x78))));
    assert!(writes.contains(&(0x59, 0, RegisterWrite::new(0x10, 0x34))));
    assert!(writes.contains(&(0x5f, 0, RegisterWrite::new(0x20, 0x56))));
    assert!(writes.contains(&(0xd0, 2, RegisterWrite::new(0x05, 0x67))));
    assert!(
        transport.events[1..]
            .iter()
            .all(|event| event.sample_frame == 441)
    );
}

#[test]
fn transport_errors_are_returned_by_render() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x20,
            value: 0xc7,
        },
    );
    builder.add_vgm_command(WaitSamples(1));

    let transport = Rc::new(RefCell::new(FakeTransport {
        fail_writes: true,
        ..FakeTransport::default()
    }));
    let mut factories = ChipFactoryRegistry::new();
    register_ymfm_factories(&mut factories, transport).unwrap();
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();

    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::Chip(ChipError::External {
            chip: Chip::Ym2151,
            ..
        }))
    ));
}

#[test]
fn invalid_register_ports_are_reported() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 2,
            register: 0x2a,
            value: 0x80,
        },
    );
    builder.add_vgm_command(WaitSamples(1));

    let transport = Rc::new(RefCell::new(FakeTransport::default()));
    let mut factories = ChipFactoryRegistry::new();
    register_ymfm_factories(&mut factories, transport).unwrap();
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();

    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::Chip(ChipError::InvalidRegisterBank {
            chip: Chip::Ym2612,
            bank: 2,
        }))
    ));
}

#[test]
fn adapter_generates_silence_with_the_expected_channel_count() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.add_vgm_command(WaitSamples(1));

    let transport = Rc::new(RefCell::new(FakeTransport::default()));
    let shared = Rc::clone(&transport);
    let mut factories = ChipFactoryRegistry::new();
    factories.register(Chip::Ym2151, move |config| {
        assert_eq!(config.output_rate(), 44_100);
        let mut chip = NdsifSoundChip::new(config, Rc::clone(&shared))?;
        assert_eq!(chip.channels(), 2);
        let mut samples = [1; 8];
        chip.generate(&mut samples)?;
        assert_eq!(samples, [0; 8]);
        Ok(Box::new(chip))
    });

    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
    assert!(player.render().unwrap().is_some());
}

#[test]
fn okim6258_factory_schedules_adpcm_stream_and_flushes_at_block_boundaries() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 4_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 1,
            value: 0x12,
        },
    );
    builder.add_vgm_command(WaitSamples(441));
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 1,
            value: 0x34,
        },
    );
    builder.add_vgm_command(WaitSamples(441));

    let transport = Rc::new(RefCell::new(FakeTransport::default()));
    let mut factories = ChipFactoryRegistry::new();
    register_okim6258_factory(&mut factories, Rc::clone(&transport));
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 128, factories).unwrap();
    while player.render().unwrap().is_some() {}

    let transport = transport.borrow();
    assert_eq!(
        transport.audio_streams,
        [(0, 4_000_000, Divider::Div1024, 44_100)]
    );
    assert_eq!(transport.adpcm_events.len(), 71);
    assert_eq!(
        transport.adpcm_events[0],
        NdsifAdpcmEvent {
            instance: 0,
            position: ndsif::BytePosition::new(0),
            sample_frame: 0,
            sample_rate: 44_100,
            value: 0x12,
        }
    );
    assert_eq!(transport.adpcm_events[19].position.get(), 19);
    assert_eq!(transport.adpcm_events[19].value, 0x34);
    assert!(transport.flushed.contains(&(882, 44_100)));
    assert_eq!(transport.finished, [(0, 882, 44_100)]);
    assert_eq!(transport.audio_controls.len(), 2);
    assert_eq!(
        transport.audio_controls[0].control,
        NdsifAudioControl::Pan(ndsif::Pan::Both)
    );
    assert!(matches!(
        transport.audio_controls.last().map(|event| event.control),
        Some(NdsifAudioControl::End(ZeroPair::Byte80 | ZeroPair::Byte08))
    ));
    assert_eq!(transport.audio_controls.last().unwrap().position.get(), 71);
}

#[test]
fn okim6258_rejects_non_audio_registers() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 4_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Okim6258Spec {
            register: 3,
            value: 0,
        },
    );
    builder.add_vgm_command(WaitSamples(1));

    let transport = Rc::new(RefCell::new(FakeTransport::default()));
    let mut factories = ChipFactoryRegistry::new();
    register_okim6258_factory(&mut factories, transport);
    let mut player =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 64, factories).unwrap();

    assert!(matches!(
        player.render(),
        Err(VgmSequencerError::Chip(ChipError::InvalidRegister {
            chip: Chip::Okim6258,
            register: 3,
        }))
    ));
}

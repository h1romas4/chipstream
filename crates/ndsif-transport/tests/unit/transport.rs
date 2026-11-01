//! Unit tests for internal transport scheduling and stream state.

use super::dac::{append_dac_waits, is_timed_dac_data_register, ticks_for_frame};
use super::*;
use chipstream::sequencer::Chip;
use ndsif::{DacRecord, DacRecordKind, DacStatus};

#[cfg(unix)]
fn test_transport() -> (NdsifSerialTransport, serialport::TTYPort) {
    use serialport::SerialPort;

    let (mut device, mut port) = serialport::TTYPort::pair().unwrap();
    device.set_timeout(Duration::from_secs(2)).unwrap();
    port.set_timeout(Duration::from_secs(2)).unwrap();
    (
        NdsifSerialTransport {
            session: SerialSession::new(Box::new(port), Duration::from_secs(2)),
            capabilities: Vec::new(),
            data_capacity: 8192,
            event_capacity: 64,
            timed_dac_capabilities: None,
            configured_chip: false,
            audio_stream: None,
            timed_dac_stream: None,
            pending: Vec::new(),
            wall_clock_origin: None,
            host_timeline: None,
        },
        device,
    )
}

#[cfg(unix)]
#[test]
fn pending_host_timeline_waits_for_start_and_can_be_cancelled() {
    let timeline = Arc::new(NdsifHostTimeline::pending());
    let (mut transport, _device) = test_transport();
    transport.set_host_timeline(Arc::clone(&timeline)).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        tx.send(transport.pace_without_audio_stream(0, 44_100))
            .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
    timeline.start(Instant::now()).unwrap();
    assert!(timeline.start(Instant::now()).is_err());
    rx.recv_timeout(Duration::from_secs(1)).unwrap().unwrap();
    worker.join().unwrap();

    let timeline = Arc::new(NdsifHostTimeline::pending());
    let (mut transport, _device) = test_transport();
    transport.set_host_timeline(Arc::clone(&timeline)).unwrap();
    let worker = std::thread::spawn(move || transport.pace_without_audio_stream(0, 44_100));
    timeline.cancel();
    assert!(worker.join().unwrap().unwrap_err().contains("cancelled"));
}

#[cfg(unix)]
fn read_device_frame(
    device: &mut serialport::TTYPort,
    decoder: &mut ndsif::Decoder,
) -> ndsif::Frame {
    use std::io::Read;

    let mut frame = None;
    while frame.is_none() {
        let mut byte = [0];
        device.read_exact(&mut byte).unwrap();
        decoder.push(&byte, |result| frame = Some(result.unwrap()));
    }
    frame.unwrap()
}

#[cfg(unix)]
fn reply_to_device_frame(device: &mut serialport::TTYPort, frame: &ndsif::Frame, reply: Reply<'_>) {
    use std::io::Write;

    let response = Response::for_request(frame, reply)
        .unwrap()
        .to_frame()
        .unwrap()
        .encode();
    device.write_all(response.as_bytes()).unwrap();
}

#[cfg(unix)]
#[test]
fn capabilities_expose_all_discovered_targets() {
    let (mut transport, _device) = test_transport();
    let entry = ndsif::CapabilityEntry {
        write_id: 0x54,
        instance: 0,
        page: 0,
        features: 0,
        clock_mode: ndsif::ClockMode::Variable,
        fixed_clock_hz: 0,
    };
    transport.capabilities.push(entry);
    assert_eq!(transport.capabilities(), &[entry]);
}

#[cfg(unix)]
#[test]
fn recording_transport_snapshots_clock_and_audio_capabilities() {
    use crate::{NdsifRecordingTransport, NdsifRoutingTransport};
    use std::num::NonZeroUsize;

    let (mut transport, _device) = test_transport();
    transport.capabilities.extend([
        ndsif::CapabilityEntry {
            write_id: 0x54,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Fixed,
            fixed_clock_hz: 4_000_000,
        },
        ndsif::CapabilityEntry {
            write_id: 0xb7,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        },
    ]);
    let recorder = NdsifRecordingTransport::new(&transport, NonZeroUsize::new(8).unwrap());
    for (id, instance, clock) in [
        (WriteId::YM2151, 0, 4_000_000),
        (WriteId::YM2151, 0, 3_000_000),
        (WriteId::YM2151, 1, 4_000_000),
        (WriteId::OKIM6258, 0, 4_000_000),
    ] {
        assert_eq!(
            recorder.supports_route_clock(id, instance, clock),
            transport.supports_clock(id, instance, clock)
        );
    }
    for (instance, clock) in [(0, 4_000_000), (0, 123), (1, 4_000_000)] {
        assert_eq!(
            recorder.supports_route_audio(instance, clock),
            transport.supports_audio_stream(instance, clock)
        );
    }
    transport.capabilities.clear();
    assert!(recorder.supports_route_clock(WriteId::YM2151, 0, 4_000_000));
}

#[cfg(unix)]
#[test]
fn device_info_queries_and_owns_model_and_firmware() {
    let (mut transport, mut device) = test_transport();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let frame = read_device_frame(&mut device, &mut ndsif::Decoder::new());
        assert!(matches!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::GetInfo
        ));
        reply_to_device_frame(
            &mut device,
            &frame,
            Reply::Info(ndsif::DeviceInfo {
                model: "NanoDrive8",
                firmware: "1.0",
            }),
        );
        wait.recv().unwrap();
    });
    let info = transport.device_info().unwrap();
    release.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(info.model, "NanoDrive8");
    assert_eq!(info.firmware, "1.0");
}

#[cfg(unix)]
#[test]
fn reset_for_reuse_keeps_connection_and_clears_playback_state() {
    let (mut transport, mut device) = test_transport();
    transport.configured_chip = true;
    transport.wall_clock_origin = Some(Instant::now());
    transport
        .schedule_psg_write(NdsifPsgEvent {
            write_id: WriteId::SN76489,
            instance: 0,
            page: 0,
            sample_frame: 10,
            sample_rate: 44_100,
            value: 0x90,
        })
        .unwrap();
    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::Reset
        ));
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::GetInfo
        ));
        reply_to_device_frame(
            &mut device,
            &frame,
            Reply::Info(ndsif::DeviceInfo {
                model: "NanoDrive8",
                firmware: "1.0",
            }),
        );
        wait.recv().unwrap();
    });

    transport.reset_for_reuse().unwrap();
    assert!(!transport.configured_chip);
    assert!(transport.pending.is_empty());
    assert!(transport.audio_stream.is_none());
    assert!(transport.timed_dac_stream.is_none());
    assert!(transport.wall_clock_origin.is_none());
    assert!(transport.host_timeline.is_none());
    assert_eq!(transport.device_info().unwrap().model, "NanoDrive8");
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn reset_for_reuse_clears_a_cancelled_host_timeline() {
    let (mut transport, mut device) = test_transport();
    let timeline = Arc::new(NdsifHostTimeline::new(Instant::now()));
    transport.set_host_timeline(Arc::clone(&timeline)).unwrap();
    timeline.cancel();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let reset = read_device_frame(&mut device, &mut ndsif::Decoder::new());
        assert!(matches!(
            ndsif::Request::decode(&reset).unwrap(),
            ndsif::Request::Reset
        ));
        reply_to_device_frame(&mut device, &reset, Reply::Complete);
        wait.recv().unwrap();
    });
    transport.reset_for_reuse().unwrap();
    assert!(transport.host_timeline.is_none());
    assert!(transport.wall_clock_origin.is_none());
    transport
        .set_host_timeline(Arc::new(NdsifHostTimeline::new(Instant::now())))
        .unwrap();
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn failed_reset_reports_error_without_discarding_playback_state() {
    let (mut transport, mut device) = test_transport();
    transport.configured_chip = true;
    transport.wall_clock_origin = Some(Instant::now());
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for reply in [Reply::Rejected, Reply::Complete] {
            let frame = read_device_frame(&mut device, &mut decoder);
            assert!(matches!(
                ndsif::Request::decode(&frame).unwrap(),
                ndsif::Request::Reset
            ));
            reply_to_device_frame(&mut device, &frame, reply);
        }
    });
    assert!(matches!(
        transport.reset_for_reuse(),
        Err(NdsifTransportError::Rejected { .. })
    ));
    assert!(transport.configured_chip);
    assert!(transport.wall_clock_origin.is_some());
    drop(transport);
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn reset_refuses_reuse_if_audio_stream_is_still_running() {
    let (mut transport, mut device) = test_transport();
    transport.configured_chip = true;
    transport.capabilities.push(ndsif::CapabilityEntry {
        write_id: 0xb7,
        instance: 0,
        page: 0,
        features: 0,
        clock_mode: ndsif::ClockMode::Variable,
        fixed_clock_hz: 0,
    });
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let reset = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&reset).unwrap(),
            ndsif::Request::Reset
        ));
        reply_to_device_frame(&mut device, &reset, Reply::Complete);
        let status = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&status).unwrap(),
            ndsif::Request::AudioStatus { .. }
        ));
        reply_to_device_frame(
            &mut device,
            &status,
            Reply::AudioStatus(ndsif::AudioStatus {
                accepted: 1,
                played: 0,
                pending: 1,
                underflows: 0,
                overflows: 0,
                rejected: 0,
                max_pending: 1,
                flags: ndsif::StatusFlags::RUNNING,
                late_events: 0,
                max_event_lag: 0,
            }),
        );
        let reset = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&reset).unwrap(),
            ndsif::Request::Reset
        ));
        reply_to_device_frame(&mut device, &reset, Reply::Complete);
    });
    assert!(matches!(
        transport.reset_for_reuse(),
        Err(NdsifTransportError::DeviceBusy {
            stream: "OKIM6258 stream"
        })
    ));
    assert!(transport.configured_chip);
    drop(transport);
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn shared_routed_playback_can_recover_and_reset_the_same_connection() {
    use chipstream::sequencer::{Instance, VgmSequencer};
    use soundlog::chip::Ym2151Spec;
    use soundlog::vgm::{VgmBuilder, command::WaitSamples};
    use std::{cell::RefCell, rc::Rc};

    let (mut transport, mut device) = test_transport();
    transport.capabilities.push(ndsif::CapabilityEntry {
        write_id: 0x54,
        instance: 0,
        page: 0,
        features: 0,
        clock_mode: ndsif::ClockMode::Variable,
        fixed_clock_hz: 0,
    });
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for index in 0..4 {
            let frame = read_device_frame(&mut device, &mut decoder);
            match (index, ndsif::Request::decode(&frame).unwrap()) {
                (0, ndsif::Request::SetChipClock { hz: 4_000_000, .. }) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Complete);
                }
                (1, ndsif::Request::WriteRegisters { writes, .. }) => {
                    assert_eq!(writes.as_bytes(), &[0x20, 0xc7]);
                }
                (2, ndsif::Request::Reset) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Complete);
                }
                (3, ndsif::Request::GetInfo) => reply_to_device_frame(
                    &mut device,
                    &frame,
                    Reply::Info(ndsif::DeviceInfo {
                        model: "NanoDrive8",
                        firmware: "1.0",
                    }),
                ),
                (_, request) => panic!("unexpected request {index}: {request:?}"),
            }
        }
        wait.recv().unwrap();
    });

    let shared = Rc::new(RefCell::new(transport));
    let factories = crate::create_shared_routed_chip_factories(
        std::slice::from_ref(&shared),
        &[crate::NdsifRoute {
            chip: Chip::Ym2151,
            source: Instance::Primary,
            device: 0,
            target: Instance::Primary,
        }],
    )
    .unwrap();
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x20,
            value: 0xc7,
        },
    );
    builder.add_vgm_command(WaitSamples(441));
    let mut sequencer =
        VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
    while sequencer.render().unwrap().is_some() {}
    drop(sequencer);

    let mut transport = Rc::try_unwrap(shared).ok().unwrap().into_inner();
    transport.reset_for_reuse().unwrap();
    assert_eq!(transport.device_info().unwrap().model, "NanoDrive8");
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn host_paced_register_writes_keep_their_frame_timing() {
    let (mut transport, mut device) = test_transport();
    transport.configured_chip = true;
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut received = Vec::new();
        for expected in [0x11, 0x22] {
            let frame = read_device_frame(&mut device, &mut decoder);
            match ndsif::Request::decode(&frame).unwrap() {
                ndsif::Request::WriteRegisters { writes, .. } => {
                    assert_eq!(writes.as_bytes(), &[0x20, expected]);
                }
                request => panic!("unexpected request: {request:?}"),
            }
            received.push(Instant::now());
        }
        received
    });

    for (sample_frame, value) in [(0, 0x11), (3_000, 0x22)] {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                chip: Chip::Ym2151,
                write_id: WriteId::YM2151,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x20, value),
                sample_frame,
                sample_rate: 10_000,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(4_000, 10_000).unwrap();
    transport.configured_chip = false;
    let received = worker.join().unwrap();
    assert!(received[1].duration_since(received[0]) >= Duration::from_millis(100));
}

#[cfg(unix)]
#[test]
fn shared_host_timeline_paces_two_devices_against_the_same_origin() {
    let origin = Instant::now() + Duration::from_millis(250);
    let timeline = Arc::new(NdsifHostTimeline::new(origin));
    let mut device_workers = Vec::new();
    let mut readers = Vec::new();
    for _ in 0..2 {
        let (mut transport, mut device) = test_transport();
        transport.configured_chip = true;
        transport.set_host_timeline(Arc::clone(&timeline)).unwrap();
        transport
            .schedule_psg_write(NdsifPsgEvent {
                write_id: WriteId::SN76489,
                instance: 0,
                page: 0,
                sample_frame: 500,
                sample_rate: 10_000,
                value: 0x90,
            })
            .unwrap();
        let (read_complete, wait_for_read) = std::sync::mpsc::channel();
        readers.push(std::thread::spawn(move || {
            let frame = read_device_frame(&mut device, &mut ndsif::Decoder::new());
            assert!(matches!(
                ndsif::Request::decode(&frame).unwrap(),
                ndsif::Request::WriteBytes { bytes: [0x90], .. }
            ));
            let received = Instant::now();
            read_complete.send(()).unwrap();
            received
        }));
        device_workers.push(std::thread::spawn(move || {
            transport.flush_scheduled_until(500, 10_000).unwrap();
            wait_for_read.recv_timeout(Duration::from_secs(2)).unwrap();
            transport.configured_chip = false;
            drop(transport);
        }));
    }
    let received: Vec<_> = readers
        .into_iter()
        .map(|reader| reader.join().unwrap())
        .collect();
    for worker in device_workers {
        worker.join().unwrap();
    }
    for instant in &received {
        assert!(*instant >= origin + Duration::from_millis(50));
    }
    let delta = if received[0] >= received[1] {
        received[0].duration_since(received[1])
    } else {
        received[1].duration_since(received[0])
    };
    assert!(delta < Duration::from_millis(200));
}

#[cfg(unix)]
#[test]
fn cancelling_a_shared_timeline_interrupts_all_waiting_devices() {
    use serialport::SerialPort;
    use std::io::Read;

    let timeline = Arc::new(NdsifHostTimeline::new(
        Instant::now() + Duration::from_secs(5),
    ));
    let (ready, ready_rx) = std::sync::mpsc::channel();
    let (done, done_rx) = std::sync::mpsc::channel();
    let mut workers = Vec::new();
    let mut devices = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..2 {
        let (mut transport, mut device) = test_transport();
        device.set_timeout(Duration::from_millis(50)).unwrap();
        devices.push(device);
        transport.configured_chip = true;
        transport.set_host_timeline(Arc::clone(&timeline)).unwrap();
        transport
            .schedule_psg_write(NdsifPsgEvent {
                write_id: WriteId::SN76489,
                instance: 0,
                page: 0,
                sample_frame: 0,
                sample_rate: 44_100,
                value: 0x90,
            })
            .unwrap();
        let ready = ready.clone();
        let done = done.clone();
        let (release, wait_for_release) = std::sync::mpsc::channel();
        releases.push(release);
        workers.push(std::thread::spawn(move || {
            ready.send(()).unwrap();
            let result = transport.flush_scheduled_until(0, 44_100);
            transport.configured_chip = false;
            done.send(result).unwrap();
            wait_for_release
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            drop(transport);
        }));
    }
    for _ in 0..2 {
        ready_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    }
    timeline.cancel();
    for _ in 0..2 {
        let error = done_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap_err();
        assert!(error.contains("cancelled"));
    }
    for mut device in devices {
        let mut byte = [0];
        assert!(device.read(&mut byte).is_err());
    }
    for release in releases {
        release.send(()).unwrap();
    }
    for worker in workers {
        worker.join().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn host_timeline_rejects_reconfiguration_and_device_timed_streams() {
    let timeline = Arc::new(NdsifHostTimeline::new(Instant::now()));
    let (mut transport, _device) = test_transport();
    transport.set_host_timeline(Arc::clone(&timeline)).unwrap();
    assert!(matches!(
        transport.set_host_timeline(Arc::clone(&timeline)),
        Err(NdsifTransportError::HostTimelineAlreadySet)
    ));
    assert!(
        transport
            .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
            .unwrap_err()
            .contains("OKIM6258 audio stream")
    );
    transport.capabilities.push(ndsif::CapabilityEntry {
        write_id: 0xb7,
        instance: 0,
        page: 0,
        features: 0,
        clock_mode: ndsif::ClockMode::Fixed,
        fixed_clock_hz: 4_000_000,
    });
    assert!(
        transport
            .configure_chip(WriteId::OKIM6258, 0, 4_000_000)
            .unwrap_err()
            .contains("OKIM6258 audio stream")
    );
    assert!(!transport.configured_chip);

    transport.capabilities.push(ndsif::CapabilityEntry {
        write_id: 0x52,
        instance: 0,
        page: 0,
        features: 0,
        clock_mode: ndsif::ClockMode::Fixed,
        fixed_clock_hz: 8_000_000,
    });
    transport.timed_dac_capabilities = Some(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 16,
        event_capacity: 8,
        max_records: 8,
        record_bytes: 4,
        max_dac_late_us: 0,
    });
    assert!(
        transport
            .configure_chip(WriteId::YM2612, 0, 8_000_000)
            .unwrap_err()
            .contains("YM2612 timed DAC stream")
    );
    assert!(!transport.configured_chip);

    let (mut transport, _device) = test_transport();
    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    assert!(matches!(
        transport.set_host_timeline(timeline),
        Err(NdsifTransportError::HostTimelineUnsupportedStream)
    ));
}

#[cfg(unix)]
#[test]
fn host_paced_events_are_ordered_and_only_same_frame_writes_are_batched() {
    let (mut transport, mut device) = test_transport();
    transport.configured_chip = true;
    transport.wall_clock_origin = Some(Instant::now() - Duration::from_secs(1));
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut messages = Vec::new();
        for _ in 0..4 {
            let frame = read_device_frame(&mut device, &mut decoder);
            match ndsif::Request::decode(&frame).unwrap() {
                ndsif::Request::WriteRegisters { writes, .. } => {
                    messages.push(writes.as_bytes().to_vec());
                }
                ndsif::Request::WriteBytes { bytes, .. } => messages.push(bytes.to_vec()),
                request => panic!("unexpected request: {request:?}"),
            }
        }
        messages
    });
    for (sample_frame, value) in [(2, 0x33), (0, 0x11), (0, 0x22), (3, 0x44)] {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                chip: Chip::Ym2151,
                write_id: WriteId::YM2151,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x20, value),
                sample_frame,
                sample_rate: 1_000,
            })
            .unwrap();
    }
    transport
        .schedule_psg_write(NdsifPsgEvent {
            write_id: WriteId::SN76489,
            instance: 0,
            page: 0,
            sample_frame: 1,
            sample_rate: 1_000,
            value: 0x90,
        })
        .unwrap();
    transport.flush_scheduled_until(3, 1_000).unwrap();
    transport.configured_chip = false;
    assert_eq!(
        worker.join().unwrap(),
        vec![
            vec![0x20, 0x11, 0x20, 0x22],
            vec![0x90],
            vec![0x20, 0x33],
            vec![0x20, 0x44],
        ]
    );
}

#[cfg(unix)]
#[test]
fn audio_data_events_and_start_keep_their_device_order() {
    let (mut transport, mut device) = test_transport();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for index in 0..5 {
            let frame = read_device_frame(&mut device, &mut decoder);
            let request = ndsif::Request::decode(&frame).unwrap();
            match (index, request) {
                (
                    0,
                    ndsif::Request::AudioData {
                        position, adpcm, ..
                    },
                ) => {
                    assert_eq!(position.get(), 0);
                    assert_eq!(adpcm, &[0x12, 0x34]);
                }
                (
                    1,
                    ndsif::Request::AudioEvent {
                        position,
                        event: ndsif::RequestEvent::Pan(ndsif::Pan::Both),
                        ..
                    },
                ) => {
                    assert_eq!(position.get(), 0);
                }
                (2, ndsif::Request::Ping(bytes)) => {
                    assert_eq!(bytes, b"chipstream-start");
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(bytes));
                }
                (3, ndsif::Request::AudioStart { .. }) => {}
                (4, ndsif::Request::AudioStatus { .. }) => {
                    reply_to_device_frame(
                        &mut device,
                        &frame,
                        Reply::AudioStatus(ndsif::AudioStatus {
                            accepted: 2,
                            played: 0,
                            pending: 2,
                            underflows: 0,
                            overflows: 0,
                            rejected: 0,
                            max_pending: 2,
                            flags: ndsif::StatusFlags::RUNNING,
                            late_events: 0,
                            max_event_lag: 0,
                        }),
                    );
                }
                _ => panic!("unexpected audio request {index}: {request:?}"),
            }
        }
        wait.recv().unwrap();
    });

    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    transport
        .schedule_audio_control(NdsifAudioControlEvent {
            instance: 0,
            position: BytePosition::new(0),
            sample_frame: 0,
            sample_rate: 44_100,
            control: NdsifAudioControl::Pan(ndsif::Pan::Both),
        })
        .unwrap();
    for (position, value) in [0x12, 0x34].into_iter().enumerate() {
        transport
            .schedule_adpcm_data(NdsifAdpcmEvent {
                instance: 0,
                position: BytePosition::new(position as u32),
                sample_frame: 0,
                sample_rate: 44_100,
                value,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(0, 44_100).unwrap();
    transport.finish_audio_stream(0, 0, 44_100).unwrap();
    let stream = transport.audio_stream.as_ref().unwrap();
    assert!(stream.state.started());
    assert_eq!(stream.queued_event_count, 0);
    assert!(stream.last_status_check.is_some());
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn deferred_audio_prebuffers_and_prepares_before_explicit_start() {
    let (mut transport, mut device) = test_transport();
    let (observed_tx, observed_rx) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for expected in ["data", "ping", "start", "status"] {
            let frame = read_device_frame(&mut device, &mut decoder);
            match (expected, ndsif::Request::decode(&frame).unwrap()) {
                (
                    "data",
                    ndsif::Request::AudioData {
                        position, adpcm, ..
                    },
                ) => {
                    assert_eq!(position.get(), 0);
                    assert_eq!(adpcm, &[0x12, 0x34]);
                }
                ("ping", ndsif::Request::Ping(bytes)) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(bytes));
                }
                ("start", ndsif::Request::AudioStart { .. }) => {}
                ("status", ndsif::Request::AudioStatus { .. }) => {
                    reply_to_device_frame(
                        &mut device,
                        &frame,
                        Reply::AudioStatus(ndsif::AudioStatus {
                            accepted: 2,
                            played: 0,
                            pending: 2,
                            underflows: 0,
                            overflows: 0,
                            rejected: 0,
                            max_pending: 2,
                            flags: ndsif::StatusFlags::RUNNING,
                            late_events: 0,
                            max_event_lag: 0,
                        }),
                    );
                }
                (_, request) => panic!("expected {expected}, got {request:?}"),
            }
            observed_tx.send(expected).unwrap();
        }
        wait.recv().unwrap();
    });

    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    transport.defer_audio_stream_start().unwrap();
    assert!(transport.defer_audio_stream_start().is_err());
    assert!(transport.start_prepared_audio_stream().is_err());
    for (position, value) in [0x12, 0x34].into_iter().enumerate() {
        transport
            .schedule_adpcm_data(NdsifAdpcmEvent {
                instance: 0,
                position: BytePosition::new(position as u32),
                sample_frame: 0,
                sample_rate: 44_100,
                value,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(0, 44_100).unwrap();
    assert_eq!(
        observed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        "data"
    );
    transport.finish_audio_stream(0, 0, 44_100).unwrap();
    assert!(transport.start_audio_stream(0).is_err());
    assert!(transport.prepare_audio_stream_start(0).is_err());
    assert!(transport.prepare_audio_stream_start(3).is_err());
    assert!(observed_rx.recv_timeout(Duration::from_millis(30)).is_err());
    transport.prepare_audio_stream_start(2).unwrap();
    assert_eq!(
        observed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        "ping"
    );
    assert!(transport.prepare_audio_stream_start(2).is_err());
    assert!(observed_rx.recv_timeout(Duration::from_millis(30)).is_err());
    transport.start_prepared_audio_stream().unwrap();
    assert_eq!(
        observed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        "start"
    );
    assert_eq!(
        observed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
        "status"
    );
    assert!(transport.audio_stream.as_ref().unwrap().state.started());
    assert!(transport.start_prepared_audio_stream().is_err());
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn deferred_audio_reset_before_start_discards_prepared_state() {
    let (mut transport, mut device) = test_transport();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for index in 0..3 {
            let frame = read_device_frame(&mut device, &mut decoder);
            match (index, ndsif::Request::decode(&frame).unwrap()) {
                (0, ndsif::Request::AudioData { .. }) => {}
                (1, ndsif::Request::Ping(bytes)) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(bytes));
                }
                (2, ndsif::Request::Reset) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Complete);
                }
                (_, request) => panic!("unexpected request: {request:?}"),
            }
        }
        wait.recv().unwrap();
    });
    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    transport.defer_audio_stream_start().unwrap();
    transport
        .schedule_adpcm_data(NdsifAdpcmEvent {
            instance: 0,
            position: BytePosition::new(0),
            sample_frame: 0,
            sample_rate: 44_100,
            value: 0x12,
        })
        .unwrap();
    transport.flush_scheduled_until(0, 44_100).unwrap();
    transport.prepare_audio_stream_start(1).unwrap();
    transport.reset_for_reuse().unwrap();
    assert!(transport.start_prepared_audio_stream().is_err());
    assert!(transport.audio_stream.is_none());
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn failed_prepared_audio_start_requires_reset() {
    let (mut transport, mut device) = test_transport();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for index in 0..5 {
            let frame = read_device_frame(&mut device, &mut decoder);
            match (index, ndsif::Request::decode(&frame).unwrap()) {
                (0, ndsif::Request::AudioData { .. }) => {}
                (1, ndsif::Request::Ping(bytes)) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(bytes));
                }
                (2, ndsif::Request::AudioStart { .. }) => {}
                (3, ndsif::Request::AudioStatus { .. }) => reply_to_device_frame(
                    &mut device,
                    &frame,
                    Reply::AudioStatus(ndsif::AudioStatus {
                        accepted: 1,
                        played: 0,
                        pending: 1,
                        underflows: 0,
                        overflows: 0,
                        rejected: 0,
                        max_pending: 1,
                        flags: ndsif::StatusFlags::from_bits_retain(0),
                        late_events: 0,
                        max_event_lag: 0,
                    }),
                ),
                (4, ndsif::Request::Reset) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Complete);
                }
                (_, request) => panic!("unexpected request: {request:?}"),
            }
        }
        wait.recv().unwrap();
    });
    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    transport.defer_audio_stream_start().unwrap();
    transport
        .schedule_adpcm_data(NdsifAdpcmEvent {
            instance: 0,
            position: BytePosition::new(0),
            sample_frame: 0,
            sample_rate: 44_100,
            value: 0x12,
        })
        .unwrap();
    transport.flush_scheduled_until(0, 44_100).unwrap();
    transport.prepare_audio_stream_start(1).unwrap();
    assert!(
        transport
            .start_prepared_audio_stream()
            .unwrap_err()
            .contains("did not enter")
    );
    assert!(transport.prepare_audio_stream_start(1).is_err());
    assert!(transport.start_prepared_audio_stream().is_err());
    assert!(
        transport
            .flush_scheduled_until(0, 44_100)
            .unwrap_err()
            .contains("reset or close")
    );
    transport.reset_for_reuse().unwrap();
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn deferred_audio_data_capacity_never_forces_an_early_start() {
    let (mut transport, mut device) = test_transport();
    transport.data_capacity = 1;
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::AudioData { .. }
        ));
        let frame = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::Reset
        ));
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        wait.recv().unwrap();
    });
    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    transport.defer_audio_stream_start().unwrap();
    for position in 0..2 {
        transport
            .schedule_adpcm_data(NdsifAdpcmEvent {
                instance: 0,
                position: BytePosition::new(position),
                sample_frame: 0,
                sample_rate: 44_100,
                value: 0x12,
            })
            .unwrap();
    }
    assert!(
        transport
            .flush_scheduled_until(0, 44_100)
            .unwrap_err()
            .contains("prebuffer exceeds device capacity")
    );
    assert!(!transport.audio_stream.as_ref().unwrap().state.started());
    assert!(transport.prepare_audio_stream_start(1).is_err());
    transport.reset_for_reuse().unwrap();
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn deferred_audio_event_capacity_never_forces_an_early_start() {
    let (mut transport, _device) = test_transport();
    transport.event_capacity = 1;
    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    transport.defer_audio_stream_start().unwrap();
    transport.audio_stream.as_mut().unwrap().queued_event_count = 1;
    let error = transport.ensure_event_capacity(1).unwrap_err();
    assert!(error.contains("before the prepared audio stream started"));
    assert!(!transport.audio_stream.as_ref().unwrap().state.started());
    assert!(transport.prepare_audio_stream_start(1).is_err());
}

#[cfg(unix)]
#[test]
fn cancelling_timeline_interrupts_audio_capacity_waits() {
    for data_wait in [true, false] {
        let (mut transport, mut device) = test_transport();
        transport.data_capacity = 1;
        transport.event_capacity = 1;
        let timeline = Arc::new(NdsifHostTimeline::pending());
        transport
            .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
            .unwrap();
        transport
            .defer_audio_stream_start_with_cancellation(Arc::clone(&timeline))
            .unwrap();
        let status = ndsif::AudioStatus {
            accepted: 1,
            played: 0,
            pending: 1,
            underflows: 0,
            overflows: 0,
            rejected: 0,
            max_pending: 1,
            flags: ndsif::StatusFlags::RUNNING,
            late_events: 0,
            max_event_lag: 0,
        };
        let stream = transport.audio_stream.as_mut().unwrap();
        stream
            .state
            .record_data_accepted(BytePosition::new(0), 1)
            .unwrap();
        stream.update_progress(status).unwrap();
        stream.queued_event_count = 1;
        stream.queued_events.push_back((1, 1));
        let (queried, query_rx) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let fake = std::thread::spawn(move || {
            let frame = read_device_frame(&mut device, &mut ndsif::Decoder::new());
            assert!(matches!(
                ndsif::Request::decode(&frame).unwrap(),
                ndsif::Request::AudioStatus { .. }
            ));
            reply_to_device_frame(&mut device, &frame, Reply::AudioStatus(status));
            queried.send(()).unwrap();
            wait.recv().unwrap();
        });
        let (done, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = if data_wait {
                transport.send_data(&[NdsifAdpcmEvent {
                    instance: 0,
                    position: BytePosition::new(1),
                    sample_frame: 0,
                    sample_rate: 44_100,
                    value: 0,
                }])
            } else {
                transport.ensure_event_capacity(1)
            };
            done.send(result).unwrap();
        });
        query_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        timeline.cancel();
        let error = done_rx
            .recv_timeout(Duration::from_millis(500))
            .unwrap()
            .unwrap_err();
        assert!(error.contains("cancelled"));
        worker.join().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn cancelling_timeline_interrupts_timed_dac_capacity_waits() {
    for data_wait in [true, false] {
        let (mut transport, mut device) = test_transport();
        transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
            profile: DAC_PROFILE_YM2612,
            revision: 40,
            ticks_per_second: DAC_TICKS_PER_SECOND,
            record_capacity: 1,
            event_capacity: 1,
            max_records: 1,
            record_bytes: 4,
            max_dac_late_us: 0,
        }));
        let timeline = Arc::new(NdsifHostTimeline::pending());
        transport
            .defer_timed_dac_start_with_cancellation(Arc::clone(&timeline))
            .unwrap();
        let stream = transport.timed_dac_stream.as_mut().unwrap();
        stream.started = true;
        stream.next_sequence = 1;
        stream.queued_records = 1;
        stream.queued_events = 1;
        let status = DacStatus {
            profile: DAC_PROFILE_YM2612,
            state: 1,
            fault: 0,
            sealed: false,
            accepted: 1,
            consumed: 0,
            queued: 1,
            free: 0,
            event_queued: 1,
            event_consumed: 0,
            underflows: 0,
            overflows: 0,
            rejected: 0,
            late_dac: 0,
            max_late_us: 0,
            accepted_tick: 0,
            played_tick: 0,
            executed_tick: 0,
        };
        transport
            .schedule_register_write(NdsifRegisterEvent {
                chip: Chip::Ym2612,
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(if data_wait { 0x2a } else { 0x22 }, 0x80),
                sample_frame: 0,
                sample_rate: 44_100,
            })
            .unwrap();
        let (queried, query_rx) = std::sync::mpsc::channel();
        let (release, wait) = std::sync::mpsc::channel();
        let fake = std::thread::spawn(move || {
            let frame = read_device_frame(&mut device, &mut ndsif::Decoder::new());
            assert!(matches!(
                ndsif::Request::decode(&frame).unwrap(),
                ndsif::Request::DacStatus { .. }
            ));
            reply_to_device_frame(&mut device, &frame, Reply::DacStatus(status));
            queried.send(()).unwrap();
            wait.recv().unwrap();
        });
        let (done, done_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            done.send(transport.flush_scheduled_until(1, 44_100))
                .unwrap();
        });
        query_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        timeline.cancel();
        assert!(
            done_rx
                .recv_timeout(Duration::from_millis(500))
                .unwrap()
                .unwrap_err()
                .contains("cancelled")
        );
        worker.join().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
    }
}

#[cfg(unix)]
#[test]
fn deferred_timed_dac_seals_before_presentation_and_drains_after_start() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 16,
        event_capacity: 8,
        max_records: 8,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    let timeline = Arc::new(NdsifHostTimeline::pending());
    transport
        .defer_timed_dac_start_with_cancellation(Arc::clone(&timeline))
        .unwrap();
    let (observed, received) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for index in 0..6 {
            let frame = read_device_frame(&mut device, &mut decoder);
            match (index, ndsif::Request::decode(&frame).unwrap()) {
                (
                    0,
                    ndsif::Request::DacData {
                        sequence, position, ..
                    },
                ) => {
                    assert_eq!((sequence, position), (0, 0));
                }
                (
                    1,
                    ndsif::Request::DacEnd {
                        next_sequence,
                        end_tick,
                        ..
                    },
                ) => {
                    assert_eq!((next_sequence, end_tick), (1, 441));
                }
                (2, ndsif::Request::Ping(bytes)) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(bytes));
                }
                (
                    3,
                    ndsif::Request::DacStart {
                        minimum_lead_ticks, ..
                    },
                ) => {
                    assert_eq!(minimum_lead_ticks, 441);
                    reply_to_device_frame(&mut device, &frame, Reply::Complete);
                }
                (4 | 5, ndsif::Request::DacStatus { .. }) => {
                    let drained = index == 5;
                    reply_to_device_frame(
                        &mut device,
                        &frame,
                        Reply::DacStatus(DacStatus {
                            profile: DAC_PROFILE_YM2612,
                            state: if drained { 2 } else { 1 },
                            fault: 0,
                            sealed: true,
                            accepted: 1,
                            consumed: u32::from(drained),
                            queued: u32::from(!drained),
                            free: if drained { 16 } else { 15 },
                            event_queued: 0,
                            event_consumed: 0,
                            underflows: 0,
                            overflows: 0,
                            rejected: 0,
                            late_dac: 0,
                            max_late_us: 0,
                            accepted_tick: 441,
                            played_tick: if drained { 441 } else { 0 },
                            executed_tick: if drained { 441 } else { 0 },
                        }),
                    );
                }
                (_, request) => panic!("unexpected DAC request {index}: {request:?}"),
            }
            observed.send(index).unwrap();
        }
        wait.recv().unwrap();
    });
    transport.flush_scheduled_until(441, 44_100).unwrap();
    transport.finish_dac_stream(441, 44_100).unwrap();
    transport.finish_dac_stream(441, 44_100).unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), 0);
    assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
    assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
    transport
        .prepare_timed_dac_start_for_frames(441, 44_100)
        .unwrap();
    assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), 2);
    assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
    transport.start_prepared_timed_dac().unwrap();
    for index in 3..6 {
        assert_eq!(
            received.recv_timeout(Duration::from_secs(1)).unwrap(),
            index
        );
    }
    assert!(transport.timed_dac_stream.as_ref().unwrap().finished);
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn timed_dac_events_start_and_drain_keep_their_device_order() {
    let (mut transport, mut device) = test_transport();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for index in 0..7 {
            let frame = read_device_frame(&mut device, &mut decoder);
            let request = ndsif::Request::decode(&frame).unwrap();
            match (index, request) {
                (0, ndsif::Request::DacRegisterEvent { tick, writes, .. }) => {
                    assert_eq!(tick, 0);
                    assert_eq!(writes, &[0x22, 0x08]);
                }
                (
                    1,
                    ndsif::Request::DacData {
                        sequence,
                        position,
                        records,
                        ..
                    },
                ) => {
                    assert_eq!((sequence, position, records.len()), (0, 0, 2));
                    let records = records.iter().collect::<Vec<_>>();
                    assert_eq!(records[0].kind, DacRecordKind::Dac);
                    assert_eq!(records[0].value, 0x80);
                    assert_eq!(records[1].wait_ticks, 4410);
                }
                (2, ndsif::Request::Ping(bytes)) => {
                    assert_eq!(bytes, b"chipstream-dac-start");
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(bytes));
                }
                (
                    3,
                    ndsif::Request::DacStart {
                        minimum_lead_ticks, ..
                    },
                ) => {
                    assert_eq!(minimum_lead_ticks, 3528);
                    reply_to_device_frame(&mut device, &frame, Reply::Complete);
                }
                (4 | 6, ndsif::Request::DacStatus { .. }) => {
                    let drained = index == 6;
                    reply_to_device_frame(
                        &mut device,
                        &frame,
                        Reply::DacStatus(DacStatus {
                            profile: DAC_PROFILE_YM2612,
                            state: if drained { 2 } else { 1 },
                            fault: 0,
                            sealed: drained,
                            accepted: 2,
                            consumed: if drained { 2 } else { 0 },
                            queued: if drained { 0 } else { 2 },
                            free: if drained { 16 } else { 14 },
                            event_queued: if drained { 0 } else { 1 },
                            event_consumed: u32::from(drained),
                            underflows: 0,
                            overflows: 0,
                            rejected: 0,
                            late_dac: 0,
                            max_late_us: 0,
                            accepted_tick: 4410,
                            played_tick: if drained { 4410 } else { 0 },
                            executed_tick: if drained { 4410 } else { 0 },
                        }),
                    );
                }
                (
                    5,
                    ndsif::Request::DacEnd {
                        next_sequence,
                        end_tick,
                        ..
                    },
                ) => {
                    assert_eq!((next_sequence, end_tick), (2, 4410));
                }
                _ => panic!("unexpected DAC request {index}: {request:?}"),
            }
        }
        wait.recv().unwrap();
    });

    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 16,
        event_capacity: 8,
        max_records: 8,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    for (address, value) in [(0x22, 0x08), (0x2a, 0x80)] {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                chip: Chip::Ym2612,
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(address, value),
                sample_frame: 0,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(4410, 44_100).unwrap();
    transport.finish_dac_stream(4410, 44_100).unwrap();
    let stream = transport.timed_dac_stream.as_ref().unwrap();
    assert!(stream.started && stream.sealed && stream.finished);
    assert_eq!((stream.queued_records, stream.queued_events), (0, 0));
    assert!(stream.last_status_check.is_some());
    release.send(()).unwrap();
    worker.join().unwrap();
}

fn stream_with_data(accepted: usize) -> AudioStream {
    let mut state =
        AudioStreamState::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100).unwrap();
    let mut position = BytePosition::new(0);
    let mut remaining = accepted;
    while remaining > 0 {
        let count = remaining.min(ndsif::MAX_AUDIO_DATA_BYTES);
        state.record_data_accepted(position, count).unwrap();
        position = position.checked_advance(count).unwrap();
        remaining -= count;
    }
    AudioStream {
        instance: 0,
        state,
        prebuffer_bytes: 4096,
        start_mode: AudioStartMode::Automatic,
        cancellation: None,
        queued_events: VecDeque::new(),
        queued_event_count: 0,
        last_status_check: None,
    }
}

#[test]
fn audio_prebuffer_respects_device_capacity() {
    assert_eq!(
        audio_prebuffer_bytes(4_000_000, Divider::Div1024, 8192),
        3906
    );
    assert_eq!(audio_prebuffer_bytes(4_000_000, Divider::Div1024, 512), 262);
}

#[test]
fn audio_status_prunes_only_played_event_positions() {
    let mut stream = stream_with_data(4);
    stream.record_queued_events(1, 2).unwrap();
    stream.record_queued_events(3, 1).unwrap();
    let mut status = ndsif::AudioStatus {
        accepted: 4,
        played: 1,
        pending: 3,
        underflows: 0,
        overflows: 0,
        rejected: 0,
        max_pending: 4,
        flags: ndsif::StatusFlags::RUNNING,
        late_events: 0,
        max_event_lag: 0,
    };
    stream.update_progress(status).unwrap();
    assert!(stream.state.started());
    assert_eq!(stream.queued_event_count, 1);
    assert_eq!(stream.queued_events, VecDeque::from([(3, 1)]));

    status.accepted = 5;
    assert!(stream.update_progress(status).is_err());
    assert_eq!(stream.state.played().get(), 1);
    assert_eq!(stream.queued_event_count, 1);

    status.accepted = 4;
    status.played = 3;
    status.pending = 1;
    stream.update_progress(status).unwrap();
    assert_eq!(stream.queued_event_count, 0);
    assert!(stream.queued_events.is_empty());
}

#[test]
fn timed_dac_progress_validates_timeline_and_capacity_before_updating() {
    let mut stream = TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 16,
        event_capacity: 8,
        max_records: 8,
        record_bytes: 4,
        max_dac_late_us: 0,
    });
    stream.started = true;
    stream.next_sequence = 6;
    stream.accepted_tick = 100;
    let mut status = DacStatus {
        profile: DAC_PROFILE_YM2612,
        state: 1,
        fault: 0,
        sealed: false,
        accepted: 6,
        consumed: 2,
        queued: 4,
        free: 12,
        event_queued: 3,
        event_consumed: 0,
        underflows: 0,
        overflows: 0,
        rejected: 0,
        late_dac: 0,
        max_late_us: 0,
        accepted_tick: 100,
        played_tick: 20,
        executed_tick: 20,
    };
    stream.update_progress(status).unwrap();
    assert_eq!((stream.queued_records, stream.queued_events), (4, 3));

    status.accepted_tick = 101;
    assert!(
        stream
            .update_progress(status)
            .unwrap_err()
            .contains("position mismatch")
    );
    status.accepted_tick = 100;
    status.free = 11;
    assert!(
        stream
            .update_progress(status)
            .unwrap_err()
            .contains("counters are inconsistent")
    );
    status.free = 12;
    status.state = 2;
    assert!(
        stream
            .update_progress(status)
            .unwrap_err()
            .contains("before the host sealed")
    );
    assert_eq!((stream.queued_records, stream.queued_events), (4, 3));

    stream.sealed = true;
    stream.update_progress(status).unwrap();
}

#[test]
fn timed_dac_frame_positions_and_wait_records_are_exact() {
    assert_eq!(
        ticks_for_frame(48_000, 48_000, DAC_TICKS_PER_SECOND).unwrap(),
        44_100
    );
    assert_eq!(
        ticks_for_frame(24_000, 48_000, DAC_TICKS_PER_SECOND).unwrap(),
        22_050
    );

    let mut records = Vec::new();
    let mut current_tick = 0;
    append_dac_waits(&mut records, &mut current_tick, 70_000).unwrap();
    assert_eq!(
        records,
        [
            DacRecord {
                kind: DacRecordKind::Wait,
                value: 0,
                wait_ticks: u16::MAX,
            },
            DacRecord {
                kind: DacRecordKind::Wait,
                value: 0,
                wait_ticks: 4_465,
            },
        ]
    );
    assert_eq!(current_tick, 70_000);
}

#[test]
fn timed_dac_register_detection_is_limited_to_ym2612_dac_registers() {
    let event = |write_id, page, address| NdsifRegisterEvent {
        chip: Chip::Ym2612,
        write_id,
        instance: 0,
        page,
        write: ndsif::RegisterWrite::new(address, 0x80),
        sample_frame: 0,
        sample_rate: 44_100,
    };
    assert!(is_timed_dac_data_register(&event(WriteId::YM2612, 0, 0x2a)));
    assert!(is_timed_dac_data_register(&event(WriteId::YM2612, 0, 0x2b)));
    assert!(!is_timed_dac_data_register(&event(
        WriteId::YM2612,
        1,
        0x2a
    )));
    assert!(!is_timed_dac_data_register(&event(
        WriteId::YM2151,
        0,
        0x2a
    )));
}

#[test]
fn audio_data_is_sent_before_time_positioned_events() {
    let control = PendingEvent::Control(NdsifAudioControlEvent {
        instance: 0,
        position: BytePosition::new(0),
        sample_frame: 0,
        sample_rate: 48_000,
        control: NdsifAudioControl::Pan(ndsif::Pan::Both),
    });
    let data = PendingEvent::Data(NdsifAdpcmEvent {
        instance: 0,
        position: BytePosition::new(0),
        sample_frame: 1_024,
        sample_rate: 48_000,
        value: 0,
    });
    let events =
        NdsifSerialTransport::order_events(vec![control, data], Some(&stream_with_data(0)))
            .unwrap();

    assert!(matches!(events[0], PendingEvent::Data(_)));
    assert!(matches!(events[1], PendingEvent::Control(_)));
}

#[test]
fn audio_events_are_ordered_by_byte_position() {
    let stream = stream_with_data(4);
    let later = PendingEvent::Control(NdsifAudioControlEvent {
        instance: 0,
        position: BytePosition::new(2),
        sample_frame: 0,
        sample_rate: 44_100,
        control: NdsifAudioControl::Pan(ndsif::Pan::Both),
    });
    let earlier = PendingEvent::Register(NdsifRegisterEvent {
        chip: soundlog::chip::Chip::Ym2151,
        write_id: WriteId::YM2151,
        instance: 0,
        page: 0,
        write: ndsif::RegisterWrite::new(0x20, 0xc7),
        sample_frame: 0,
        sample_rate: 44_100,
    });

    let events = NdsifSerialTransport::order_events(vec![later, earlier], Some(&stream)).unwrap();
    assert!(matches!(events[0], PendingEvent::Register(_)));
    assert!(matches!(events[1], PendingEvent::Control(_)));
}

#[test]
fn timed_events_wait_until_the_audio_position_is_accepted() {
    let stream = stream_with_data(0);
    let control = PendingEvent::Control(NdsifAudioControlEvent {
        instance: 0,
        position: BytePosition::new(1),
        sample_frame: 0,
        sample_rate: 44_100,
        control: NdsifAudioControl::Pan(ndsif::Pan::Both),
    });

    assert!(!NdsifSerialTransport::event_is_ready(&control, Some(&stream)).unwrap());

    let stream = stream_with_data(1);
    assert!(NdsifSerialTransport::event_is_ready(&control, Some(&stream)).unwrap());
}

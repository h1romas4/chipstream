//! Unit tests for internal transport scheduling and stream state.

use super::dac::{append_dac_waits, is_timed_dac_data_register, ticks_for_frame};
use super::*;
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
            output_volume_supported: false,
            output_fade_timeline: None,
            last_volume_update: None,
            output_attenuation: 0,
            output_volume_modified: false,
            presentation_volume_synchronized: false,
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

#[test]
fn output_fade_uses_the_presentation_clock_and_linear_amplitude() {
    let origin = Instant::now();
    let timeline = NdsifHostTimeline::new(origin);
    assert!(
        timeline
            .schedule_output_fade(0, 0, Duration::from_secs(2))
            .is_err()
    );
    assert!(
        timeline
            .schedule_output_fade(0, 44_100, Duration::ZERO)
            .is_err()
    );
    timeline
        .schedule_output_fade(44_100, 44_100, Duration::from_secs(2))
        .unwrap();
    assert!(
        timeline
            .schedule_output_fade(0, 44_100, Duration::from_secs(1))
            .is_err()
    );
    assert_eq!(timeline.fade_attenuation(origin).unwrap(), None);
    assert_eq!(
        timeline
            .fade_attenuation(origin + Duration::from_secs(1))
            .unwrap(),
        Some(0)
    );
    assert_eq!(
        timeline
            .fade_attenuation(origin + Duration::from_secs(2))
            .unwrap(),
        Some(6)
    );
    assert_eq!(
        timeline
            .fade_attenuation(origin + Duration::from_millis(2500))
            .unwrap(),
        Some(12)
    );
    assert_eq!(
        timeline
            .fade_attenuation(origin + Duration::from_secs(3))
            .unwrap(),
        Some(96)
    );
    timeline.cancel();
    assert_eq!(
        timeline
            .fade_attenuation(origin + Duration::from_secs(4))
            .unwrap(),
        None
    );
}

#[test]
fn shared_gain_combines_with_fade_and_validates_input() {
    let origin = Instant::now();
    let timeline = NdsifHostTimeline::new(origin);
    let gain = Arc::new(NdsifOutputGain::new());
    assert_eq!(gain.get(), 1.0);
    for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert!(gain.set(invalid).is_err());
        assert_eq!(gain.get(), 1.0);
    }
    gain.set(0.5).unwrap();
    timeline.set_output_gain(Arc::clone(&gain)).unwrap();
    assert!(timeline.set_output_gain(Arc::clone(&gain)).is_err());
    timeline
        .schedule_output_fade(44_100, 44_100, Duration::from_secs(2))
        .unwrap();
    assert_eq!(timeline.fade_attenuation(origin).unwrap(), Some(6));
    assert_eq!(
        timeline
            .fade_attenuation(origin + Duration::from_secs(2))
            .unwrap(),
        Some(12)
    );
    gain.set(0.0).unwrap();
    assert_eq!(timeline.fade_attenuation(origin).unwrap(), Some(96));
    gain.set(1.0).unwrap();
    assert_eq!(timeline.fade_attenuation(origin).unwrap(), Some(0));
    assert_eq!(
        timeline
            .fade_attenuation(origin + Duration::from_secs(3))
            .unwrap(),
        Some(96)
    );
}

#[cfg(unix)]
#[test]
fn output_volume_initialization_sends_current_gain_despite_cached_values() {
    let (mut transport, mut device) = test_transport();
    let timeline = Arc::new(NdsifHostTimeline::pending());
    assert!(
        !transport
            .set_output_fade_timeline(Arc::clone(&timeline))
            .unwrap()
    );
    transport.initialize_output_volume().unwrap();
    transport.output_volume_supported = true;
    assert!(
        transport
            .set_output_fade_timeline(Arc::clone(&timeline))
            .unwrap()
    );
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for attenuation in [0, 0, 6, 6] {
            let frame = read_device_frame(&mut device, &mut decoder);
            assert_eq!(
                ndsif::Request::decode(&frame).unwrap(),
                ndsif::Request::SetOutputVolume { attenuation }
            );
            reply_to_device_frame(&mut device, &frame, Reply::Complete);
        }
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 96 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::Reset
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 0 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        wait.recv().unwrap();
    });
    transport.initialize_output_volume().unwrap();
    transport.initialize_output_volume().unwrap();
    transport.update_output_fade().unwrap();
    transport.synchronize_output_volume().unwrap();
    let gain = Arc::new(NdsifOutputGain::new());
    gain.set(0.5).unwrap();
    timeline.set_output_gain(gain).unwrap();
    transport.initialize_output_volume().unwrap();
    transport.initialize_output_volume().unwrap();
    transport.synchronize_output_volume().unwrap();
    timeline.cancel();
    transport.initialize_output_volume().unwrap();
    transport.reset_for_reuse().unwrap();
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn failed_output_volume_initialization_is_reported_and_restored() {
    let (mut transport, mut device) = test_transport();
    transport.output_volume_supported = true;
    transport
        .set_output_fade_timeline(Arc::new(NdsifHostTimeline::pending()))
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 0 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Rejected);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 96 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::Reset
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 0 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        wait.recv().unwrap();
    });
    assert!(
        transport
            .initialize_output_volume()
            .unwrap_err()
            .contains("rejected")
    );
    assert!(transport.output_volume_modified);
    transport.reset_for_reuse().unwrap();
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn output_fade_is_capability_gated_and_restored_after_cancellation() {
    let (mut transport, mut device) = test_transport();
    let timeline = Arc::new(NdsifHostTimeline::new(
        Instant::now() - Duration::from_secs(1),
    ));
    timeline
        .schedule_output_fade(0, 44_100, Duration::from_secs(2))
        .unwrap();
    assert!(
        !transport
            .set_output_fade_timeline(Arc::clone(&timeline))
            .unwrap()
    );
    transport.update_output_fade().unwrap();
    transport.output_volume_supported = true;
    assert!(
        transport
            .set_output_fade_timeline(Arc::clone(&timeline))
            .unwrap()
    );
    assert!(
        transport
            .set_output_fade_timeline(Arc::clone(&timeline))
            .is_err()
    );
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume {
                attenuation: 1..=96
            }
        ));
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        for expected in [
            ndsif::Request::SetOutputVolume { attenuation: 96 },
            ndsif::Request::Reset,
            ndsif::Request::SetOutputVolume { attenuation: 0 },
        ] {
            let frame = read_device_frame(&mut device, &mut decoder);
            assert_eq!(ndsif::Request::decode(&frame).unwrap(), expected);
            reply_to_device_frame(&mut device, &frame, Reply::Complete);
        }
        wait.recv().unwrap();
    });
    transport.update_output_fade().unwrap();
    timeline.cancel();
    transport.update_output_fade().unwrap();
    transport.reset_for_reuse().unwrap();
    assert!(!transport.output_volume_modified);
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn output_fade_rejection_is_reported_and_requires_restoration() {
    let (mut transport, mut device) = test_transport();
    transport.output_volume_supported = true;
    let timeline = Arc::new(NdsifHostTimeline::new(
        Instant::now() - Duration::from_secs(1),
    ));
    timeline
        .schedule_output_fade(0, 44_100, Duration::from_secs(2))
        .unwrap();
    transport.set_output_fade_timeline(timeline).unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        reply_to_device_frame(&mut device, &frame, Reply::Rejected);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 96 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::Reset
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 0 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        wait.recv().unwrap();
    });
    assert!(
        transport
            .update_output_fade()
            .unwrap_err()
            .contains("rejected")
    );
    assert!(transport.output_volume_modified);
    transport.reset_for_reuse().unwrap();
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn closing_after_a_volume_change_resets_and_restores_output() {
    let (mut transport, mut device) = test_transport();
    transport.output_volume_modified = true;
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::Reset
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 0 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        wait.recv().unwrap();
    });
    drop(transport);
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn reset_for_reuse_mutes_before_reset_and_restores_output() {
    let (mut transport, mut device) = test_transport();
    transport.output_volume_supported = true;
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for request in [
            ndsif::Request::SetOutputVolume { attenuation: 96 },
            ndsif::Request::Reset,
            ndsif::Request::SetOutputVolume { attenuation: 0 },
        ] {
            let frame = read_device_frame(&mut device, &mut decoder);
            assert_eq!(ndsif::Request::decode(&frame).unwrap(), request);
            reply_to_device_frame(&mut device, &frame, Reply::Complete);
        }
        wait.recv().unwrap();
    });

    transport.reset_for_reuse().unwrap();
    assert!(!transport.output_volume_modified);
    assert_eq!(transport.output_attenuation, 0);
    release.send(()).unwrap();
    worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn live_gain_changes_are_combined_with_an_active_fade() {
    let (mut transport, mut device) = test_transport();
    transport.output_volume_supported = true;
    let timeline = Arc::new(NdsifHostTimeline::new(
        Instant::now() - Duration::from_secs(1),
    ));
    timeline
        .schedule_output_fade(0, 44_100, Duration::from_secs(4))
        .unwrap();
    let gain = Arc::new(NdsifOutputGain::new());
    gain.set(0.5).unwrap();
    timeline.set_output_gain(Arc::clone(&gain)).unwrap();
    transport.set_output_fade_timeline(timeline).unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut values = Vec::new();
        for _ in 0..3 {
            let frame = read_device_frame(&mut device, &mut decoder);
            let ndsif::Request::SetOutputVolume { attenuation } =
                ndsif::Request::decode(&frame).unwrap()
            else {
                panic!("expected output-volume command");
            };
            values.push(attenuation);
            reply_to_device_frame(&mut device, &frame, Reply::Complete);
        }
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 96 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::Reset
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(
            ndsif::Request::decode(&frame).unwrap(),
            ndsif::Request::SetOutputVolume { attenuation: 0 }
        );
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        wait.recv().unwrap();
        values
    });
    transport.synchronize_output_volume().unwrap();
    gain.set(0.25).unwrap();
    transport.synchronize_output_volume().unwrap();
    gain.set(0.0).unwrap();
    transport.synchronize_output_volume().unwrap();
    transport.reset_for_reuse().unwrap();
    release.send(()).unwrap();
    let values = fake.join().unwrap();
    assert!(values[0] > 6 && values[0] < values[1], "{values:?}");
    assert_eq!(values[2], 96);
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
        (WriteId::YM2151, 0, 4_000_040),
        (WriteId::YM2151, 0, 4_000_041),
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

#[test]
fn fixed_clock_tolerance_is_bounded_and_uses_device_frequency() {
    for (fixed, requested, expected) in [
        (3_579_540, 3_579_545, true),
        (3_579_545, 3_579_540, true),
        (4_000_000, 4_000_000, true),
        (4_000_000, 3_999_960, true),
        (4_000_000, 4_000_040, true),
        (4_000_000, 3_999_959, false),
        (4_000_000, 4_000_041, false),
        (4_000_000, 0, false),
        (0, 0, false),
        (u32::MAX, u32::MAX, true),
        (u32::MAX, 0, false),
    ] {
        assert_eq!(
            fixed_clock_matches(fixed, requested),
            expected,
            "fixed={fixed}, requested={requested}"
        );
    }
}

#[cfg(unix)]
#[test]
fn fixed_clock_route_and_configuration_accept_same_tolerance_without_clock_command() {
    use crate::{NdsifRecordingTransport, NdsifRoutingTransport};
    use serialport::SerialPort;
    use std::{io::Read, num::NonZeroUsize};

    let (mut transport, mut device) = test_transport();
    let ym2413 = WriteId::try_from(0x51).unwrap();
    transport.capabilities.push(ndsif::CapabilityEntry {
        write_id: ym2413.get(),
        instance: 0,
        page: 0,
        features: 0,
        clock_mode: ClockMode::Fixed,
        fixed_clock_hz: 3_579_540,
    });
    let mut recorder = NdsifRecordingTransport::new(&transport, NonZeroUsize::new(8).unwrap());
    assert!(recorder.supports_route_clock(ym2413, 0, 3_579_545));
    assert!(!recorder.supports_route_clock(ym2413, 0, 3_579_576));
    recorder.configure_chip(ym2413, 0, 3_579_545).unwrap();
    recorder
        .take_batch()
        .unwrap()
        .replay(&mut transport)
        .unwrap();
    assert!(transport.configured_chip);
    let error = transport.configure_chip(ym2413, 0, 3_579_576).unwrap_err();
    assert!(error.contains("tolerance: 10 ppm"));
    device.set_timeout(Duration::from_millis(20)).unwrap();
    let error = device.read(&mut [0]).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ));
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
    shared
        .borrow_mut()
        .configure_chip(WriteId::YM2151, 0, 4_000_000)
        .unwrap();
    shared
        .borrow_mut()
        .schedule_register_write(NdsifRegisterEvent {
            write_id: WriteId::YM2151,
            instance: 0,
            page: 0,
            write: ndsif::RegisterWrite::new(0x20, 0xc7),
            sample_frame: 0,
            sample_rate: 44_100,
        })
        .unwrap();
    shared
        .borrow_mut()
        .flush_scheduled_until(441, 44_100)
        .unwrap();

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
fn clock_changes_reject_unsupported_device_clocks_without_mutating_the_timeline() {
    let (mut transport, _device) = test_transport();
    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    transport.capabilities.push(ndsif::CapabilityEntry {
        write_id: WriteId::OKIM6258.get(),
        instance: 0,
        page: 0,
        features: 0,
        clock_mode: ndsif::ClockMode::Fixed,
        fixed_clock_hz: 4_000_000,
    });
    let event = NdsifAudioControlEvent {
        instance: 0,
        position: BytePosition::new(0),
        sample_frame: 0,
        sample_rate: 44_100,
        control: NdsifAudioControl::Clock {
            clock_hz: 8_000_000,
            divider: Divider::Div512,
        },
    };
    assert!(
        transport
            .schedule_audio_control(event)
            .unwrap_err()
            .contains("not supported")
    );
    let stream = transport.audio_stream.as_ref().unwrap();
    assert!(stream.clock_changes.is_empty());
    assert!(transport.pending.is_empty());
    let divider_only = NdsifAudioControlEvent {
        control: NdsifAudioControl::Clock {
            clock_hz: 4_000_000,
            divider: Divider::Div512,
        },
        ..event
    };
    transport.schedule_audio_control(divider_only).unwrap();
    assert_eq!(
        transport
            .audio_stream
            .as_ref()
            .unwrap()
            .position_for_frame(4410, 44_100)
            .unwrap()
            .get(),
        390
    );
}

#[test]
fn audio_clock_history_discards_only_segments_no_longer_needed() {
    let mut stream = stream_with_data(4);
    for (sample_frame, position, clock_hz, divider) in [
        (4410, 195, 8_000_000, Divider::Div768),
        (8820, 716, 4_000_000, Divider::Div512),
    ] {
        stream
            .record_clock_change(NdsifAudioControlEvent {
                instance: 0,
                position: BytePosition::new(position),
                sample_frame,
                sample_rate: 44_100,
                control: NdsifAudioControl::Clock { clock_hz, divider },
            })
            .unwrap();
    }
    stream.discard_clock_history_before(6615);
    assert_eq!(stream.timeline.len(), 2);
    assert_eq!(stream.clock_changes.len(), 1);
    assert_eq!(stream.position_for_frame(6615, 44_100).unwrap().get(), 455);
    assert_eq!(
        stream.position_for_frame(13230, 44_100).unwrap().get(),
        1106
    );
    assert!(stream.position_for_frame(4409, 44_100).is_err());
    stream.discard_clock_history_before(13230);
    assert_eq!(stream.timeline.len(), 1);
    assert!(stream.clock_changes.is_empty());
    assert_eq!(
        stream.position_for_frame(13230, 44_100).unwrap().get(),
        1106
    );
}

#[cfg(unix)]
#[test]
fn clock_changes_and_other_chip_writes_share_the_audio_timeline() {
    let (mut transport, mut device) = test_transport();
    transport
        .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
        .unwrap();
    transport.defer_audio_stream_start().unwrap();
    let changes = [
        (8820, 716, 4_000_000, Divider::Div512),
        (4410, 195, 8_000_000, Divider::Div768),
    ];
    for (sample_frame, position, clock_hz, divider) in changes {
        transport
            .schedule_audio_control(NdsifAudioControlEvent {
                instance: 0,
                position: BytePosition::new(position),
                sample_frame,
                sample_rate: 44_100,
                control: NdsifAudioControl::Clock { clock_hz, divider },
            })
            .unwrap();
    }
    for sample_frame in [2205, 6615, 11025] {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2151,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x20, 0xc7),
                sample_frame,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    for position in 0..1107 {
        transport
            .schedule_adpcm_data(NdsifAdpcmEvent {
                instance: 0,
                position: BytePosition::new(position),
                sample_frame: 0,
                sample_rate: 44_100,
                value: 0x80,
            })
            .unwrap();
    }
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let batches = 1107_usize.div_ceil(ndsif::MAX_AUDIO_DATA_BYTES);
        let mut accepted = 0;
        let mut events = Vec::new();
        for _ in 0..batches + 6 {
            let frame = read_device_frame(&mut device, &mut decoder);
            match ndsif::Request::decode(&frame).unwrap() {
                ndsif::Request::AudioData {
                    position, adpcm, ..
                } => {
                    assert_eq!(position.get(), accepted);
                    accepted += adpcm.len() as u32;
                }
                ndsif::Request::AudioEvent {
                    position, event, ..
                } => {
                    events.push((
                        position.get(),
                        match event {
                            ndsif::RequestEvent::Registers { .. } => "register",
                            ndsif::RequestEvent::Clock {
                                clock: OkiClock::Mhz8,
                                divider: Divider::Div768,
                            } => "clock8",
                            ndsif::RequestEvent::Clock {
                                clock: OkiClock::Mhz4,
                                divider: Divider::Div512,
                            } => "clock4",
                            other => panic!("unexpected audio event: {other:?}"),
                        },
                    ));
                }
                ndsif::Request::Ping(bytes) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(bytes));
                }
                request => panic!("unexpected request: {request:?}"),
            }
        }
        assert_eq!(accepted, 1107);
        wait.recv().unwrap();
        events
    });
    transport.flush_scheduled_until(13230, 44_100).unwrap();
    assert!(
        transport
            .prepare_audio_stream_start_for_frames(13275, 44_100)
            .unwrap_err()
            .contains("prebuffer is insufficient")
    );
    transport
        .prepare_audio_stream_start_for_frames(13230, 44_100)
        .unwrap();
    assert_eq!(
        transport
            .audio_stream
            .as_ref()
            .unwrap()
            .position_for_frame(13230, 44_100)
            .unwrap()
            .get(),
        1106
    );
    let old = NdsifAudioControlEvent {
        instance: 0,
        position: BytePosition::new(195),
        sample_frame: 4410,
        sample_rate: 44_100,
        control: NdsifAudioControl::Clock {
            clock_hz: 4_000_000,
            divider: Divider::Div1024,
        },
    };
    assert!(
        transport
            .schedule_audio_control(old)
            .unwrap_err()
            .contains("earlier than the committed")
    );
    release.send(()).unwrap();
    assert_eq!(
        worker.join().unwrap(),
        vec![
            (97, "register"),
            (195, "clock8"),
            (455, "register"),
            (716, "clock4"),
            (911, "register"),
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
fn audio_prebuffer_is_complete_at_pcm_block_boundary() {
    let (mut transport, mut device) = test_transport();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut accepted = 0;
        loop {
            let frame = read_device_frame(&mut device, &mut decoder);
            match ndsif::Request::decode(&frame).unwrap() {
                ndsif::Request::AudioData {
                    position, adpcm, ..
                } => {
                    assert_eq!(position.get(), accepted);
                    accepted += adpcm.len() as u32;
                }
                ndsif::Request::Ping(bytes) => {
                    assert_eq!(accepted, 666);
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(bytes));
                    break;
                }
                request => panic!("unexpected request: {request:?}"),
            }
        }
        wait.recv().unwrap();
    });
    transport
        .configure_audio_stream(0, 8_000_000, Divider::Div512, 48_000)
        .unwrap();
    transport.defer_audio_stream_start().unwrap();
    for position in 0..666 {
        transport
            .schedule_adpcm_data(NdsifAdpcmEvent {
                instance: 0,
                position: BytePosition::new(position),
                sample_frame: 4096,
                sample_rate: 48_000,
                value: 0x77,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(4096, 48_000).unwrap();
    assert_eq!(
        transport
            .audio_stream
            .as_ref()
            .unwrap()
            .state
            .accepted()
            .get(),
        666
    );
    transport
        .prepare_audio_stream_start_for_frames(4096, 48_000)
        .unwrap();
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
        stream.events_sent = 1;
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
fn successful_retry_restores_normal_status_cadence_after_lost_reply() {
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
    transport.timed_dac_stream.as_mut().unwrap().started = true;
    let (retry_sent, retry_received) = std::sync::mpsc::channel();
    let (third_seen, third_received) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let first = read_device_frame(&mut device, &mut decoder);
        let retry = read_device_frame(&mut device, &mut decoder);
        let status = DacStatus {
            profile: DAC_PROFILE_YM2612,
            state: 1,
            fault: 0,
            sealed: false,
            accepted: 0,
            consumed: 0,
            queued: 0,
            free: 16,
            event_queued: 0,
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
        reply_to_device_frame(&mut device, &retry, Reply::DacStatus(status));
        retry_sent.send(()).unwrap();
        let third = read_device_frame(&mut device, &mut decoder);
        assert_ne!(third.transaction_id(), first.transaction_id());
        reply_to_device_frame(&mut device, &first, Reply::DacStatus(status));
        reply_to_device_frame(&mut device, &third, Reply::DacStatus(status));
        third_seen.send(()).unwrap();
        wait.recv().unwrap();
    });
    transport.check_timed_dac_status_if_due().unwrap();
    transport
        .timed_dac_stream
        .as_mut()
        .unwrap()
        .last_status_check = Some(Instant::now() - Duration::from_millis(21));
    transport.check_timed_dac_status_if_due().unwrap();
    retry_received.recv_timeout(Duration::from_secs(1)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        transport.check_timed_dac_status_if_due().unwrap();
        if transport.session.unconfirmed_bytes() == 0 {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    transport
        .timed_dac_stream
        .as_mut()
        .unwrap()
        .last_status_check = Some(Instant::now() - Duration::from_millis(6));
    transport.check_timed_dac_status_if_due().unwrap();
    third_received.recv_timeout(Duration::from_secs(1)).unwrap();
    loop {
        transport.check_timed_dac_status_if_due().unwrap();
        if transport.session.unconfirmed_bytes() == 0 {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn async_timed_dac_status_rejects_mismatched_request_counters() {
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
    transport.timed_dac_stream.as_mut().unwrap().started = true;
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let frame = read_device_frame(&mut device, &mut ndsif::Decoder::new());
        reply_to_device_frame(
            &mut device,
            &frame,
            Reply::DacStatus(DacStatus {
                profile: DAC_PROFILE_YM2612,
                state: 1,
                fault: 0,
                sealed: false,
                accepted: 1,
                consumed: 0,
                queued: 1,
                free: 15,
                event_queued: 0,
                event_consumed: 0,
                underflows: 0,
                overflows: 0,
                rejected: 0,
                late_dac: 0,
                max_late_us: 0,
                accepted_tick: 0,
                played_tick: 0,
                executed_tick: 0,
            }),
        );
        wait.recv().unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    let error = loop {
        match transport.check_timed_dac_status_if_due() {
            Ok(()) => {}
            Err(error) => break error,
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(error.contains("do not match the request fence"), "{error}");
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn async_timed_dac_status_recovers_from_a_truncated_reply() {
    use std::io::Write;
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
    transport.timed_dac_stream.as_mut().unwrap().started = true;
    let (sent, received) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let first = read_device_frame(&mut device, &mut decoder);
        let status = DacStatus {
            profile: DAC_PROFILE_YM2612,
            state: 1,
            fault: 0,
            sealed: false,
            accepted: 0,
            consumed: 0,
            queued: 0,
            free: 16,
            event_queued: 0,
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
        let partial = Response::for_request(&first, Reply::DacStatus(status))
            .unwrap()
            .to_frame()
            .unwrap()
            .encode();
        device.write_all(&partial.as_bytes()[..20]).unwrap();
        let retry = read_device_frame(&mut device, &mut decoder);
        assert_ne!(first.transaction_id(), retry.transaction_id());
        reply_to_device_frame(&mut device, &retry, Reply::DacStatus(status));
        sent.send(()).unwrap();
        wait.recv().unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        transport.check_timed_dac_status_if_due().unwrap();
        if received.try_recv().is_ok() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    loop {
        transport.check_timed_dac_status_if_due().unwrap();
        if transport.session.unconfirmed_bytes() == 0 {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn async_timed_dac_status_times_out_after_read_only_retries() {
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
    transport.timed_dac_stream.as_mut().unwrap().started = true;
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let first = read_device_frame(&mut device, &mut decoder);
        reply_to_device_frame(
            &mut device,
            &first,
            Reply::DacStatus(DacStatus {
                profile: DAC_PROFILE_YM2612,
                state: 1,
                fault: 0,
                sealed: false,
                accepted: 0,
                consumed: 0,
                queued: 0,
                free: 16,
                event_queued: 0,
                event_consumed: 0,
                underflows: 0,
                overflows: 0,
                rejected: 0,
                late_dac: 0,
                max_late_us: 0,
                accepted_tick: 0,
                played_tick: 0,
                executed_tick: 0,
            }),
        );
        let second = read_device_frame(&mut device, &mut decoder);
        let retry = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&second).unwrap(),
            ndsif::Request::DacStatus { .. }
        ));
        assert!(matches!(
            ndsif::Request::decode(&retry).unwrap(),
            ndsif::Request::DacStatus { .. }
        ));
        assert_ne!(second.transaction_id(), retry.transaction_id());
        wait.recv().unwrap();
    });
    let start = Instant::now();
    let error = loop {
        match transport.check_timed_dac_status_if_due() {
            Ok(()) => {}
            Err(error) => break error,
        }
        assert!(start.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(
        error.contains("timed out waiting for NDSIF DAC_STATUS"),
        "{error}"
    );
    assert!(start.elapsed() >= Duration::from_millis(500));
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn host_fallback_respects_unconfirmed_fm_byte_budget() {
    let (mut transport, _device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 32,
        event_capacity: 512,
        max_records: 16,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    for value in 0..520 {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x22, value as u8),
                sample_frame: 1,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(1024, 44_100).unwrap();
    let stream = transport.timed_dac_stream.as_mut().unwrap();
    stream.started = true;
    stream.started_at = Some(Instant::now() - Duration::from_secs(1));
    stream.sealed = true;
    transport.service_timed_dac_host_events(false).unwrap();
    let stream = transport.timed_dac_stream.as_ref().unwrap();
    assert_eq!(stream.host_bytes_sent, 256);
    assert_eq!(stream.host_events.len(), 520 - 112);
    assert!(transport.session.unconfirmed_bytes() <= 1024);
}

#[cfg(unix)]
#[test]
fn host_only_zero_duration_stream_is_flushed_without_starting_dac() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 32,
        event_capacity: 4,
        max_records: 16,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        match ndsif::Request::decode(&frame).unwrap() {
            ndsif::Request::WriteRegisters { writes, .. } => assert_eq!(writes.iter().count(), 5),
            other => panic!("unexpected zero-duration write: {other:?}"),
        }
        let ping = read_device_frame(&mut device, &mut decoder);
        match ndsif::Request::decode(&ping).unwrap() {
            ndsif::Request::Ping(payload) => {
                reply_to_device_frame(&mut device, &ping, Reply::Ping(payload))
            }
            other => panic!("unexpected zero-duration fence: {other:?}"),
        }
        wait.recv().unwrap();
    });
    for value in 0..5 {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x22, value),
                sample_frame: 0,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport.finish_dac_stream(0, 44_100).unwrap();
    assert!(transport.timed_dac_stream.as_ref().unwrap().finished);
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn dense_dac_samples_are_chunked_without_changing_sample_times() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 1024,
        event_capacity: 8,
        max_records: 60,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    transport
        .defer_timed_dac_start_with_cancellation(Arc::new(NdsifHostTimeline::pending()))
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut sequence = 0;
        let mut tick = 0;
        let mut samples = Vec::new();
        while tick < 882 {
            let frame = read_device_frame(&mut device, &mut decoder);
            match ndsif::Request::decode(&frame).unwrap() {
                ndsif::Request::DacData {
                    sequence: start,
                    position,
                    records,
                    ..
                } => {
                    assert_eq!((start, position), (sequence, tick));
                    let records: Vec<_> = records.iter().collect();
                    assert!(records.len() <= 60);
                    for record in records {
                        if record.kind == DacRecordKind::Dac {
                            samples.push((tick, record.value));
                        }
                        tick += u64::from(record.wait_ticks);
                        sequence += 1;
                    }
                    if tick < 882 {
                        assert!(tick - position <= 220);
                    }
                }
                ndsif::Request::Ping(payload) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(payload))
                }
                other => panic!("unexpected DAC chunk request: {other:?}"),
            }
        }
        assert_eq!(
            samples,
            (1..=441).map(|tick| (tick, tick as u8)).collect::<Vec<_>>()
        );
        wait.recv().unwrap();
    });
    for frame in 1..=441 {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x2a, frame as u8),
                sample_frame: frame,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(882, 44_100).unwrap();
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn host_fallback_waits_for_all_earlier_device_events() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 32,
        event_capacity: 4,
        max_records: 16,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    let (first_status, first_received) = std::sync::mpsc::channel();
    let (allow_drain, drain_allowed) = std::sync::mpsc::channel();
    let (completed, complete_received) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let event = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&event).unwrap(),
            ndsif::Request::DacRegisterEvent { .. }
        ));
        for _ in 0..2 {
            let data = read_device_frame(&mut device, &mut decoder);
            assert!(matches!(
                ndsif::Request::decode(&data).unwrap(),
                ndsif::Request::DacData { .. }
            ));
        }
        let mut status = DacStatus {
            profile: DAC_PROFILE_YM2612,
            state: 1,
            fault: 0,
            sealed: false,
            accepted: 2,
            consumed: 0,
            queued: 2,
            free: 30,
            event_queued: 2,
            event_consumed: 0,
            underflows: 0,
            overflows: 0,
            rejected: 0,
            late_dac: 0,
            max_late_us: 0,
            accepted_tick: 2048,
            played_tick: 0,
            executed_tick: 0,
        };
        let request = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&request).unwrap(),
            ndsif::Request::DacStatus { .. }
        ));
        reply_to_device_frame(&mut device, &request, Reply::DacStatus(status));
        first_status.send(()).unwrap();
        drain_allowed.recv().unwrap();
        let request = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&request).unwrap(),
            ndsif::Request::DacStatus { .. }
        ));
        status.event_queued = 0;
        status.event_consumed = 2;
        reply_to_device_frame(&mut device, &request, Reply::DacStatus(status));
        let writes = read_device_frame(&mut device, &mut decoder);
        match ndsif::Request::decode(&writes).unwrap() {
            ndsif::Request::WriteRegisters { writes, .. } => assert_eq!(
                writes.iter().map(|write| write.value).collect::<Vec<_>>(),
                [2, 3, 4]
            ),
            other => panic!("unexpected host handover request: {other:?}"),
        }
        completed.send(()).unwrap();
        wait.recv().unwrap();
    });
    for (frame, values) in [(0, 0..2), (1, 2..5)] {
        for value in values {
            transport
                .schedule_register_write(NdsifRegisterEvent {
                    write_id: WriteId::YM2612,
                    instance: 0,
                    page: 0,
                    write: ndsif::RegisterWrite::new(0x22, value),
                    sample_frame: frame,
                    sample_rate: 44_100,
                })
                .unwrap();
        }
        transport
            .flush_scheduled_until(if frame == 0 { 1 } else { 2048 }, 44_100)
            .unwrap();
        if frame == 0 {
            let stream = transport.timed_dac_stream.as_mut().unwrap();
            stream.started = true;
            stream.started_at = Some(Instant::now());
        }
    }
    first_received.recv_timeout(Duration::from_secs(1)).unwrap();
    transport.service_timed_dac_host_events(false).unwrap();
    allow_drain.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        transport.service_timed_dac_host_events(false).unwrap();
        if complete_received.try_recv().is_ok() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn dense_first_dac_chunk_uses_ordered_host_startup_before_start() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 32,
        event_capacity: 512,
        max_records: 16,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    transport
        .defer_timed_dac_start_with_cancellation(Arc::new(NdsifHostTimeline::pending()))
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let data = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&data).unwrap(),
            ndsif::Request::DacData { .. }
        ));
        let mut values = Vec::new();
        while values.len() < 520 {
            let writes = read_device_frame(&mut device, &mut decoder);
            match ndsif::Request::decode(&writes).unwrap() {
                ndsif::Request::WriteRegisters { writes, .. } => {
                    values.extend(writes.iter().map(|write| write.value));
                }
                other => panic!("unexpected host startup request: {other:?}"),
            }
        }
        assert_eq!(
            values,
            (0..520).map(|value| value as u8).collect::<Vec<_>>()
        );
        let ping = read_device_frame(&mut device, &mut decoder);
        match ndsif::Request::decode(&ping).unwrap() {
            ndsif::Request::Ping(payload) => {
                reply_to_device_frame(&mut device, &ping, Reply::Ping(payload))
            }
            other => panic!("unexpected startup fence: {other:?}"),
        }
        let status = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&status).unwrap(),
            ndsif::Request::DacStatus { .. }
        ));
        reply_to_device_frame(
            &mut device,
            &status,
            Reply::DacStatus(DacStatus {
                profile: DAC_PROFILE_YM2612,
                state: 0,
                fault: 0,
                sealed: false,
                accepted: 1,
                consumed: 0,
                queued: 1,
                free: 31,
                event_queued: 0,
                event_consumed: 0,
                underflows: 0,
                overflows: 0,
                rejected: 0,
                late_dac: 0,
                max_late_us: 0,
                accepted_tick: 1,
                played_tick: 0,
                executed_tick: 0,
            }),
        );
        wait.recv().unwrap();
    });
    for value in 0..520 {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x22, value as u8),
                sample_frame: 0,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport
        .schedule_register_write(NdsifRegisterEvent {
            write_id: WriteId::YM2612,
            instance: 0,
            page: 0,
            write: ndsif::RegisterWrite::new(0x2a, 0x80),
            sample_frame: 0,
            sample_rate: 44_100,
        })
        .unwrap();
    transport.flush_scheduled_until(1, 44_100).unwrap();
    transport
        .prepare_timed_dac_start_for_frames(1, 44_100)
        .unwrap();
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn host_startup_defers_key_on_and_psg_writes_until_playback() {
    let (mut transport, mut device) = test_transport();
    let mut stream = TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 32,
        event_capacity: 8,
        max_records: 16,
        record_bytes: 4,
        max_dac_late_us: 0,
    });
    stream.output_rate = Some(44_100);
    for (address, value) in [(0x22, 1), (0x28, 0xf0), (0x22, 2)] {
        stream
            .host_events
            .push_back(PendingEvent::Register(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(address, value),
                sample_frame: 0,
                sample_rate: 44_100,
            }));
    }
    stream
        .host_events
        .push_back(PendingEvent::Psg(NdsifPsgEvent {
            write_id: WriteId::SN76489,
            instance: 0,
            page: 0,
            value: 0x90,
            sample_frame: 0,
            sample_rate: 44_100,
        }));
    transport.timed_dac_stream = Some(stream);
    transport.prepare_timed_dac_host_startup().unwrap();
    let frame = read_device_frame(&mut device, &mut ndsif::Decoder::new());
    match ndsif::Request::decode(&frame).unwrap() {
        ndsif::Request::WriteRegisters { writes, .. } => {
            assert_eq!(
                writes.iter().collect::<Vec<_>>(),
                [ndsif::RegisterWrite::new(0x22, 1)]
            );
        }
        other => panic!("unexpected startup request: {other:?}"),
    }
    let pending = &transport.timed_dac_stream.as_ref().unwrap().host_events;
    assert_eq!(pending.len(), 3);
    assert!(matches!(&pending[0], PendingEvent::Register(event) if event.write.address == 0x28));
    assert!(matches!(&pending[2], PendingEvent::Psg(event) if event.value == 0x90));
    let stream = transport.timed_dac_stream.as_mut().unwrap();
    stream.started = true;
    stream.sealed = true;
    stream.started_at = Some(Instant::now());
    stream.last_status_check = Some(Instant::now());
    transport.service_timed_dac_host_events(false).unwrap();
    let mut decoder = ndsif::Decoder::new();
    let register = read_device_frame(&mut device, &mut decoder);
    match ndsif::Request::decode(&register).unwrap() {
        ndsif::Request::WriteRegisters { writes, .. } => assert_eq!(
            writes.iter().collect::<Vec<_>>(),
            [
                ndsif::RegisterWrite::new(0x28, 0xf0),
                ndsif::RegisterWrite::new(0x22, 2),
            ]
        ),
        other => panic!("unexpected register request: {other:?}"),
    }
    let psg = read_device_frame(&mut device, &mut decoder);
    match ndsif::Request::decode(&psg).unwrap() {
        ndsif::Request::WriteBytes { bytes, .. } => assert_eq!(bytes, [0x90]),
        other => panic!("unexpected PSG request: {other:?}"),
    }
    assert!(
        transport
            .timed_dac_stream
            .as_ref()
            .unwrap()
            .host_events
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn async_timed_dac_status_retries_and_accepts_late_older_reply() {
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
    transport.timed_dac_stream.as_mut().unwrap().started = true;
    let (ready, wait) = std::sync::mpsc::channel();
    let (release, done) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let first = read_device_frame(&mut device, &mut decoder);
        let second = read_device_frame(&mut device, &mut decoder);
        assert_ne!(first.transaction_id(), second.transaction_id());
        let status = DacStatus {
            profile: DAC_PROFILE_YM2612,
            state: 1,
            fault: 0,
            sealed: false,
            accepted: 0,
            consumed: 0,
            queued: 0,
            free: 16,
            event_queued: 0,
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
        reply_to_device_frame(&mut device, &second, Reply::DacStatus(status));
        reply_to_device_frame(&mut device, &first, Reply::DacStatus(status));
        ready.send(()).unwrap();
        done.recv().unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        transport.check_timed_dac_status_if_due().unwrap();
        if wait.try_recv().is_ok() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    loop {
        transport.check_timed_dac_status_if_due().unwrap();
        if transport.session.unconfirmed_bytes() == 0 {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn async_timed_dac_status_uses_request_time_counters_after_later_writes() {
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
    let (ready, wait) = std::sync::mpsc::channel();
    let (respond, receive_respond) = std::sync::mpsc::channel();
    let (release, done) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let first_event = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&first_event).unwrap(),
            ndsif::Request::DacRegisterEvent { .. }
        ));
        let first_data = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&first_data).unwrap(),
            ndsif::Request::DacData { .. }
        ));
        let request = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&request).unwrap(),
            ndsif::Request::DacStatus { .. }
        ));
        let next_event = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&next_event).unwrap(),
            ndsif::Request::DacRegisterEvent { .. }
        ));
        let next_data = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&next_data).unwrap(),
            ndsif::Request::DacData { .. }
        ));
        ready.send(()).unwrap();
        receive_respond.recv().unwrap();
        reply_to_device_frame(
            &mut device,
            &request,
            Reply::DacStatus(DacStatus {
                profile: DAC_PROFILE_YM2612,
                state: 1,
                fault: 0,
                sealed: false,
                accepted: 1,
                consumed: 0,
                queued: 1,
                free: 15,
                event_queued: 1,
                event_consumed: 0,
                underflows: 0,
                overflows: 0,
                rejected: 0,
                late_dac: 0,
                max_late_us: 0,
                accepted_tick: 1,
                played_tick: 0,
                executed_tick: 0,
            }),
        );
        done.recv().unwrap();
    });
    for frame in 0..2 {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x22, frame as u8),
                sample_frame: frame,
                sample_rate: 44_100,
            })
            .unwrap();
        transport.flush_scheduled_until(frame + 1, 44_100).unwrap();
        if frame == 0 {
            transport.timed_dac_stream.as_mut().unwrap().started = true;
            transport.check_timed_dac_status_if_due().unwrap();
        }
    }
    wait.recv().unwrap();
    let before = transport.session.unconfirmed_bytes();
    respond.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        transport.check_timed_dac_status_if_due().unwrap();
        let stream = transport.timed_dac_stream.as_ref().unwrap();
        if stream.queued_records == 2
            && stream.queued_events == 2
            && transport.session.unconfirmed_bytes() > 0
            && transport.session.unconfirmed_bytes() < before
        {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn timed_dac_vgm_initialization_without_wait_does_not_emit_keys_or_dac_values() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 32,
        event_capacity: 8,
        max_records: 16,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    for (address, value) in [(0x22, 1), (0x28, 0xf0), (0x2a, 0x55), (0x2b, 0x80)] {
        transport
            .schedule_vgm_initial_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(address, value),
                sample_frame: 0,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport
        .schedule_vgm_initial_psg_write(NdsifPsgEvent {
            write_id: WriteId::SN76489,
            instance: 0,
            page: 0,
            sample_frame: 0,
            sample_rate: 44_100,
            value: 0x90,
        })
        .unwrap();
    transport.finish_timed_dac_initialization(false).unwrap();
    let frame = read_device_frame(&mut device, &mut ndsif::Decoder::new());
    match ndsif::Request::decode(&frame).unwrap() {
        ndsif::Request::WriteRegisters { writes, .. } => {
            assert_eq!(
                writes.iter().collect::<Vec<_>>(),
                [ndsif::RegisterWrite::new(0x22, 1)]
            );
        }
        other => panic!("unexpected request: {other:?}"),
    }
    assert_eq!(transport.pending.len(), 1);
    match &transport.pending[0] {
        PendingEvent::Register(event) => assert_eq!(event.write.address, 0x2b),
        _ => panic!("expected DAC select"),
    }
}

#[cfg(unix)]
#[test]
fn timed_dac_vgm_initialization_preserves_configuration_and_final_keys() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 32,
        event_capacity: 16,
        max_records: 16,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    transport
        .defer_timed_dac_start_with_cancellation(Arc::new(NdsifHostTimeline::pending()))
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut commands = Vec::new();
        loop {
            let frame = read_device_frame(&mut device, &mut decoder);
            match ndsif::Request::decode(&frame).unwrap() {
                ndsif::Request::WriteRegisters { writes, .. } => {
                    commands.push((
                        "write",
                        writes
                            .iter()
                            .flat_map(|w| [w.address, w.value])
                            .collect::<Vec<_>>(),
                    ));
                }
                ndsif::Request::WriteBytes { bytes, .. } => commands.push(("psg", bytes.to_vec())),
                ndsif::Request::DacRegisterEvent {
                    writes,
                    tick,
                    write_id,
                    ..
                } => {
                    assert_eq!(tick, 0);
                    let name = if write_id == WriteId::SN76489 {
                        "psg-event"
                    } else {
                        "event"
                    };
                    commands.push((name, writes.to_vec()));
                }
                ndsif::Request::DacData { records, .. } => {
                    let records: Vec<_> = records
                        .iter()
                        .map(|r| (r.kind, r.value, r.wait_ticks))
                        .collect();
                    assert_eq!(
                        records,
                        [
                            (DacRecordKind::Select, 0x80, 0),
                            (DacRecordKind::Dac, 0x55, 1)
                        ]
                    );
                    commands.push(("data", vec![]));
                }
                ndsif::Request::Ping(payload) => {
                    reply_to_device_frame(&mut device, &frame, Reply::Ping(payload));
                    break;
                }
                other => panic!("unexpected request: {other:?}"),
            }
        }
        assert_eq!(
            commands,
            [
                ("write", vec![0x22, 1]),
                ("write", vec![0x22, 2]),
                ("event", vec![0x28, 0xf1, 0x28, 0x00]),
                (
                    "psg-event",
                    vec![0x9f, 0x96, 0x80, 0x0f, 0xbf, 0xb6, 0xa0, 0x14]
                ),
                ("data", vec![]),
            ]
        );
        wait.recv().unwrap();
    });
    for (address, value) in [
        (0x22, 1),
        (0x28, 0xf0),
        (0x28, 0xf1),
        (0x28, 0),
        (0x22, 2),
        (0x2a, 0x33),
        (0x2a, 0x55),
        (0x2b, 0x80),
    ] {
        transport
            .schedule_vgm_initial_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(address, value),
                sample_frame: 0,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    for value in [0x9f, 0x96, 0x80, 0x0f, 0xbf, 0xb6, 0xa0, 0x14] {
        transport
            .schedule_vgm_initial_psg_write(NdsifPsgEvent {
                write_id: WriteId::SN76489,
                instance: 0,
                page: 0,
                sample_frame: 0,
                sample_rate: 44_100,
                value,
            })
            .unwrap();
    }
    transport.finish_vgm_initialization().unwrap();
    transport.finish_vgm_initialization().unwrap();
    transport.flush_scheduled_until(1, 44_100).unwrap();
    transport.request(Command::Ping(b"test-finish")).unwrap();
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn timed_dac_collapses_only_identical_initial_dac_writes() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 32,
        event_capacity: 8,
        max_records: 32,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    transport
        .defer_timed_dac_start_with_cancellation(Arc::new(NdsifHostTimeline::pending()))
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let frame = read_device_frame(&mut device, &mut ndsif::Decoder::new());
        let ndsif::Request::DacData { records, .. } = ndsif::Request::decode(&frame).unwrap()
        else {
            panic!("expected DAC data");
        };
        let actual: Vec<_> = records
            .iter()
            .map(|record| (record.kind, record.value, record.wait_ticks))
            .collect();
        assert_eq!(
            actual,
            [
                (DacRecordKind::Select, 0x80, 0),
                (DacRecordKind::Dac, 0x80, 0),
                (DacRecordKind::Dac, 0x81, 0),
                (DacRecordKind::Select, 0, 0),
                (DacRecordKind::Dac, 0x81, 1),
                (DacRecordKind::Dac, 0x81, 0),
                (DacRecordKind::Dac, 0x81, 1),
            ]
        );
        wait.recv().unwrap();
    });
    for (frame, address, value) in [
        (0, 0x2b, 0x80),
        (0, 0x2a, 0x80),
        (0, 0x2a, 0x80),
        (0, 0x2a, 0x81),
        (0, 0x2a, 0x81),
        (0, 0x2b, 0),
        (0, 0x2a, 0x81),
        (1, 0x2a, 0x81),
        (1, 0x2a, 0x81),
    ] {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(address, value),
                sample_frame: frame,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(2, 44_100).unwrap();
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn timed_dac_bounds_unanswered_bytes_before_and_after_start() {
    let (mut transport, mut device) = test_transport();
    transport.timed_dac_stream = Some(TimedDacStream::new(DacCapabilities {
        profile: DAC_PROFILE_YM2612,
        revision: 40,
        ticks_per_second: DAC_TICKS_PER_SECOND,
        record_capacity: 1024,
        event_capacity: 512,
        max_records: 60,
        record_bytes: 4,
        max_dac_late_us: 0,
    }));
    transport
        .defer_timed_dac_start_with_cancellation(Arc::new(NdsifHostTimeline::pending()))
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut bytes_since_response = 0;
        let mut started = false;
        let mut accepted = 0;
        let mut tick = 0;
        let mut events = 0;
        let mut syncs = 0;
        let mut statuses = 0;
        loop {
            let frame = read_device_frame(&mut device, &mut decoder);
            bytes_since_response += frame.encode().as_bytes().len();
            assert!(bytes_since_response <= if started { 4096 } else { 2048 });
            let request = ndsif::Request::decode(&frame).unwrap();
            let mut done = false;
            let reply = match request {
                ndsif::Request::Ping(payload) => {
                    if payload == b"chipstream-dac-sync" {
                        syncs += 1;
                    }
                    done = payload == b"probe-finish";
                    Some(Reply::Ping(payload))
                }
                ndsif::Request::DacRegisterEvent { writes, .. } => {
                    events += (writes.len() / 2) as u32;
                    None
                }
                ndsif::Request::DacData {
                    sequence,
                    position,
                    records,
                    ..
                } => {
                    assert_eq!((sequence, position), (accepted, tick));
                    for record in records.iter() {
                        accepted += 1;
                        tick += u64::from(record.wait_ticks);
                    }
                    None
                }
                ndsif::Request::DacStart { .. } => {
                    started = true;
                    Some(Reply::Complete)
                }
                ndsif::Request::DacStatus { .. } => {
                    statuses += 1;
                    Some(Reply::DacStatus(DacStatus {
                        profile: DAC_PROFILE_YM2612,
                        state: u8::from(started),
                        fault: 0,
                        sealed: false,
                        accepted,
                        consumed: 0,
                        queued: accepted,
                        free: 1024 - accepted,
                        event_queued: if started { 0 } else { events },
                        event_consumed: if started { events } else { 0 },
                        underflows: 0,
                        overflows: 0,
                        rejected: 0,
                        late_dac: 0,
                        max_late_us: 0,
                        accepted_tick: tick,
                        played_tick: 0,
                        executed_tick: 0,
                    }))
                }
                request => panic!("unexpected DAC request: {request:?}"),
            };
            if let Some(reply) = reply {
                reply_to_device_frame(&mut device, &frame, reply);
                bytes_since_response = 0;
            }
            if done {
                break;
            }
        }
        assert!(syncs > 0, "pre-start traffic must be synchronized");
        assert!(statuses > 2, "running traffic must query status");
        assert_eq!(events, 806);
        wait.recv().unwrap();
    });

    for frame in 0..506 {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x22, 0x08),
                sample_frame: frame,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(1024, 44_100).unwrap();
    transport
        .prepare_timed_dac_start_for_frames(1024, 44_100)
        .unwrap();
    transport.start_prepared_timed_dac().unwrap();
    for frame in 1024..1324 {
        transport
            .schedule_register_write(NdsifRegisterEvent {
                write_id: WriteId::YM2612,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x22, 0x08),
                sample_frame: frame,
                sample_rate: 44_100,
            })
            .unwrap();
    }
    transport.flush_scheduled_until(2048, 44_100).unwrap();
    transport.request(Command::Ping(b"probe-finish")).unwrap();
    release.send(()).unwrap();
    fake.join().unwrap();
}

#[cfg(unix)]
#[test]
fn timed_dac_preparation_rejects_missing_device_events() {
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
    transport
        .defer_timed_dac_start_with_cancellation(Arc::new(NdsifHostTimeline::pending()))
        .unwrap();
    let stream = transport.timed_dac_stream.as_mut().unwrap();
    stream.output_rate = Some(44_100);
    stream.accepted_tick = 441;
    stream.next_sequence = 1;
    stream.queued_records = 1;
    stream.queued_events = 1;
    let (release, wait) = std::sync::mpsc::channel();
    let fake = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let ping = read_device_frame(&mut device, &mut decoder);
        let ndsif::Request::Ping(payload) = ndsif::Request::decode(&ping).unwrap() else {
            panic!("expected ping");
        };
        reply_to_device_frame(&mut device, &ping, Reply::Ping(payload));
        let status = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            ndsif::Request::decode(&status).unwrap(),
            ndsif::Request::DacStatus { .. }
        ));
        reply_to_device_frame(
            &mut device,
            &status,
            Reply::DacStatus(DacStatus {
                profile: DAC_PROFILE_YM2612,
                state: 0,
                fault: 0,
                sealed: false,
                accepted: 1,
                consumed: 0,
                queued: 1,
                free: 15,
                event_queued: 0,
                event_consumed: 0,
                underflows: 0,
                overflows: 0,
                rejected: 0,
                late_dac: 0,
                max_late_us: 0,
                accepted_tick: 441,
                played_tick: 0,
                executed_tick: 0,
            }),
        );
        wait.recv().unwrap();
    });
    let error = transport
        .prepare_timed_dac_start_for_frames(441, 44_100)
        .unwrap_err();
    assert!(error.contains("prebuffer does not match device"));
    assert!(transport.start_prepared_timed_dac().is_err());
    release.send(()).unwrap();
    fake.join().unwrap();
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
        for index in 0..7 {
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
                    4,
                    ndsif::Request::DacStart {
                        minimum_lead_ticks, ..
                    },
                ) => {
                    assert_eq!(minimum_lead_ticks, 441);
                    reply_to_device_frame(&mut device, &frame, Reply::Complete);
                }
                (3 | 5 | 6, ndsif::Request::DacStatus { .. }) => {
                    let drained = index == 6;
                    reply_to_device_frame(
                        &mut device,
                        &frame,
                        Reply::DacStatus(DacStatus {
                            profile: DAC_PROFILE_YM2612,
                            state: if drained {
                                2
                            } else if index == 3 {
                                0
                            } else {
                                1
                            },
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
    assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap(), 3);
    assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
    transport.start_prepared_timed_dac().unwrap();
    for index in 4..7 {
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
                    assert_eq!((sequence, position, records.len()), (0, 0, 1));
                    let records = records.iter().collect::<Vec<_>>();
                    assert_eq!(records[0].kind, DacRecordKind::Dac);
                    assert_eq!(records[0].value, 0x80);
                    assert_eq!(records[0].wait_ticks, 4410);
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
                            accepted: 1,
                            consumed: u32::from(drained),
                            queued: u32::from(!drained),
                            free: if drained { 16 } else { 15 },
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
                    assert_eq!((next_sequence, end_tick), (1, 4410));
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
        clock_changes: Vec::new(),
        committed_frame: None,
        timeline: vec![
            AudioPositionClock::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100)
                .unwrap(),
        ],
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
fn timed_dac_packs_sample_waits_without_changing_sample_times() {
    let mut records = Vec::new();
    let mut current_tick = 0;
    for index in 0..441_u64 {
        append_dac_waits(&mut records, &mut current_tick, index * 3).unwrap();
        records.push(DacRecord {
            kind: DacRecordKind::Dac,
            value: (index % 256) as u8,
            wait_ticks: 0,
        });
    }
    append_dac_waits(&mut records, &mut current_tick, 441 * 3).unwrap();
    assert_eq!(records.len(), 441);
    let mut tick = 0;
    for (index, record) in records.iter().enumerate() {
        assert_eq!(tick, index as u64 * 3);
        assert_eq!(record.kind, DacRecordKind::Dac);
        assert_eq!(record.value, (index % 256) as u8);
        tick += u64::from(record.wait_ticks);
    }
    assert_eq!(tick, 441 * 3);

    let mut records = vec![DacRecord {
        kind: DacRecordKind::Select,
        value: 0x80,
        wait_ticks: 0,
    }];
    let mut current_tick = 0;
    append_dac_waits(&mut records, &mut current_tick, 70_000).unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].wait_ticks, u16::MAX);
    assert_eq!(records[1].kind, DacRecordKind::Wait);
    assert_eq!(records[1].wait_ticks, 4_465);
    assert_eq!(current_tick, 70_000);
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

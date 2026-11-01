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
        },
        device,
    )
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

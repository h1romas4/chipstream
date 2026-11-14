use super::super::tests::{read_device_frame, reply_to_device_frame, test_transport};
use super::*;
use ndsif::{CapabilityEntry, LiveDacCapabilities, Request};

fn set_targets(transport: &mut NdsifSerialTransport) {
    for (write_id, page) in [(0x52, 0), (0x53, 1), (0x50, 0)] {
        transport.capabilities.push(CapabilityEntry {
            write_id,
            instance: 0,
            page,
            features: 1,
            clock_mode: ClockMode::Fixed,
            fixed_clock_hz: 7_670_454,
        });
    }
}

fn ordered_caps(revision: u8) -> OrderedLiveCapabilities {
    OrderedLiveCapabilities {
        profile: 3,
        revision,
        ticks_per_second: 44_100,
        record_capacity: 512,
        max_records: 100,
    }
}

#[test]
fn live_dac_discovers_pages_sends_unacknowledged_and_requires_reset() {
    let (mut transport, mut device) = test_transport();
    transport.control_features = CONTROL_FEATURE_LIVE_DAC;
    set_targets(&mut transport);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for (offset, next, entries) in [(0, 1, [0x52, 0]), (1, u16::MAX, [0x52, 1])] {
            let frame = read_device_frame(&mut device, &mut decoder);
            assert!(
                matches!(Request::decode(&frame).unwrap(), Request::GetLiveDacCapabilities { offset: got } if got == offset)
            );
            let caps = LiveDacCapabilities::new(2, 1, 44_100, 512, 50, next, &entries).unwrap();
            reply_to_device_frame(&mut device, &frame, Reply::LiveDacCapabilities(caps));
        }
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(frame.command_id(), 0x53);
        assert_eq!(frame.payload(), &[0, 1, 0x28, 0xf0]);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(frame.command_id(), 0x50);
        assert_eq!(frame.payload(), &[0, 0, 0x90]);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(frame.payload(), &[2, 0x52, 0, 0, 0, 0, 0, 0x80]);
        let frame = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(Request::decode(&frame).unwrap(), Request::Reset));
        reply_to_device_frame(&mut device, &frame, Reply::Complete);
        done_rx.recv().unwrap();
    });
    {
        let mut live = transport.begin_live_dac(0).unwrap();
        assert_eq!(live.max_records(), 50);
        assert_eq!(live.record_capacity(), 512);
        assert!(
            live.send(&[LiveDacRecord {
                wait_ticks: 1,
                value: 0
            }])
            .is_err()
        );
        assert!(
            live.write_registers(0, &[RegisterWrite::new(0x2a, 0)])
                .is_err()
        );
        live.write_registers(1, &[RegisterWrite::new(0x28, 0xf0)])
            .unwrap();
        live.write_psg(0, &[0x90]).unwrap();
        live.send(&[LiveDacRecord {
            wait_ticks: 0,
            value: 0x80,
        }])
        .unwrap();
    }
    assert!(matches!(
        transport.begin_live_dac(0),
        Err(NdsifTransportError::LiveResetRequired)
    ));
    assert!(
        transport
            .configure_chip(WriteId::YM2612, 0, 7_670_454)
            .is_err()
    );
    transport.reset_for_reuse().unwrap();
    done_tx.send(()).unwrap();
    worker.join().unwrap();
    assert!(transport.live_reset_ready);
}

#[test]
fn ordered_revision_five_preserves_sequence_and_status() {
    let (mut transport, mut device) = test_transport();
    transport.control_features = CONTROL_FEATURE_ORDERED_LIVE;
    set_targets(&mut transport);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        reply_to_device_frame(
            &mut device,
            &frame,
            Reply::OrderedLiveCapabilities(ordered_caps(5)),
        );
        for sequence in [0, u32::MAX, 0] {
            let frame = read_device_frame(&mut device, &mut decoder);
            let Request::OrderedLiveData {
                sequence: got,
                records,
            } = Request::decode(&frame).unwrap()
            else {
                panic!("wrong command")
            };
            assert_eq!(got, sequence);
            assert_eq!(records.len(), 2);
            assert_eq!(
                records.iter().last().unwrap().write,
                OrderedLiveWrite::Psg(0x90)
            );
        }
        let frame = read_device_frame(&mut device, &mut decoder);
        assert!(matches!(
            Request::decode(&frame).unwrap(),
            Request::GetOrderedLiveStatus
        ));
        reply_to_device_frame(
            &mut device,
            &frame,
            Reply::OrderedLiveStatus(OrderedLiveStatus {
                profile: 3,
                next_sequence: 1,
                accepted_records: 6,
                queued: 1,
                free: 511,
                missing_frames: 0,
            }),
        );
        done_rx.recv().unwrap();
    });
    let mut live = transport.begin_ordered_live().unwrap();
    let records = [
        OrderedLiveRecord {
            wait_ticks: 0,
            write: OrderedLiveWrite::Register {
                page: 1,
                address: 0x28,
                value: 0xf0,
            },
        },
        OrderedLiveRecord {
            wait_ticks: 0,
            write: OrderedLiveWrite::Psg(0x90),
        },
    ];
    live.send(&records).unwrap();
    live.sequence = u32::MAX;
    live.send(&records).unwrap();
    live.send(&records).unwrap();
    assert_eq!(live.status().unwrap().queued, 1);
    done_tx.send(()).unwrap();
    worker.join().unwrap();
}

#[test]
fn revision_four_refuses_psg_and_respects_device_payload_limit() {
    let (mut transport, mut device) = test_transport();
    transport.control_features = CONTROL_FEATURE_ORDERED_LIVE;
    transport.max_payload = 15;
    set_targets(&mut transport);
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        let mut caps = ordered_caps(4);
        caps.max_records = 5;
        reply_to_device_frame(&mut device, &frame, Reply::OrderedLiveCapabilities(caps));
        let frame = read_device_frame(&mut device, &mut decoder);
        assert_eq!(frame.payload(), &[3, 0, 0, 0, 0, 0, 0x80]);
    });
    let mut live = transport.begin_ordered_live().unwrap();
    assert!(
        live.send(&[OrderedLiveRecord {
            wait_ticks: 0,
            write: OrderedLiveWrite::Psg(1)
        }])
        .is_err()
    );
    let large = OrderedLiveRecord {
        wait_ticks: 0,
        write: OrderedLiveWrite::Register {
            page: 1,
            address: 0x28,
            value: 1,
        },
    };
    assert!(live.send(&[large, large]).is_err());
    live.send(&[OrderedLiveRecord {
        wait_ticks: 0,
        write: OrderedLiveWrite::Dac(0x80),
    }])
    .unwrap();
    worker.join().unwrap();
}

#[test]
fn live_dac_rejects_duplicate_targets_across_pages() {
    let (mut transport, mut device) = test_transport();
    transport.control_features = CONTROL_FEATURE_LIVE_DAC;
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        for next in [1, u16::MAX] {
            let frame = read_device_frame(&mut device, &mut decoder);
            let caps = LiveDacCapabilities::new(2, 1, 44_100, 512, 50, next, &[0x52, 0]).unwrap();
            reply_to_device_frame(&mut device, &frame, Reply::LiveDacCapabilities(caps));
        }
        done_rx.recv().unwrap();
    });
    assert!(matches!(
        transport.begin_live_dac(0),
        Err(NdsifTransportError::InvalidLiveCapabilities { .. })
    ));
    done_tx.send(()).unwrap();
    worker.join().unwrap();
}

#[test]
fn live_requires_explicit_reset_and_independent_feature_bits() {
    let (mut transport, _device) = test_transport();
    transport.live_reset_ready = false;
    assert!(matches!(
        transport.begin_ordered_live(),
        Err(NdsifTransportError::LiveResetRequired)
    ));
    transport.live_reset_ready = true;
    transport.control_features = CONTROL_FEATURE_LIVE_DAC;
    assert!(matches!(
        transport.begin_ordered_live(),
        Err(NdsifTransportError::UnsupportedLiveProfile { profile: 3 })
    ));
}

#[test]
fn unknown_ordered_revision_is_rejected() {
    let (mut transport, mut device) = test_transport();
    transport.control_features = CONTROL_FEATURE_ORDERED_LIVE;
    set_targets(&mut transport);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        reply_to_device_frame(
            &mut device,
            &frame,
            Reply::OrderedLiveCapabilities(ordered_caps(6)),
        );
        done_rx.recv().unwrap();
    });
    assert!(matches!(
        transport.begin_ordered_live(),
        Err(NdsifTransportError::InvalidLiveCapabilities { .. })
    ));
    done_tx.send(()).unwrap();
    worker.join().unwrap();
}

#[test]
fn failed_serial_write_poisoned_until_reset() {
    let (mut transport, mut device) = test_transport();
    transport.control_features = CONTROL_FEATURE_ORDERED_LIVE;
    set_targets(&mut transport);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let frame = read_device_frame(&mut device, &mut decoder);
        reply_to_device_frame(
            &mut device,
            &frame,
            Reply::OrderedLiveCapabilities(ordered_caps(5)),
        );
        done_rx.recv().unwrap();
    });
    let mut live = transport.begin_ordered_live().unwrap();
    done_tx.send(()).unwrap();
    worker.join().unwrap();
    let records = [OrderedLiveRecord {
        wait_ticks: 0,
        write: OrderedLiveWrite::Dac(0x80),
    }];
    assert!(matches!(
        live.send(&records),
        Err(NdsifTransportError::Io { .. })
    ));
    assert!(matches!(
        live.send(&records),
        Err(NdsifTransportError::LiveWriteIndeterminate)
    ));
    assert!(matches!(
        transport.begin_ordered_live(),
        Err(NdsifTransportError::LiveResetRequired)
    ));
}

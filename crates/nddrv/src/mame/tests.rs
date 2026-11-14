use super::*;
use std::io::{Read, Write};
use std::sync::mpsc;

use ndsif::{Capabilities, MessageType, OrderedLiveCapabilities, Reply, Request, Response};
use ndsif_transport::NdsifSerialConfig;
use serialport::SerialPort;

fn test_connection(
    plugin: Plugin,
) -> (
    NdsifSerialTransport,
    Receiver<ndsif::Frame>,
    Arc<AtomicBool>,
    std::thread::JoinHandle<()>,
) {
    let (mut device, port) = serialport::TTYPort::pair().unwrap();
    let name = port.name().unwrap();
    device.set_timeout(Duration::from_millis(100)).unwrap();
    let (frames_tx, frames_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = Arc::clone(&stop);
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut bytes = [0; 512];
        while !worker_stop.load(Ordering::Acquire) {
            match device.read(&mut bytes) {
                Ok(0) => continue,
                Ok(count) => {
                    let mut frames = Vec::new();
                    decoder.push(&bytes[..count], |frame| frames.push(frame.unwrap()));
                    for frame in frames {
                        if frame.message_type() == MessageType::NoResponse {
                            frames_tx.send(frame).unwrap();
                            continue;
                        }
                        let reply = match frame.command_id() {
                            0 => Reply::Complete,
                            1 => Reply::Ping(frame.payload()),
                            3 => Reply::Complete,
                            5 => {
                                let mut entries = Vec::new();
                                let chips = match plugin {
                                    Plugin::NanoDrive6 => vec![
                                        (0x52, 0, 7_670_454_u32),
                                        (0x53, 1, 7_670_454),
                                        (0x50, 0, 3_579_545),
                                    ],
                                    Plugin::NanoDrive8 => vec![(0x54, 0, 4_000_000)],
                                };
                                for (id, page, clock) in chips {
                                    entries.extend_from_slice(&[id, 0, page, 1, 1]);
                                    entries.extend_from_slice(&clock.to_le_bytes());
                                }
                                let caps = Capabilities::new(
                                    256,
                                    0,
                                    0,
                                    1 | if plugin == Plugin::NanoDrive6 { 8 } else { 0 },
                                    u16::MAX,
                                    &entries,
                                )
                                .unwrap();
                                let response =
                                    Response::for_request(&frame, Reply::Capabilities(caps))
                                        .unwrap()
                                        .encode()
                                        .unwrap();
                                device.write_all(response.as_bytes()).unwrap();
                                continue;
                            }
                            0x21 => Reply::OrderedLiveCapabilities(OrderedLiveCapabilities {
                                profile: 3,
                                revision: 5,
                                ticks_per_second: 44_100,
                                record_capacity: 512,
                                max_records: 100,
                            }),
                            opcode => panic!("unexpected request {opcode:02x}"),
                        };
                        let response = Response::for_request(&frame, reply)
                            .unwrap()
                            .encode()
                            .unwrap();
                        device.write_all(response.as_bytes()).unwrap();
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
                Err(error) => panic!("fake device read: {error}"),
            }
        }
    });
    let transport = NdsifSerialTransport::open(NdsifSerialConfig {
        port: name,
        baud_rate: 115_200,
        response_timeout: Duration::from_secs(2),
        reset_device: false,
    })
    .unwrap();
    drop(port);
    (transport, frames_rx, stop, worker)
}

fn sockets() -> (TcpListener, TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, _) = listener.accept().unwrap();
    listener.set_nonblocking(true).unwrap();
    server.set_read_timeout(Some(POLL)).unwrap();
    (listener, client, server)
}

#[test]
fn clock_skip_bounds_drift_and_preserves_non_dac_writes() {
    let origin = Instant::now();
    let mut clock = ClockSkip::default();
    assert_eq!(clock.adjust(0, 0, origin, false), (0, false));
    assert_eq!(clock.adjust(500, 500, origin, false), (88, false));
    assert_eq!(clock.adjust(1_000, 500, origin, true), (0, true));
}

#[test]
fn input_stop_takes_priority_over_pending_events() {
    let (listener, _client, mut socket) = sockets();
    let mut input = Input::new(Plugin::NanoDrive6);
    input.pending.push_back(Event::Reset);
    assert!(
        input
            .next(&listener, &mut socket, &AtomicBool::new(true))
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn nd6_plugin_stream_reaches_ordered_device() {
    let (mut transport, frames, device_stop, device_worker) = test_connection(Plugin::NanoDrive6);
    let (listener, mut client, mut socket) = sockets();
    let mut message = vec![b'R', b'C'];
    message.extend_from_slice(&7_670_454_u32.to_le_bytes());
    message.push(b'Y');
    message.extend_from_slice(&5_u64.to_le_bytes());
    message.extend_from_slice(&[0, 0x2a, 0x80]);
    message.push(b'G');
    message.extend_from_slice(&9_u64.to_le_bytes());
    message.push(0x90);
    message.push(b'P');
    message.extend_from_slice(&0_u64.to_le_bytes());
    message.push(b'U');
    message.extend_from_slice(&0_u64.to_le_bytes());
    message.push(b'Q');
    client.write_all(&message).unwrap();
    let (events, _) = mpsc::channel();
    nd6_connection(
        &mut transport,
        &listener,
        &mut socket,
        &AtomicBool::new(false),
        &events,
    )
    .unwrap();
    let frame = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    let Request::OrderedLiveData { sequence, records } = Request::decode(&frame).unwrap() else {
        panic!("unexpected frame")
    };
    assert_eq!(sequence, 0);
    assert_eq!(
        records.iter().collect::<Vec<_>>(),
        vec![
            OrderedLiveRecord {
                wait_ticks: 0,
                write: OrderedLiveWrite::Dac(0x80)
            },
            OrderedLiveRecord {
                wait_ticks: 4,
                write: OrderedLiveWrite::Psg(0x90)
            },
        ]
    );
    transport.reset_for_reuse().unwrap();
    device_stop.store(true, Ordering::Release);
    device_worker.join().unwrap();
}

#[cfg(unix)]
#[test]
fn nd8_plugin_stream_reaches_ym2151_device() {
    let (mut transport, frames, device_stop, device_worker) = test_connection(Plugin::NanoDrive8);
    let (listener, mut client, mut socket) = sockets();
    client
        .write_all(b"I\x2a\0\0\0\x03\x03abc:ymRW\x20\x7fW\x21\x11PQ")
        .unwrap();
    let (events, _) = mpsc::channel();
    nd8_connection(
        &mut transport,
        &listener,
        &mut socket,
        &AtomicBool::new(false),
        &events,
        |pid, name, tag, _| {
            assert_eq!((pid, name, tag), (42, "abc", ":ym"));
            Ok(4_000_000)
        },
    )
    .unwrap();
    let frame = frames.recv_timeout(Duration::from_secs(2)).unwrap();
    let Request::WriteRegisters {
        write_id, writes, ..
    } = Request::decode(&frame).unwrap()
    else {
        panic!("unexpected frame")
    };
    assert_eq!(write_id, WriteId::YM2151);
    assert_eq!(
        writes.iter().collect::<Vec<_>>(),
        vec![
            RegisterWrite::new(0x20, 0x7f),
            RegisterWrite::new(0x21, 0x11)
        ]
    );
    transport.reset_for_reuse().unwrap();
    device_stop.store(true, Ordering::Release);
    device_worker.join().unwrap();
}

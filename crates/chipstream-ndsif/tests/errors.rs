use std::error::Error;
use std::time::Duration;

use chipstream_ndsif::{NdsifSerialConfig, create_chip_factories};
use ndsif_transport::{NdsifSerialTransport, NdsifTransportError};

fn config(port: &str) -> NdsifSerialConfig {
    NdsifSerialConfig {
        port: port.to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(100),
        reset_device: false,
    }
}

#[test]
fn factory_validates_all_configs_before_opening_any_device() {
    let first = config("/nonexistent/chipstream-ndsif-test");
    let mut invalid = config("/another-port");
    invalid.baud_rate = 0;
    assert!(matches!(
        create_chip_factories(&[first.clone(), invalid]),
        Err(NdsifTransportError::InvalidBaudRate { port }) if port == "/another-port"
    ));
    assert!(matches!(
        create_chip_factories(&[first.clone(), first]),
        Err(NdsifTransportError::DuplicatePort { port })
            if port == "/nonexistent/chipstream-ndsif-test"
    ));
    assert!(matches!(
        create_chip_factories(&[config(" ")]),
        Err(NdsifTransportError::EmptyPort)
    ));
    let mut invalid = config("/another-port");
    invalid.response_timeout = Duration::ZERO;
    assert!(matches!(
        create_chip_factories(&[invalid]),
        Err(NdsifTransportError::InvalidTimeout { port }) if port == "/another-port"
    ));
    assert!(create_chip_factories(&[]).is_ok());
}

#[test]
fn direct_open_validates_and_preserves_serial_error_source() {
    let mut invalid = config("");
    assert!(matches!(
        NdsifSerialTransport::open(invalid.clone()),
        Err(NdsifTransportError::EmptyPort)
    ));
    invalid = config("port");
    invalid.baud_rate = 0;
    assert!(matches!(
        NdsifSerialTransport::open(invalid.clone()),
        Err(NdsifTransportError::InvalidBaudRate { port }) if port == "port"
    ));
    invalid.baud_rate = 115_200;
    invalid.response_timeout = Duration::ZERO;
    assert!(matches!(
        NdsifSerialTransport::open(invalid),
        Err(NdsifTransportError::InvalidTimeout { port }) if port == "port"
    ));
    let error = match NdsifSerialTransport::open(config("\0")) {
        Ok(_) => panic!("a port containing a NUL cannot be opened"),
        Err(error) => error,
    };
    assert!(
        matches!(&error, NdsifTransportError::Device { port, source }
        if port == "\0" && matches!(source.as_ref(), NdsifTransportError::Serial { .. }))
    );
    let serial = error.source().unwrap();
    assert!(serial.downcast_ref::<NdsifTransportError>().is_some());
    assert!(
        serial
            .source()
            .unwrap()
            .downcast_ref::<serialport::Error>()
            .is_some()
    );
}

#[cfg(unix)]
fn respond_to_ping<F>(response: F) -> NdsifTransportError
where
    F: FnOnce(&ndsif::Frame) -> Vec<u8> + Send + 'static,
{
    use serialport::SerialPort;
    use std::io::{Read, Write};

    let (mut device, port) = serialport::TTYPort::pair().unwrap();
    let path = port.name().unwrap();
    device.set_timeout(Duration::from_secs(2)).unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = ndsif::Decoder::new();
        let mut bytes = [0; 256];
        loop {
            let count = device.read(&mut bytes).unwrap();
            let mut request = None;
            decoder.push(&bytes[..count], |frame| request = Some(frame.unwrap()));
            if let Some(request) = request {
                device.write_all(&response(&request)).unwrap();
                wait.recv().unwrap();
                break;
            }
        }
    });
    let mut settings = config(&path);
    settings.response_timeout = Duration::from_secs(2);
    let result = NdsifSerialTransport::open(settings);
    release.send(()).unwrap();
    worker.join().unwrap();
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("device rejection must fail initialization"),
    };
    assert!(matches!(&error, NdsifTransportError::Device { port, .. } if port == &path));
    error
}

#[cfg(unix)]
#[test]
fn initialization_reports_device_rejection_with_port_and_command() {
    use ndsif::{Reply, Response, Status};

    let error = respond_to_ping(|request| {
        Response::for_request(request, Reply::Rejected)
            .unwrap()
            .to_frame()
            .unwrap()
            .encode()
            .as_bytes()
            .to_vec()
    });
    assert!(
        matches!(&error, NdsifTransportError::Device { source, .. }
        if matches!(source.as_ref(), NdsifTransportError::Rejected {
            command, status: Status::Rejected
        } if *command == ndsif::Command::Ping(b"chipstream").to_frame(0).unwrap().command_id())),
        "unexpected error: {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn initialization_preserves_protocol_error_source() {
    use ndsif::{Reply, Response};

    let error = respond_to_ping(|request| {
        let mut response = Response::for_request(request, Reply::Ping(b"chipstream")).unwrap();
        response.transaction_id = response.transaction_id.wrapping_add(1);
        response.to_frame().unwrap().encode().as_bytes().to_vec()
    });
    let device_error = error.source().unwrap();
    assert!(matches!(
        device_error.downcast_ref::<NdsifTransportError>(),
        Some(NdsifTransportError::Protocol {
            source: ndsif::Error::ResponseMismatch,
            ..
        })
    ));
    assert_eq!(
        device_error
            .source()
            .unwrap()
            .downcast_ref::<ndsif::Error>(),
        Some(&ndsif::Error::ResponseMismatch)
    );
}

#[cfg(unix)]
#[test]
fn initialization_reports_timeout_with_command_and_transaction() {
    use serialport::SerialPort;

    let (_device, port) = serialport::TTYPort::pair().unwrap();
    let path = port.name().unwrap();
    let mut settings = config(&path);
    settings.response_timeout = Duration::from_millis(30);
    let error = match NdsifSerialTransport::open(settings) {
        Err(error) => error,
        Ok(_) => panic!("an unresponsive device must time out"),
    };
    assert!(
        matches!(&error, NdsifTransportError::Device { port, source }
        if port == &path && matches!(source.as_ref(), NdsifTransportError::Timeout {
            command, transaction_id: 0
        } if *command == ndsif::Command::Ping(b"chipstream").to_frame(0).unwrap().command_id())),
        "unexpected error: {error:?}"
    );
}

#[cfg(unix)]
#[test]
fn fixed_clock_routing_and_connection_reuse_work_through_serial_transport() {
    use chipstream::sequencer::{Chip, Instance, VgmSequencer};
    use chipstream_ndsif::{NdsifRoute, create_shared_routed_chip_factories};
    use ndsif::{Capabilities, Decoder, Reply, Request, Response};
    use serialport::SerialPort;
    use soundlog::{
        chip::Ym2413Spec,
        vgm::{VgmBuilder, command::WaitSamples},
    };
    use std::{
        cell::RefCell,
        io::{Read, Write},
        rc::Rc,
    };

    let (mut fake, port) = serialport::TTYPort::pair().unwrap();
    fake.set_timeout(Duration::from_secs(3)).unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let (writes, received) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let mut decoder = Decoder::new();
        loop {
            let mut frame = None;
            while frame.is_none() {
                let mut byte = [0];
                fake.read_exact(&mut byte).unwrap();
                decoder.push(&byte, |result| frame = Some(result.unwrap()));
            }
            let frame = frame.unwrap();
            let mut entry = [0x51, 0, 0, 0, 1, 0, 0, 0, 0];
            entry[5..].copy_from_slice(&3_579_540_u32.to_le_bytes());
            let (reply, stop) = match Request::decode(&frame).unwrap() {
                Request::Ping(data) => (Some(Reply::Ping(data)), false),
                Request::GetCapabilities { offset: 0 } => (
                    Some(Reply::Capabilities(
                        Capabilities::new(256, 0, 0, 0, u16::MAX, &entry).unwrap(),
                    )),
                    false,
                ),
                Request::WriteRegisters {
                    writes: registers, ..
                } => {
                    writes.send(registers.as_bytes().to_vec()).unwrap();
                    (None, false)
                }
                Request::Reset => (Some(Reply::Complete), true),
                request => panic!("unexpected NDSIF request: {request:?}"),
            };
            if let Some(reply) = reply {
                let response = Response::for_request(&frame, reply)
                    .unwrap()
                    .to_frame()
                    .unwrap()
                    .encode();
                fake.write_all(response.as_bytes()).unwrap();
            }
            if stop {
                break;
            }
        }
        wait.recv().unwrap();
    });
    let mut settings = config(&port.name().unwrap());
    settings.response_timeout = Duration::from_secs(2);
    let transport = NdsifSerialTransport::open(settings).unwrap();
    let shared = Rc::new(RefCell::new(transport));
    let route = NdsifRoute {
        chip: Chip::Ym2413,
        source: Instance::Primary,
        device: 0,
        target: Instance::Primary,
    };
    for (clock, accepted) in [(3_579_576, false), (3_579_545, true)] {
        let factories = create_shared_routed_chip_factories(
            std::slice::from_ref(&shared),
            std::slice::from_ref(&route),
        )
        .unwrap();
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym2413, Instance::Primary, clock);
        builder.add_chip_write(
            Instance::Primary,
            Ym2413Spec {
                register: 0x20,
                value: 0xc7,
            },
        );
        builder.add_vgm_command(WaitSamples(441));
        let result = VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories);
        if accepted {
            let mut player = result.unwrap();
            while player.render().unwrap().is_some() {}
        } else {
            assert!(result.is_err());
        }
    }
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        [0x20, 0xc7]
    );
    let mut transport = Rc::try_unwrap(shared).ok().unwrap().into_inner();
    transport.reset_for_reuse().unwrap();
    release.send(()).unwrap();
    worker.join().unwrap();
}

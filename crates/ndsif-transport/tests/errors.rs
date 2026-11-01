use std::error::Error;
use std::time::Duration;

use ndsif_transport::{
    NdsifSerialConfig, NdsifSerialTransport, NdsifTransportError, create_chip_factories,
};

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
    assert!(matches!(
        NdsifSerialTransport::open("", 115_200, Duration::from_secs(1), false),
        Err(NdsifTransportError::EmptyPort)
    ));
    assert!(matches!(
        NdsifSerialTransport::open("port", 0, Duration::from_secs(1), false),
        Err(NdsifTransportError::InvalidBaudRate { port }) if port == "port"
    ));
    assert!(matches!(
        NdsifSerialTransport::open("port", 115_200, Duration::ZERO, false),
        Err(NdsifTransportError::InvalidTimeout { port }) if port == "port"
    ));
    let error = match NdsifSerialTransport::open("\0", 115_200, Duration::from_millis(100), false) {
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
    let result = NdsifSerialTransport::open(&path, 115_200, Duration::from_secs(2), false);
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
    let error = match NdsifSerialTransport::open(&path, 115_200, Duration::from_millis(30), false) {
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

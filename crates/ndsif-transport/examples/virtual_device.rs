//! Exercise NdsifSerialTransport against a small software NDSIF device.

#[cfg(unix)]
/// Capabilities advertised by the virtual device: one variable-clock YM2151.
static CAPABILITY_ENTRIES: [u8; 9] = [0x54, 0, 0, 0x04, 0x02, 0, 0, 0, 0];

#[cfg(unix)]
/// Emulate the device endpoint by decoding host requests and returning replies.
fn run_virtual_device(
    mut virtual_device: serialport::TTYPort,
    writes_tx: std::sync::mpsc::Sender<Vec<ndsif::RegisterWrite>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use std::io::{Read, Write};

    use ndsif::{Capabilities, Decoder, Reply, Request, Response, WriteId};

    let mut decoder = Decoder::new();
    loop {
        // Read a serial byte and wait until the incremental decoder completes a frame.
        let mut byte = [0];
        virtual_device.read_exact(&mut byte)?;
        let mut decoded = None;
        decoder.push(&byte, |result| decoded = Some(result));
        let Some(frame) = decoded else {
            continue;
        };
        let frame = frame?;
        let request = Request::decode(&frame)?;
        // Handle the initialization requests and the single register write used by this example.
        let reply =
            match request {
                Request::Ping(payload) => Some(Reply::Ping(payload)),
                Request::GetInfo => Some(Reply::Info(ndsif::DeviceInfo {
                    model: "Virtual NanoDrive",
                    firmware: "example",
                })),
                Request::GetCapabilities { offset: 0 } => Some(Reply::Capabilities(
                    Capabilities::new(256, 0, 0, 0, u16::MAX, &CAPABILITY_ENTRIES)?,
                )),
                Request::SetChipClock { .. } => Some(Reply::Complete),
                Request::WriteRegisters {
                    write_id,
                    instance,
                    page,
                    writes,
                } => {
                    if write_id != WriteId::YM2151 || instance != 0 || page != 0 {
                        return Err("unexpected register target".into());
                    }
                    writes_tx.send(writes.iter().collect::<Vec<_>>())?;
                    break;
                }
                _ => return Err("unexpected request from transport".into()),
            };

        // Send the matching protocol response for commands that expect one.
        if let Some(reply) = reply {
            let response = Response::for_request(&frame, reply)?.to_frame()?.encode();
            virtual_device.write_all(response.as_bytes())?;
        }
    }
    Ok(())
}

#[cfg(unix)]
/// Connect through the PTY and exercise the real serial transport API.
fn run_host(
    port_name: String,
    writes_rx: std::sync::mpsc::Receiver<Vec<ndsif::RegisterWrite>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    use ndsif::{RegisterWrite, WriteId};
    use ndsif_transport::{
        NdsifRegisterEvent, NdsifSerialConfig, NdsifSerialTransport, NdsifTransport,
    };

    // Open the host-side PTY with the same transport used for a physical device.
    let mut transport = NdsifSerialTransport::open(NdsifSerialConfig {
        port: port_name,
        baud_rate: 115_200,
        response_timeout: Duration::from_secs(2),
        reset_device: false,
    })?;
    let info = transport.device_info()?;
    println!("Connected to {} ({})", info.model, info.firmware);
    assert!(transport.supports_clock(WriteId::YM2151, 0, 3_579_545));
    transport.configure_chip(WriteId::YM2151, 0, 3_579_545)?;
    // Queue a timed register write and flush it through frame 1.
    transport.schedule_register_write(NdsifRegisterEvent {
        write_id: WriteId::YM2151,
        instance: 0,
        page: 0,
        write: RegisterWrite::new(0x20, 0xc7),
        sample_frame: 0,
        sample_rate: 44_100,
    })?;
    transport.flush_scheduled_until(1, 44_100)?;

    let writes = writes_rx.recv_timeout(Duration::from_secs(2))?;
    println!(
        "Virtual device received {} register write(s): {writes:?}",
        writes.len()
    );
    Ok(())
}

#[cfg(unix)]
/// Create the PTY pair, run both endpoints, and collect the virtual device result.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{sync::mpsc, thread, time::Duration};

    use serialport::{SerialPort, TTYPort};

    // One PTY endpoint acts as the host serial port; the other is the virtual device.
    let (mut virtual_device, host_port) = TTYPort::pair()?;
    let port_name = host_port
        .name()
        .ok_or("virtual serial pair did not provide a port name")?;
    virtual_device.set_timeout(Duration::from_secs(2))?;

    let (writes_tx, writes_rx) = mpsc::channel();
    let device_thread = thread::spawn(move || run_virtual_device(virtual_device, writes_tx));
    run_host(port_name, writes_rx)?;
    device_thread
        .join()
        .map_err(|_| std::io::Error::other("virtual device thread panicked"))?
        .map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(not(unix))]
/// Explain why the PTY-based example cannot run on this platform.
fn main() {
    eprintln!("This virtual serial example requires a Unix platform.");
}

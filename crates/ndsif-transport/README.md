# ndsif-transport

Serial transport and `chipstream` backend integration for NanoDrive NDSIF
devices.

This crate owns serial communication, capability discovery, event scheduling,
and selection of NDSIF-backed chip factories. Pass all connected NDSIF devices
in preference order; each chip instance uses the first device advertising a
compatible chip and clock. Unsupported configurations retain their emulated
backend.

All device configurations are validated before any port is opened or reset.
`create_chip_factories` and `NdsifSerialTransport::open` return
`NdsifTransportError`, which distinguishes invalid configuration, serial I/O,
protocol errors, timeouts, rejected requests, and non-idle streams. Errors
encountered during device initialization include the port and retain their
underlying sources through `std::error::Error::source`.

The public `chipstream::chip::NdsifTransport` trait currently reports playback
errors as strings; errors crossing that trait boundary are formatted into its
existing error type. The typed initialization errors do not change this
playback API.

```rust,no_run
use std::time::Duration;

let devices = [
    ndsif_transport::NdsifSerialConfig {
        port: "/dev/ttyACM0".to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(500),
        reset_device: true,
    },
    ndsif_transport::NdsifSerialConfig {
        port: "/dev/ttyACM1".to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(500),
        reset_device: true,
    },
];
let factories = ndsif_transport::create_chip_factories(&devices)?;
# let _ = factories;
# Ok::<(), Box<dyn std::error::Error>>(())
```

For example, callers can distinguish a bad setting from a device failure:

```rust,no_run
use ndsif_transport::{NdsifSerialConfig, NdsifTransportError, create_chip_factories};

fn connect(devices: &[NdsifSerialConfig]) -> Result<(), NdsifTransportError> {
    match create_chip_factories(devices) {
        Ok(_factories) => Ok(()),
        Err(NdsifTransportError::InvalidBaudRate { port }) => {
            eprintln!("Configure a nonzero baud rate for {port}");
            Err(NdsifTransportError::InvalidBaudRate { port })
        }
        Err(error) => Err(error),
    }
}
```

The crate has no dependency on GStreamer or a particular audio output backend.
The lower-level `ndsif` crate remains responsible for the I/O-independent
protocol codec and stream state. Chip routing is based on the capabilities each
firmware advertises, rather than a hard-coded model name. The current NanoDrive
6 firmware advertises YM2612 and two SN76489 targets; NanoDrive 7 advertises
YM2203, YM2413 (when its clock is configured), YMF262, YM3526, and YM3812;
NanoDrive 8 advertises YM2151 and an OKIM6258 stream. Only chip types already
supported by `chipstream` are routed.

When a device advertises revision 40/profile 1, YM2612 playback uses its timed
DAC stream: `$2A` and `$2B` writes become ordered DAC records, while FM and PSG
writes are submitted as positioned events. The transport queries capacities,
prebuffers 80 ms, and seals/drains the profile at end of playback. Devices that
do not advertise the profile keep using regular scheduled register writes.

## License

BSD-3-Clause License

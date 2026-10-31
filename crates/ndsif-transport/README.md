# ndsif-transport

Serial transport and `chipstream` backend integration for NanoDrive NDSIF
devices.

This crate owns serial communication, capability discovery, event scheduling,
and selection of NDSIF-backed chip factories. Pass all connected NDSIF devices
in preference order; each chip instance uses the first device advertising a
compatible chip and clock. Unsupported configurations retain their emulated
backend.

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

The crate has no dependency on GStreamer or a particular audio output backend.
The lower-level `ndsif` crate remains responsible for the I/O-independent
protocol codec and stream state. Chip routing is based on the capabilities each
firmware advertises, rather than a hard-coded model name. The current NanoDrive
6 firmware advertises YM2612 and two SN76489 targets; NanoDrive 7 advertises
YM2203, YM2413 (when its clock is configured), YMF262, YM3526, and YM3812;
NanoDrive 8 advertises YM2151 and an OKIM6258 stream. Only chip types already
supported by `chipstream` are routed.

NanoDrive 6's optional revision-40 timed YM2612 DAC profile is not implemented
by the current `ndsif` codec; YM2612 register writes use the regular scheduled
register path.

## License

BSD-3-Clause License

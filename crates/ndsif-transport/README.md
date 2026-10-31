# ndsif-transport

Serial transport and `chipstream` backend integration for NDSIF devices.

This crate owns serial communication, capability discovery, event scheduling,
and selection of NDSIF-backed YM2151/OKIM6258 factories. Frontends supply their
serial settings and pass the returned factory registry to a sequencer:

```rust,no_run
use std::time::Duration;

let factories = ndsif_transport::create_chip_factories(
    &ndsif_transport::NdsifSerialConfig {
        port: "/dev/ttyACM0".to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(500),
        reset_device: true,
    },
)?;
# let _ = factories;
# Ok::<(), Box<dyn std::error::Error>>(())
```

The crate has no dependency on GStreamer or a particular audio output backend.
The lower-level `ndsif` crate remains responsible for the I/O-independent
protocol codec and stream state.

## License

BSD-3-Clause License

# ndsif-transport

Serial transport and `chipstream` backend integration for NanoDrive NDSIF
devices. This crate opens devices, discovers their capabilities, and schedules
chip writes. The lower-level `ndsif` crate provides the I/O-independent
protocol codec and stream state.

Hardware-backed chips produce **silence in the host PCM buffer**. Their sound
comes from the physical device and must be played or captured separately.
Emulated chips continue to produce host PCM normally.

## Public API

| Item | Purpose |
| --- | --- |
| `NdsifSerialConfig` | Serial port and initialization options for one device. |
| `create_chip_factories` | Open devices and create a `chipstream` factory registry. |
| `NdsifSerialTransport::open` | Open and initialize one transport directly. |
| `NdsifSerialTransport::supports_clock` | Check cached chip and clock capabilities. |
| `NdsifSerialTransport::supports_audio_stream` | Check cached OKIM6258 stream support. |
| `NdsifTransportError` | Typed configuration and initialization errors. |

`NdsifSerialTransport` also implements `chipstream::chip::NdsifTransport` for
lower-level scheduling. Most players should use `create_chip_factories` and
let the sequencer manage scheduling.

## Configuration and device selection

`NdsifSerialConfig` has no implicit defaults; supply all fields:

| Field | Meaning and constraints |
| --- | --- |
| `port: String` | Serial device path; empty or whitespace-only paths are rejected. |
| `baud_rate: u32` | Serial baud rate; must be nonzero. |
| `response_timeout: Duration` | Timeout for a request/response transaction; must be nonzero. |
| `reset_device: bool` | Reset before capability discovery when `true`. |

For example, `/dev/ttyACM0`, `115_200`, and 500 ms are illustrative values;
use the port and serial settings appropriate to the device.

`create_chip_factories` validates **all** configurations, including duplicate
port strings, before opening any device. An empty slice returns an empty
registry. Otherwise, devices are opened in the supplied order. For each
supported chip instance, the first device with a compatible capability and
clock is selected when the sequencer constructs that chip. If no device
matches, the normal emulated backend is used. A failure to open or initialize
any configured device is returned as an error rather than falling back.

Initialization clears pending serial input, optionally resets the device,
sends a ping, discovers capabilities, and verifies applicable audio and timed
DAC streams are idle. With `reset_device: false`, an active or faulted stream
can cause a `DeviceBusy` initialization error. Opening multiple devices is not
transactional: earlier devices may already have been opened or reset when a
later device fails.

Variable-clock capabilities are accepted; fixed clocks must match exactly, and
unknown clock modes do not match. `supports_clock` only checks cached
capabilities: it performs no I/O and does not configure the clock.
`supports_audio_stream` checks the primary OKIM6258 instance, nonzero advertised
capacities, and a supported OKIM6258 clock. It does **not** check the
advertised chip clock mode; also call `supports_clock` when checking a clock.

### Multiple devices

```rust,no_run
use std::time::Duration;
use ndsif_transport::{NdsifSerialConfig, create_chip_factories};

let devices = [
    NdsifSerialConfig {
        port: "/dev/ttyACM0".to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(500),
        reset_device: true,
    },
    NdsifSerialConfig {
        port: "/dev/ttyACM1".to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(500),
        reset_device: true,
    },
];
let factories = create_chip_factories(&devices)?;
# let _ = factories;
# Ok::<(), Box<dyn std::error::Error>>(())
```

### Software-only registry

An empty device list performs no serial I/O:

```rust
use ndsif_transport::create_chip_factories;

let factories = create_chip_factories(&[])?;
# let _ = factories;
# Ok::<(), ndsif_transport::NdsifTransportError>(())
```

## Playing with chipstream

Pass the registry to a sequencer. These examples use the `chipstream` crate
directly; the MDX example also requires `soundlog` as a direct dependency.
The host application remains responsible for sending the returned stereo
frames to an audio output. Configure a suitable loop limit if playback should
terminate; continuously looping input can otherwise keep rendering.

### VGM

`from_vgm_with_factories` accepts **uncompressed VGM** bytes. Decompress VGZ
before calling it.

```rust,no_run
use std::time::Duration;
use chipstream::sequencer::VgmSequencer;
use ndsif_transport::{NdsifSerialConfig, create_chip_factories};

fn play_vgm(vgm: Vec<u8>, port: &str) -> Result<(), Box<dyn std::error::Error>> {
    let devices = [NdsifSerialConfig {
        port: port.to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(500),
        reset_device: true,
    }];
    let factories = create_chip_factories(&devices)?;
    let mut player = VgmSequencer::from_vgm_with_factories(vgm, 48_000, 512, factories)?;

    while let Some(block) = player.render()? {
        // Send block.frames() to the application's stereo audio output.
        let _frames = block.frames();
    }
    Ok(())
}
```

### MDX with optional PDX

```rust,no_run
use std::time::Duration;
use chipstream::sequencer::{MdxSequencer, MdxSequencerOptions};
use ndsif_transport::{NdsifSerialConfig, create_chip_factories};
use soundlog::mdx::package::MdxPackage;

fn play_mdx(
    mdx: Vec<u8>,
    pdx: Option<Vec<u8>>,
    port: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let package = MdxPackage::parse_owned(mdx, pdx)?;
    let devices = [NdsifSerialConfig {
        port: port.to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(500),
        reset_device: true,
    }];
    let factories = create_chip_factories(&devices)?;
    let options = MdxSequencerOptions {
        loop_count: Some(1),
        ..MdxSequencerOptions::default()
    };
    let mut player =
        MdxSequencer::with_options_with_factories(package, 48_000, 512, options, factories)?;

    while let Some(block) = player.render()? {
        // Send block.frames() to the application's stereo audio output.
        let _frames = block.frames();
    }
    Ok(())
}
```

## Direct transport access

Use the direct API to inspect a device without creating a registry. This
example requires `ndsif` as a direct dependency for `WriteId`:

```rust,no_run
use std::time::Duration;
use ndsif::WriteId;
use ndsif_transport::NdsifSerialTransport;

let device = NdsifSerialTransport::open(
    "/dev/ttyACM0",
    115_200,
    Duration::from_millis(500),
    false,
)?;
let clock = 4_000_000;
let supports_oki = device.supports_clock(WriteId::OKIM6258, 0, clock)
    && device.supports_audio_stream(0, clock);
println!("primary OKIM6258 stream supported: {supports_oki}");
# Ok::<(), ndsif_transport::NdsifTransportError>(())
```

The transport's `NdsifTransport` methods queue events at absolute host
output-frame positions. Callers using this lower-level trait are responsible
for configuring the chip, flushing scheduled events, and finishing streams.
Events from different chips can be queued out of order. Playback operations
through this trait report errors as `String`.

## Playback behavior and limitations

- Factory routing covers YM2413, YM2151, YM2203, YM2608, YM2610B, YM2612,
  YM3526, Y8950, YM3812, YMF262, YMF278B, SN76489, and OKIM6258. Actual
  hardware selection depends on the device's advertised capabilities.
- Game Gear stereo SN76489 uses emulation; T6W28 PSG is rejected. When a VGM
  pairs YM2151 with OKIM6258, hardware selection for YM2151 also requires a
  compatible primary OKIM6258 stream on that device.
- Only one primary OKIM6258 audio stream is supported per transport. It cannot
  share a transport with the timed DAC profile. Raw PSG writes cannot share an
  OKIM6258 audio stream.
- A device advertising the supported revision 40, profile 1 timed DAC
  capabilities uses that profile for primary YM2612. YM2612 `$2A` and `$2B`
  writes become DAC records; other supported FM and PSG writes become
  positioned events. The transport targets 80 ms of prebuffering and
  seals/drains the profile at end of playback. Without this profile, normal
  scheduled writes are used.
- Without an audio or timed DAC stream, the transport paces configured chip
  writes against a host wall clock. Playback operations may block while pacing,
  waiting for queue capacity, communicating with the device, or draining.
- Dropping a transport after chip configuration attempts a device reset;
  reset failures are reported to standard error. `reset_device: false` only
  disables the **initial** reset.

Capabilities, firmware, physical audio routing, and serial configuration vary
by device. For reference, current NanoDrive 6 firmware advertises YM2612 and
two SN76489 targets; NanoDrive 7 advertises YM2203, YM2413 when its clock is
configured, YMF262, YM3526, and YM3812; NanoDrive 8 advertises YM2151 and an
OKIM6258 stream. Check the actual device rather than relying on model names.

## Errors

`create_chip_factories` and `NdsifSerialTransport::open` return
`NdsifTransportError`. Configuration errors include `EmptyPort`,
`DuplicatePort`, `InvalidBaudRate`, and `InvalidTimeout`. Initialization
failures are wrapped in `Device { port, source }`; the source may identify a
serial or I/O error, a protocol error, timeout, rejection, unexpected reply,
invalid capability pagination, unexpected DAC profile, or non-idle stream.
Underlying serial, I/O, and codec errors remain available through
`std::error::Error::source`.

```rust,no_run
use ndsif_transport::{NdsifSerialConfig, NdsifTransportError, create_chip_factories};

fn connect(devices: &[NdsifSerialConfig]) -> Result<(), NdsifTransportError> {
    match create_chip_factories(devices) {
        Ok(_factories) => Ok(()),
        Err(NdsifTransportError::InvalidBaudRate { port }) => {
            eprintln!("Configure a nonzero baud rate for {port}");
            Err(NdsifTransportError::InvalidBaudRate { port })
        }
        Err(error @ NdsifTransportError::Device { .. }) => {
            eprintln!("Device initialization failed: {error}");
            Err(error)
        }
        Err(error) => Err(error),
    }
}
```

Errors during sequencer playback cross the `NdsifTransport` trait's existing
`String` error boundary; they are not returned as `NdsifTransportError`.

## Internal organization and tests

The private `factory`, `session`, and `transport` modules handle device
selection, serial exchange, and scheduling respectively. Audio and timed-DAC
operations are organized under `transport`. Transport unit tests live in
`tests/unit/transport.rs` and are included as a test-only module to exercise
private scheduling and stream state.

## License

BSD-3-Clause License

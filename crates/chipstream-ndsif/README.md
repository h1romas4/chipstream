# chipstream-ndsif

NDSIF hardware integration for `chipstream`. Use this crate to discover device
capabilities and route supported chip instances to hardware. Serial I/O and
scheduled transport types are provided by `ndsif-transport`.

Hardware-backed chips produce **silence in the host PCM buffer**. Their audio
comes from the physical device and must be played or captured separately.
Emulated chips continue to produce host PCM.

## Choose an API

| Use case | API |
| --- | --- |
| Typical playback with automatic device selection | `NdsifSerialConfig`, `create_chip_factories` |
| Select a device and target for each chip instance | `NdsifRoute`, `create_routed_chip_factories` |
| Keep device connections open across playback sessions | `create_shared_routed_chip_factories`, `NdsifSerialTransport::reset_for_reuse` |
| Use a custom scheduled transport or record chip writes | `NdsifRoutingTransport`, `create_routed_transport_factories`, `NdsifRecordingTransport` |
| Schedule multiple devices on a host audio timeline | `NdsifWorkerDevice`, `NdsifHardwareWorkers`, `NdsifHostTimeline` |
| Inspect or control a device directly | `NdsifSerialTransport::open` |

For NanoDrive 7 devices, pass `NdsifNd7Policy` to either
`create_routed_transport_factories_with_policy` or
`create_automatic_transport_factories_with_policy`. The policy validates
shared chip and clock resources and permits YM2149 on AY targets as an
approximation using YM2203 SSG. Factories without this policy reject YM2149.
Custom `NdsifRoutePolicy` implementations can validate selected routes and
specify AY compatibility. A rejected hardware route is an error, not a
software fallback.

```rust,no_run
use std::{cell::RefCell, rc::Rc};
use chipstream::sequencer::ChipFactoryRegistry;
use chipstream_ndsif::{
  NdsifNd7Policy, NdsifRoute, NdsifRoutingError, NdsifRoutingTransport,
    create_automatic_transport_factories_with_policy,
  create_routed_transport_factories_with_policy,
};

fn factories<T: NdsifRoutingTransport + 'static>(
    transports: &[Rc<RefCell<T>>],
    supports_ay: bool,
    nd7_devices: &[usize],
) -> Result<ChipFactoryRegistry, NdsifRoutingError> {
    let policy = NdsifNd7Policy::new(nd7_devices.iter().copied(), transports.len())?;
    Ok(create_automatic_transport_factories_with_policy(transports, supports_ay, policy))
}

  fn routed_factories<T: NdsifRoutingTransport + 'static>(
    transports: &[Rc<RefCell<T>>],
    routes: &[NdsifRoute],
    nd7_devices: &[usize],
  ) -> Result<ChipFactoryRegistry, NdsifRoutingError> {
    let policy = NdsifNd7Policy::new(nd7_devices.iter().copied(), transports.len())?;
    create_routed_transport_factories_with_policy(transports, routes, policy)
  }
```

Create a new policy for each playback: it retains selected routes to detect
shared-resource conflicts. The supplied device count must match the transport
list used by the factory. Validation occurs while each active chip is created,
so earlier chips may already be configured when a later route is rejected.

Most applications should start with `create_chip_factories`. It selects the
first connected device that advertises a compatible chip and clock; unmatched
chips use chipstream's normal emulation. Errors opening or initializing a
configured device are returned rather than silently ignored.

## Connect devices and play

`NdsifSerialConfig` requires all connection and initialization settings:

| Field | Description |
| --- | --- |
| `port` | Serial device path |
| `baud_rate` | Nonzero serial baud rate |
| `response_timeout` | Nonzero request/response timeout |
| `reset_device` | Reset the device before discovering capabilities |

An empty device list performs no serial I/O and creates a software-only
registry. For example, to play an uncompressed VGM file:

```rust,no_run
use std::time::Duration;
use chipstream::sequencer::VgmSequencer;
use chipstream_ndsif::{NdsifSerialConfig, create_chip_factories};

let vgm = std::fs::read("music.vgm")?;
let devices = [NdsifSerialConfig {
    port: "/dev/ttyACM0".to_owned(),
    baud_rate: 115_200,
    response_timeout: Duration::from_millis(500),
    reset_device: true,
}];
let factories = create_chip_factories(&devices)?;
let mut player = VgmSequencer::from_vgm_with_factories(vgm, 48_000, 512, factories)?;

while let Some(block) = player.render()? {
    // Send block.frames() to the application's audio output.
}
# Ok::<(), Box<dyn std::error::Error>>(())
```

The application supplies the audio output and handles the rendered PCM.
VGZ files must be decompressed before passing their VGM data to chipstream.
For MDX playback, use `MdxPackage` with an optional PDX bank and pass the
package to `MdxSequencer`.

### Write registers without a sequencer

For direct control, open `NdsifSerialTransport`, configure a supported chip,
then queue register events through `NdsifTransport`. Scheduling a write does
not send it immediately; call `flush_scheduled_until` to send queued events
through the requested output frame. This example writes YM2151 register `0x1b`
at frame 0:

```rust,no_run
use std::time::Duration;
use ndsif::{RegisterWrite, WriteId};
use ndsif_transport::{NdsifRegisterEvent, NdsifSerialTransport, NdsifTransport};

let mut device = NdsifSerialTransport::open(
    "/dev/ttyACM0",
    115_200,
    Duration::from_millis(500),
    false,
)?;
let output_rate = 44_100;
device.configure_chip(WriteId::YM2151, 0, 3_579_545)?;
device.schedule_register_write(NdsifRegisterEvent {
    write_id: WriteId::YM2151,
    instance: 0,
    page: 0,
    write: RegisterWrite::new(0x1b, 0x00),
    sample_frame: 0,
    sample_rate: output_rate,
})?;
device.flush_scheduled_until(1, output_rate)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Add `ndsif-transport` and `ndsif` as direct dependencies to use the event and
register types. Choose a clock supported by the selected device. The caller
must provide chip-specific initialization, schedule writes on a monotonically
advancing absolute frame timeline, and manage playback and device lifetime.

## Explicit routing

Use `NdsifRoute` when a saved configuration must select an exact source and
hardware target. Each route specifies a chip type, source instance, device
index, and target instance. Unrouted instances use the built-in backend;
unsupported explicit routes return an error instead of falling back.

`create_shared_routed_chip_factories` accepts shared transport handles so the
application can retain device connections between playback sessions. Drop
the sequencer and factories before recovering a connection, then call
`reset_for_reuse` before returning it to the connection owner. Do not reset a
transport while a sequencer is using it. If reset fails, close the connection
instead of reusing it.

## Multiple synchronized devices

For applications that need to pace several devices against host audio:

1. Construct an `NdsifWorkerDevice` for each shared serial connection. Each
  device owns its matching recorder; no application-specific ID is required.
2. Obtain the recorders with `NdsifWorkerDevice::recorder`, create routed
  factories with `create_routed_transport_factories`, and construct the
  sequencer. Route indices refer to the order of the recorder list.
3. Start `NdsifHardwareWorkers` with the worker devices. The devices may be
  supplied in any order because each remains paired with its own recorder.
4. After rendering, call `enqueue` on the workers to submit all recorded
  batches. Buffer enough audio before starting the shared `NdsifHostTimeline`
  at the estimated audio presentation time.
5. Check worker errors during playback and finish after the final batch.
  Drop sequencers, factories, recorders, worker devices, and workers before
  resetting or reclaiming connections.

```rust,no_run
use std::{num::NonZeroUsize, sync::{Arc, Mutex}, time::Duration};
use chipstream::sequencer::VgmSequencer;
use chipstream_ndsif::create_automatic_transport_factories;
use ndsif_transport::{
  NdsifHardwareWorkers, NdsifHostTimeline, NdsifSerialTransport,
  NdsifWorkerDevice,
};

fn prepare(vgm: Vec<u8>) -> Result<(VgmSequencer<'static>, NdsifHardwareWorkers, Arc<NdsifHostTimeline>), Box<dyn std::error::Error>> {
  let connection = NdsifSerialTransport::open(
    "/dev/ttyACM0", 115_200, Duration::from_millis(500), true,
  )?;
  let supports_ay = connection.capabilities().iter().any(|entry| entry.write_id == 0xa0);
  let devices = [NdsifWorkerDevice::new(
    Arc::new(Mutex::new(connection)), NonZeroUsize::new(16_384).unwrap(),
  )?];
  let recorders: Vec<_> = devices.iter().map(NdsifWorkerDevice::recorder).collect();
  let factories = create_automatic_transport_factories(&recorders, supports_ay);
  let player = VgmSequencer::from_vgm_with_factories(vgm, 48_000, 512, factories)?;
  let timeline = Arc::new(NdsifHostTimeline::pending());
  let workers = NdsifHardwareWorkers::start(&devices, Arc::clone(&timeline), NonZeroUsize::new(8).unwrap())?;
  Ok((player, workers, timeline))
}
```

Use `enqueue_prebuffer` when you need bounded startup for timed DAC data.
Applications are responsible for audio output, connection management,
cancellation, and bounding any inter-thread queues. Worker playback and serial
operations may block; this crate does not put hardware audio into host PCM.

## Playback support and limitations

The transport supports YM2413, YM2151, YM2203, YM2608, YM2610B, YM2612,
YM3526, Y8950, YM3812, YMF262, YMF278B, SN76489, and OKIM6258 where the
device advertises compatible capabilities. Check the connected device rather
than relying on its model name.

- Automatic routing uses emulation when no compatible hardware target is
  available. Explicit routes fail when the selected target is incompatible.
- SN76489 hardware routing is unavailable for non-default Game Gear stereo
  masks; T6W28 PSG is not supported. A VGM stereo-capable flag alone does not
  prevent routing.
- A VGM pairing YM2151 with OKIM6258 requires a compatible primary OKIM6258
  stream on the same device for hardware YM2151 routing.
- Only one primary OKIM6258 stream is supported per transport. Supported
  clocks are 4 MHz and 8 MHz, subject to device capabilities.
- Hardware writes are best-effort, not sample-accurate. Device latency,
  serial bandwidth, scheduling, and clock drift can affect timing. Hardware
  audio must be mixed externally with any host audio.

## Errors and device reuse

`create_chip_factories` and `NdsifSerialTransport::open` return
`NdsifTransportError`. Invalid ports, baud rates, timeouts, and duplicate
ports are reported as configuration errors; serial, I/O, protocol, and device
initialization failures are reported with their source error. Opening multiple
devices is not transactional: earlier devices may already have been opened
or reset if a later device fails.

Use `NdsifRoutingError` for explicit route validation failures.
`NdsifSerialTransport::supports_clock` and `supports_audio_stream` check
capabilities discovered when the device was opened; they do not perform I/O
or configure the device. Playback errors crossing the
`NdsifTransport` interface are reported as `String`.

## License

BSD-3-Clause License

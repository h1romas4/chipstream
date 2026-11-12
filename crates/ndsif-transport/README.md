# ndsif-transport

NDSIF is a retro sound-chip playback interface developed by the NanoDrive
Project. This crate provides serial transport and scheduled playback for
NanoDrive devices that support NDSIF.

If you provide your own transport or only need the protocol codec, use
[`ndsif`](../ndsif/README.md) instead.

Use `ndsif-transport` directly when you need serial transport, custom
scheduling, or device workers.

## Supported Devices

| Device | Repository |
|---|---|
| NanoDrive8 | [Fujix1/NanoDrive8](https://github.com/Fujix1/NanoDrive8) |
| NanoDrive7 | [Fujix1/NanoDrive7](https://github.com/Fujix1/NanoDrive7) |
| NanoDrive6 | [Fujix1/NanoDrive6](https://github.com/Fujix1/NanoDrive6) |

## Device information and reset

Open the serial transport, request the device model and firmware with
`device_info()`, then lower the output volume before resetting it.

Stop playback before resetting. On devices with output-volume control,
`reset_for_reuse()` restores normal volume (0 dB attenuation) after reset;
lower the gain first to reduce the pop.

```rust,no_run
use std::sync::Arc;
use std::time::Duration;
use ndsif_transport::{NdsifHostTimeline, NdsifOutputGain, NdsifSerialTransport};

let mut device = NdsifSerialTransport::open(
    "/dev/ttyACM0", 115_200, Duration::from_millis(500), false,
)?;
let info = device.device_info()?;
println!("{} ({})", info.model, info.firmware);
let gain = Arc::new(NdsifOutputGain::new());
gain.set(0.25)?;
let timeline = Arc::new(NdsifHostTimeline::pending());
timeline.set_output_gain(gain)?;
if device.set_output_fade_timeline(timeline)? {
    // Lower the device output before resetting.
    device.initialize_output_volume()?;
}
// Sends RESET, then restores modified output volume to normal.
device.reset_for_reuse()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## Direct Control

Open `NdsifSerialTransport`, configure a supported chip, and queue events
through `NdsifTransport`. Scheduling a write does not send it immediately;
`flush_scheduled_until` commits queued events through the requested output
frame. The caller must provide chip initialization and advance the absolute
frame timeline monotonically.

```rust,no_run
use std::time::Duration;
use ndsif::{RegisterWrite, WriteId};
use ndsif_transport::{NdsifRegisterEvent, NdsifSerialTransport, NdsifTransport};

let mut device = NdsifSerialTransport::open(
    "/dev/ttyACM0", 115_200, Duration::from_millis(500), false,
)?;
device.configure_chip(WriteId::YM2151, 0, 3_579_545)?;
device.schedule_register_write(NdsifRegisterEvent {
    write_id: WriteId::YM2151,
    instance: 0,
    page: 0,
    write: RegisterWrite::new(0x1b, 0),
    sample_frame: 0,
    sample_rate: 44_100,
})?;
device.flush_scheduled_until(1, 44_100)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Use the device's cached capabilities and `supports_clock` or
`supports_audio_stream` to select supported settings. These methods do not
perform I/O or configure the device. Hardware audio is emitted by the device,
not into host PCM. Timing is best-effort and can be affected by latency,
serial bandwidth, scheduling, and clock drift.

## Recording and Workers

Record ADPCM data and its end event in a bounded batch, then send it through a
device worker. Replace the sample bytes and end repetition value with values
appropriate for your encoded stream.

```rust,no_run
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use ndsif::{BytePosition, Divider, ZeroPair};
use ndsif_transport::{
    NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent,
    NdsifHardwareWorkers, NdsifHostTimeline, NdsifSerialTransport, NdsifTransport,
    NdsifWorkerDevice,
};

const RATE: u32 = 44_100;
const PREBUFFER_FRAMES: usize = 4_410;
const END_FRAME: usize = 6_000;
// Share the serial connection with the worker that will perform device I/O.
let connection = Arc::new(Mutex::new(NdsifSerialTransport::open(
    "/dev/ttyACM0", 115_200, Duration::from_millis(500), false,
)?));
let device = NdsifWorkerDevice::new(Arc::clone(&connection), NonZeroUsize::new(1024).unwrap())?;
let recorder = device.recorder();
// Record stream setup before starting the worker so it can configure the device.
recorder.borrow_mut().configure_audio_stream(0, 4_000_000, Divider::Div1024, RATE)?;

// Workers replay recorded batches; the timeline coordinates when audio starts.
let timeline = Arc::new(NdsifHostTimeline::pending());
let workers = NdsifHardwareWorkers::start(
    std::slice::from_ref(&device), Arc::clone(&timeline), NonZeroUsize::new(8).unwrap(),
)?;
// Replace this placeholder with encoded OKIM6258 ADPCM bytes.
let adpcm_data = [0_u8; 256];
{
    let mut recorder = recorder.borrow_mut();
    // Positions identify bytes; END_FRAME is the frame horizon for this batch.
    for (position, value) in adpcm_data.into_iter().enumerate() {
        recorder.schedule_adpcm_data(NdsifAdpcmEvent {
            instance: 0,
            position: BytePosition::new(position as u32),
            sample_frame: END_FRAME,
            sample_rate: RATE,
            value,
        })?;
    }
    // End the stream after its final byte, then flush all recorded data.
    recorder.schedule_audio_control(NdsifAudioControlEvent {
        instance: 0,
        position: BytePosition::new(adpcm_data.len() as u32),
        sample_frame: END_FRAME,
        sample_rate: RATE,
        control: NdsifAudioControl::End(ZeroPair::Byte80),
    })?;
    recorder.finish_audio_stream(0, END_FRAME, RATE)?;
}
// Send the batch and provide a cancellation flag for shutdown or errors.
let cancelled = AtomicBool::new(false);
workers.enqueue(&cancelled)?;
// Set a future presentation time, then verify enough audio is buffered to start.
timeline.start(Instant::now() + Duration::from_millis(100))?;
workers.prepare_audio(PREBUFFER_FRAMES, RATE, &cancelled)?;
// Drain queued work and join the device thread before resetting the transport.
workers.finish()?;
drop(recorder);
drop(device);
connection.lock().map_err(|_| "device lock poisoned")?.reset_for_reuse()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

`prepare_audio` verifies the prebuffer before starting at the shared timeline.
Replay can report device-specific errors; discard a recorder after its
operation limit is exceeded.

The manager and recorders stay on the calling
thread; device I/O runs on worker threads. Start or cancel the timeline before
`finish` (dropping workers cancels it). Drop workers and transport handles
before resetting connections; close a connection if `reset_for_reuse` fails.

`NdsifSerialTransport::open` returns `NdsifTransportError` for invalid
configuration or serial, I/O, protocol, and initialization failures. Errors
crossing `NdsifTransport` are reported as `String`.

## License

BSD-3-Clause License
# ndsif-transport

Serial transport and scheduled playback for NanoDrive NDSIF devices. 

## Supported Devices

| Device | Repository |
|---|---|
| NanoDrive8 | [Fujix1/NanoDrive8](https://github.com/Fujix1/NanoDrive8) |
| NanoDrive7 | [Fujix1/NanoDrive7](https://github.com/Fujix1/NanoDrive7) |
| NanoDrive6 | [Fujix1/NanoDrive6](https://github.com/Fujix1/NanoDrive6) |

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

`NdsifRecordingTransport` records bounded batches without serial I/O. Replay
each batch in order on the matching device:

```rust,no_run
use std::{num::NonZeroUsize, time::Duration};
use ndsif::{RegisterWrite, WriteId};
use ndsif_transport::{
    NdsifRecordingTransport, NdsifRegisterEvent, NdsifSerialTransport, NdsifTransport,
};

let mut device = NdsifSerialTransport::open(
    "/dev/ttyACM0", 115_200, Duration::from_millis(500), false,
)?;
let mut recorder = NdsifRecordingTransport::new(&device, NonZeroUsize::new(1024).unwrap());
recorder.configure_chip(WriteId::YM2151, 0, 3_579_545)?;
recorder.schedule_register_write(NdsifRegisterEvent {
    write_id: WriteId::YM2151,
    instance: 0,
    page: 0,
    write: RegisterWrite::new(0x1b, 0),
    sample_frame: 0,
    sample_rate: 44_100,
})?;
recorder.flush_scheduled_until(1, 44_100)?;
recorder.take_batch()?.replay(&mut device)?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Device-specific failures can occur during replay. A recorder that exceeds its
operation limit must be discarded.

For worker-based playback, pair each shared connection with a recorder. This
example starts hardware-only playback 100 ms in the future:

```rust,no_run
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use ndsif::{RegisterWrite, WriteId};
use ndsif_transport::{
    NdsifHardwareWorkers, NdsifHostTimeline, NdsifRegisterEvent,
    NdsifSerialTransport, NdsifTransport, NdsifWorkerDevice,
};

let connection = Arc::new(Mutex::new(NdsifSerialTransport::open(
    "/dev/ttyACM0", 115_200, Duration::from_millis(500), false,
)?));
let device = NdsifWorkerDevice::new(Arc::clone(&connection), NonZeroUsize::new(1024).unwrap())?;
let recorder = device.recorder();
recorder.borrow_mut().configure_chip(WriteId::YM2151, 0, 3_579_545)?;

let timeline = Arc::new(NdsifHostTimeline::pending());
let workers = NdsifHardwareWorkers::start(
    std::slice::from_ref(&device), Arc::clone(&timeline), NonZeroUsize::new(8).unwrap(),
)?;
{
    let mut recorder = recorder.borrow_mut();
    recorder.schedule_register_write(NdsifRegisterEvent {
        write_id: WriteId::YM2151,
        instance: 0,
        page: 0,
        write: RegisterWrite::new(0x1b, 0),
        sample_frame: 0,
        sample_rate: 44_100,
    })?;
    recorder.flush_scheduled_until(1, 44_100)?;
}
let cancelled = AtomicBool::new(false);
workers.enqueue(&cancelled)?;
workers.check()?;
timeline.start(Instant::now() + Duration::from_millis(100))?;
workers.finish()?;
drop(recorder);
drop(device);
connection.lock().map_err(|_| "device lock poisoned")?.reset_for_reuse()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Each worker retains its own recorder, regardless of device order. For host
audio, buffer enough PCM and start the timeline at its estimated presentation
time. Use `enqueue_prebuffer` to bound startup for timed DAC data and call
`prepare_audio` if the initial span was not prepared automatically.

The worker manager retains its recorders on the calling thread and cannot be
sent to another thread. Device I/O runs on separate worker threads. Start or
cancel a pending timeline before calling `finish`; dropping workers cancels
the timeline. Drop the workers, recorders, and worker devices before resetting
or reclaiming the connections. Never reset a transport while it is in use.
If `reset_for_reuse` fails, close the connection instead of reusing it.

`NdsifSerialTransport::open` returns `NdsifTransportError` for invalid
configuration or serial, I/O, protocol, and initialization failures. Errors
crossing `NdsifTransport` are reported as `String`.

## License

BSD-3-Clause License
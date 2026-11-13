# ndsif-transport

NDSIF is a retro sound-chip playback interface developed by the NanoDrive
Project.

This crate provides serial transport and scheduled playback for
physical NanoDrive devices that support NDSIF. If you provide
your own transport or only need the protocol codec,
use [`ndsif`](https://crates.io/crates/ndsif) instead.

Use `ndsif-transport` directly when you need serial transport, custom
scheduling, or device workers.

## Supported Devices

| Device | Repository |
|---|---|
| NanoDrive8 | [Fujix1/NanoDrive8](https://github.com/Fujix1/NanoDrive8) |
| NanoDrive7 | [Fujix1/NanoDrive7](https://github.com/Fujix1/NanoDrive7) |
| NanoDrive6 | [Fujix1/NanoDrive6](https://github.com/Fujix1/NanoDrive6) |

## Device information and reset

Open the serial transport and request the device model and firmware with
`device_info()`.

Stop playback before resetting. On devices with output-volume control,
`reset_for_reuse()` temporarily sets maximum attenuation (silence) before
resetting, then restores normal volume (0 dB attenuation).

```rust,no_run
use std::time::Duration;
use ndsif_transport::{NdsifSerialConfig, NdsifSerialTransport};

let mut device = NdsifSerialTransport::open(NdsifSerialConfig {
    port: "/dev/ttyACM0".to_owned(),
    baud_rate: 115_200,
    response_timeout: Duration::from_millis(500),
    reset_device: false,
})?;
let info = device.device_info()?;
println!("{} ({})", info.model, info.firmware);
// Mutes capable devices, sends RESET, then restores normal volume.
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
use ndsif_transport::{
    NdsifRegisterEvent, NdsifSerialConfig, NdsifSerialTransport, NdsifTransport,
};

let mut device = NdsifSerialTransport::open(NdsifSerialConfig {
    port: "/dev/ttyACM0".to_owned(),
    baud_rate: 115_200,
    response_timeout: Duration::from_millis(500),
    reset_device: false,
})?;
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

## Synchronous and worker I/O

Direct `NdsifSerialTransport` calls perform I/O synchronously on the calling
thread. `NdsifHardwareWorkers` instead runs one I/O thread per device and
accepts recorded batches from the calling thread.

Use only one I/O model at a time for each serial connection. While a worker is
active, do not access its connection directly: synchronous calls can contend
with worker I/O and bypass batch ordering. To switch modes, stop and drop the
workers and their device handles first; call `reset_for_reuse()` only after
playback has stopped.

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
use ndsif::{BytePosition, Divider, RegisterWrite, WriteId, ZeroPair};
use ndsif_transport::{
    NdsifAdpcmEvent, NdsifAudioControl, NdsifAudioControlEvent,
    NdsifHardwareWorkers, NdsifHostTimeline, NdsifRegisterEvent, NdsifSerialConfig,
    NdsifSerialTransport, NdsifTransport, NdsifWorkerDevice,
};

const RATE: u32 = 44_100;
const PREBUFFER_FRAMES: usize = 4_410;
const END_FRAME: usize = 6_000;
// Share the serial connection with the worker that will perform device I/O.
let connection = Arc::new(Mutex::new(
    NdsifSerialTransport::open(NdsifSerialConfig {
        port: "/dev/ttyACM0".to_owned(),
        baud_rate: 115_200,
        response_timeout: Duration::from_millis(500),
        reset_device: false,
    })?,
));
let device = NdsifWorkerDevice::new(Arc::clone(&connection), NonZeroUsize::new(1024).unwrap())?;
let recorder = device.recorder();
// Record both chip configurations before starting the worker.
{
    let mut recorder = recorder.borrow_mut();
    recorder.configure_chip(WriteId::YM2151, 0, 3_579_545)?;
    recorder.configure_audio_stream(0, 4_000_000, Divider::Div1024, RATE)?;
}

// Workers replay recorded batches; the timeline coordinates when audio starts.
let timeline = Arc::new(NdsifHostTimeline::pending());
let workers = NdsifHardwareWorkers::start(
    std::slice::from_ref(&device), Arc::clone(&timeline), NonZeroUsize::new(8).unwrap(),
)?;
// Replace this placeholder with encoded OKIM6258 ADPCM bytes.
let adpcm_data = [0_u8; 256];
{
    let mut recorder = recorder.borrow_mut();
    // Queue an ordinary YM2151 register write alongside the ADPCM stream.
    recorder.schedule_register_write(NdsifRegisterEvent {
        write_id: WriteId::YM2151,
        instance: 0,
        page: 0,
        write: RegisterWrite::new(0x20, 0xc7),
        sample_frame: 0,
        sample_rate: RATE,
    })?;
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

`NdsifHardwareWorkers` starts one I/O thread per device; its manager and
recorders stay on the calling thread. Start or cancel the timeline before
`finish` (dropping workers cancels it). Drop workers and transport handles
before resetting connections; close a connection if `reset_for_reuse` fails.

`NdsifSerialTransport::open` returns `NdsifTransportError` for invalid
configuration or serial, I/O, protocol, and initialization failures. Errors
crossing `NdsifTransport` are reported as `String`.

## YM2612 timed DAC

Regular scheduled writes are paced against host output frames and sent over
serial on a best-effort basis. On devices advertising the timed DAC profile,
primary YM2612 writes to registers `0x2A` and `0x2B` are instead buffered for
execution on the device's DAC clock. The scheduling API is the same, but timed
DAC playback requires prebuffering and coordinated startup.

The timed DAC profile is optional. Check that the device advertises support
before configuring YM2612. Replace the sample waveform with your PCM data.

```rust,no_run
use std::{
    num::NonZeroUsize,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::{Duration, Instant},
};
use ndsif::{RegisterWrite, WriteId};
use ndsif_transport::{
    NdsifHardwareWorkers, NdsifHostTimeline, NdsifRegisterEvent, NdsifSerialConfig,
    NdsifSerialTransport, NdsifTransport, NdsifWorkerDevice,
};

const RATE: u32 = 44_100;
const PREBUFFER_FRAMES: usize = 441;
const END_FRAME: usize = 441;
let transport = NdsifSerialTransport::open(NdsifSerialConfig {
    port: "/dev/ttyACM0".to_owned(),
    baud_rate: 115_200,
    response_timeout: Duration::from_millis(500),
    reset_device: false,
})?;
if !transport.supports_timed_dac() {
    return Err("device does not support the timed DAC profile".into());
}

// Unlike the previous OKIM6258 example, this uses YM2612's timed-DAC profile:
// samples are timed register writes, not AudioData bytes and AudioEvents.
// Configure the chip directly; no configure_audio_stream call is needed.
let connection = Arc::new(Mutex::new(transport));
let device = NdsifWorkerDevice::new(Arc::clone(&connection), NonZeroUsize::new(1024).unwrap())?;
let recorder = device.recorder();
recorder.borrow_mut().configure_chip(WriteId::YM2612, 0, 7_670_454)?;

let timeline = Arc::new(NdsifHostTimeline::pending());
let workers = NdsifHardwareWorkers::start(
    std::slice::from_ref(&device), Arc::clone(&timeline), NonZeroUsize::new(8).unwrap(),
)?;
let waveform = [
    0x80, 0x98, 0xb0, 0xc0, 0xb0, 0x98, 0x80, 0x68,
    0x50, 0x40, 0x50, 0x68, 0x80, 0x98, 0xb0, 0xc0,
];
{
    let mut recorder = recorder.borrow_mut();
    // Enable YM2612 DAC output with 0x2B, then send each PCM sample via 0x2A.
    recorder.schedule_register_write(NdsifRegisterEvent {
        write_id: WriteId::YM2612,
        instance: 0,
        page: 0,
        write: RegisterWrite::new(0x2b, 0x80),
        sample_frame: 0,
        sample_rate: RATE,
    })?;
    for (index, value) in waveform.into_iter().cycle().take(32).enumerate() {
        recorder.schedule_register_write(NdsifRegisterEvent {
            write_id: WriteId::YM2612,
            instance: 0,
            page: 0,
            write: RegisterWrite::new(0x2a, value),
            sample_frame: index * 2,
            sample_rate: RATE,
        })?;
    }
    // Disable DAC output and seal the timed-DAC stream. Unlike the prior
    // example, finish_dac_stream replaces finish_audio_stream here.
    recorder.schedule_register_write(NdsifRegisterEvent {
        write_id: WriteId::YM2612,
        instance: 0,
        page: 0,
        write: RegisterWrite::new(0x2b, 0x00),
        sample_frame: END_FRAME,
        sample_rate: RATE,
    })?;
    recorder.flush_scheduled_until(END_FRAME, RATE)?;
    recorder.finish_dac_stream(END_FRAME, RATE)?;
}

let cancelled = AtomicBool::new(false);
workers.enqueue(&cancelled)?;
workers.prepare_audio(PREBUFFER_FRAMES, RATE, &cancelled)?;
timeline.start(Instant::now() + Duration::from_millis(100))?;
workers.finish()?;
drop(recorder);
drop(device);
connection.lock().map_err(|_| "device lock poisoned")?.reset_for_reuse()?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

## License

BSD-3-Clause License
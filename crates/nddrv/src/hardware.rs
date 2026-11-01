//! Bounded device workers for hardware playback.

use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use ndsif_transport::{
    NdsifCommandBatch, NdsifHostTimeline, NdsifRecordingTransport, NdsifSerialTransport,
};

use crate::device::DeviceLease;

const QUEUE_BLOCKS: usize = 8;
const MAX_OPERATIONS_PER_BLOCK: usize = 16_384;

pub type Recorder = Rc<RefCell<NdsifRecordingTransport>>;

enum Work {
    Batch(NdsifCommandBatch),
    PrepareAudio {
        frames: usize,
        rate: u32,
        ready: mpsc::Sender<Result<(), String>>,
    },
}

struct DeviceWorker {
    id: u64,
    sender: mpsc::SyncSender<Work>,
    audio: bool,
}

pub fn recorders(lease: &DeviceLease<NdsifSerialTransport>) -> Result<Vec<Recorder>> {
    lease
        .devices()
        .iter()
        .map(|(id, device)| {
            let device = device
                .lock()
                .map_err(|_| anyhow!("NanoDrive device {id} worker failed"))?;
            Ok(Rc::new(RefCell::new(NdsifRecordingTransport::new(
                &device,
                NonZeroUsize::new(MAX_OPERATIONS_PER_BLOCK).expect("nonzero operation limit"),
            ))))
        })
        .collect()
}

pub struct HardwareWorkers {
    senders: Vec<DeviceWorker>,
    threads: Vec<JoinHandle<Result<(), String>>>,
    errors: mpsc::Receiver<String>,
    timeline: Arc<NdsifHostTimeline>,
    joined: bool,
}

impl HardwareWorkers {
    pub fn start(
        lease: &DeviceLease<NdsifSerialTransport>,
        recorders: &[Recorder],
        timeline: Arc<NdsifHostTimeline>,
    ) -> Result<Self> {
        if lease.devices().len() != recorders.len() {
            bail!("NanoDrive recorder and device counts do not match");
        }
        let (errors_tx, errors) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        let mut workers = Self {
            senders: Vec::new(),
            threads: Vec::new(),
            errors,
            timeline,
            joined: false,
        };
        for ((id, device), recorder) in lease.devices().iter().zip(recorders) {
            let setup = recorder
                .try_borrow_mut()
                .map_err(|_| anyhow!("NanoDrive recorder {id} is already borrowed"))?
                .take_batch()?;
            let audio = setup.configures_audio_stream();
            let (sender, receiver) = mpsc::sync_channel::<Work>(QUEUE_BLOCKS);
            let device = Arc::clone(device);
            let timeline = Arc::clone(&workers.timeline);
            let ready_tx = ready_tx.clone();
            let errors_tx = errors_tx.clone();
            let id = *id;
            let handle = thread::Builder::new()
                .name(format!("nanodrive-{id}"))
                .spawn(move || {
                    let setup_result = (|| {
                        let mut device = device
                            .lock()
                            .map_err(|_| "device lock poisoned".to_owned())?;
                        if !audio {
                            device
                                .set_host_timeline(Arc::clone(&timeline))
                                .map_err(|error| error.to_string())?;
                        }
                        setup
                            .replay(&mut *device)
                            .map_err(|error| error.to_string())?;
                        if audio {
                            device.defer_audio_stream_start_with_cancellation(Arc::clone(&timeline))?;
                        }
                        Ok::<(), String>(())
                    })();
                    if let Err(error) = &setup_result {
                        let _ = ready_tx.send(Err(format!("NanoDrive device {id}: {error}")));
                        timeline.cancel();
                        return Err(error.clone());
                    }
                    if ready_tx.send(Ok(())).is_err() {
                        timeline.cancel();
                        return Err("playback worker stopped during device setup".into());
                    }
                    drop(ready_tx);
                    loop {
                        if timeline.is_cancelled() {
                            return Ok(());
                        }
                        match receiver.recv_timeout(Duration::from_millis(10)) {
                            Ok(work) => {
                                let result = match work {
                                    Work::Batch(batch) => device
                                        .lock()
                                        .map_err(|_| "device lock poisoned".to_owned())
                                        .and_then(|mut device| {
                                            batch.replay(&mut *device).map_err(|error| error.to_string())
                                        }),
                                    Work::PrepareAudio { frames, rate, ready } => {
                                        let result = device.lock()
                                            .map_err(|_| "device lock poisoned".to_owned())
                                            .and_then(|mut device| device.prepare_audio_stream_start_for_frames(frames, rate));
                                        match result {
                                            Ok(()) => {
                                                if ready.send(Ok(())).is_err() {
                                                    Err("playback worker stopped during audio preparation".into())
                                                } else {
                                                    timeline.wait_until_presentation().and_then(|()| {
                                                        if timeline.is_cancelled() {
                                                            return Err("NDSIF host pacing was cancelled".into());
                                                        }
                                                        device.lock()
                                                            .map_err(|_| "device lock poisoned".to_owned())
                                                            .and_then(|mut device| device.start_prepared_audio_stream())
                                                    })
                                                }
                                            }
                                            Err(error) => {
                                                let _ = ready.send(Err(error.clone()));
                                                Err(error)
                                            }
                                        }
                                    }
                                };
                                if let Err(error) = result {
                                    if timeline.is_cancelled() {
                                        return Ok(());
                                    }
                                    let message = format!("NanoDrive device {id}: {error}");
                                    if errors_tx.send(message.clone()).is_err() {
                                        eprintln!("{message}");
                                    }
                                    timeline.cancel();
                                    return Err(message);
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => {}
                            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                        }
                    }
                })
                .context("start NanoDrive device worker")?;
            workers.senders.push(DeviceWorker { id, sender, audio });
            workers.threads.push(handle);
        }
        drop(ready_tx);
        drop(errors_tx);
        for _ in 0..workers.threads.len() {
            ready
                .recv()
                .context("NanoDrive worker stopped during setup")?
                .map_err(|error| anyhow!(error))?;
        }
        Ok(workers)
    }

    pub fn check(&self) -> Result<()> {
        match self.errors.try_recv() {
            Ok(error) => bail!("{error}"),
            Err(mpsc::TryRecvError::Empty) => Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => {
                bail!("NanoDrive workers stopped unexpectedly")
            }
        }
    }

    pub fn enqueue(&self, recorders: &[Recorder], cancelled: &AtomicBool) -> Result<()> {
        if recorders.len() != self.senders.len() {
            bail!("NanoDrive recorder and worker counts do not match");
        }
        for (recorder, worker) in recorders.iter().zip(&self.senders) {
            let batch = recorder
                .try_borrow_mut()
                .map_err(|_| anyhow!("NanoDrive recorder is already borrowed"))?
                .take_batch()?;
            if batch.is_empty() {
                continue;
            }
            self.send(worker, Work::Batch(batch), cancelled)?;
        }
        Ok(())
    }

    fn send(&self, worker: &DeviceWorker, mut work: Work, cancelled: &AtomicBool) -> Result<()> {
        loop {
            if cancelled.load(Ordering::Acquire) {
                return Ok(());
            }
            if let Err(error) = self.check() {
                if cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                return Err(error);
            }
            match worker.sender.try_send(work) {
                Ok(()) => return Ok(()),
                Err(mpsc::TrySendError::Full(pending)) => {
                    work = pending;
                    thread::sleep(Duration::from_millis(2));
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {
                    if cancelled.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    self.check()?;
                    bail!("NanoDrive device {} worker disconnected", worker.id);
                }
            }
        }
    }

    /// Wait until each device-timed stream has accepted the initial PCM span.
    pub fn prepare_audio(&self, frames: usize, rate: u32, cancelled: &AtomicBool) -> Result<()> {
        for worker in self.senders.iter().filter(|worker| worker.audio) {
            let (ready, result) = mpsc::channel();
            self.send(
                worker,
                Work::PrepareAudio {
                    frames,
                    rate,
                    ready,
                },
                cancelled,
            )?;
            loop {
                if cancelled.load(Ordering::Acquire) {
                    return Ok(());
                }
                match result.recv_timeout(Duration::from_millis(5)) {
                    Ok(result) => {
                        result
                            .map_err(|error| anyhow!("NanoDrive device {}: {error}", worker.id))?;
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => self.check()?,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.check()?;
                        bail!(
                            "NanoDrive device {} stopped during audio preparation",
                            worker.id
                        );
                    }
                }
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<()> {
        self.senders.clear();
        let result = self.join();
        self.joined = true;
        result
    }

    fn join(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        for worker in self.threads.drain(..) {
            match worker.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => failures.push(error),
                Err(_) => failures.push("NanoDrive worker panicked".into()),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            bail!("{}", failures.join("; "))
        }
    }
}

impl Drop for HardwareWorkers {
    fn drop(&mut self) {
        if !self.joined {
            self.timeline.cancel();
            self.senders.clear();
            if let Err(error) = self.join() {
                eprintln!("NanoDrive worker cleanup failed: {error:#}");
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::device::{DeviceEvent, DeviceService};
    use crate::routing::Device;
    use chipstream::chip::{NdsifRegisterEvent, NdsifTransport};
    use chipstream::sequencer::Chip;
    use ndsif::{Capabilities, Decoder, Reply, Request, Response, WriteId};
    use serialport::SerialPort;
    use std::io::{Read, Write};
    use std::time::Instant;

    fn fake_device(
        id: u64,
        reject_clock: bool,
        writes: mpsc::Sender<(u64, u8, Instant)>,
    ) -> (
        Device,
        serialport::TTYPort,
        mpsc::Sender<()>,
        JoinHandle<()>,
    ) {
        let (mut fake, port) = serialport::TTYPort::pair().unwrap();
        fake.set_timeout(Duration::from_secs(3)).unwrap();
        let device = Device {
            id,
            name: format!("Test {id}"),
            port: port.name().unwrap(),
            baud_rate: crate::device::BAUD_RATE,
        };
        let (release, wait) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut decoder = Decoder::new();
            loop {
                let mut frame = None;
                while frame.is_none() {
                    let mut byte = [0];
                    fake.read_exact(&mut byte).unwrap();
                    decoder.push(&byte, |result| frame = Some(result.unwrap()));
                }
                let frame = frame.unwrap();
                let (reply, stop) = match Request::decode(&frame).unwrap() {
                    Request::Ping(data) => (Some(Reply::Ping(data)), false),
                    Request::GetCapabilities { offset: 0 } => {
                        let entries = &[0x54, 0, 0, 4, 2, 0, 0, 0, 0];
                        (
                            Some(Reply::Capabilities(
                                Capabilities::new(256, 0, 0, 0, u16::MAX, entries).unwrap(),
                            )),
                            false,
                        )
                    }
                    Request::GetInfo => (
                        Some(Reply::Info(ndsif::DeviceInfo {
                            model: "Test NanoDrive",
                            firmware: "test",
                        })),
                        false,
                    ),
                    Request::SetChipClock { .. } => (
                        Some(if reject_clock {
                            Reply::Rejected
                        } else {
                            Reply::Complete
                        }),
                        false,
                    ),
                    Request::WriteRegisters { writes: bytes, .. } => {
                        writes
                            .send((id, bytes.as_bytes()[1], Instant::now()))
                            .unwrap();
                        (None, false)
                    }
                    Request::Reset => (Some(Reply::Complete), true),
                    request => panic!("unexpected request: {request:?}"),
                };
                if let Some(reply) = reply {
                    let response = Response::for_request(&frame, reply)
                        .unwrap()
                        .to_frame()
                        .unwrap()
                        .encode();
                    fake.write_all(response.as_bytes()).unwrap();
                }
                if stop {
                    break;
                }
            }
            wait.recv().unwrap();
        });
        (device, port, release, thread)
    }

    fn fake_audio_device(
        id: u64,
        starts: mpsc::Sender<Instant>,
        fail_start: bool,
    ) -> (
        Device,
        serialport::TTYPort,
        mpsc::Sender<()>,
        JoinHandle<()>,
    ) {
        let (mut fake, port) = serialport::TTYPort::pair().unwrap();
        fake.set_timeout(Duration::from_secs(3)).unwrap();
        let device = Device {
            id,
            name: format!("Audio {id}"),
            port: port.name().unwrap(),
            baud_rate: crate::device::BAUD_RATE,
        };
        let (release, wait) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut decoder = Decoder::new();
            let mut accepted = 0_u32;
            let mut running = false;
            let mut reset = false;
            loop {
                let mut frame = None;
                while frame.is_none() {
                    let mut byte = [0];
                    fake.read_exact(&mut byte).unwrap();
                    decoder.push(&byte, |result| frame = Some(result.unwrap()));
                }
                let frame = frame.unwrap();
                let reply = match Request::decode(&frame).unwrap() {
                    Request::Ping(data) => Some(Reply::Ping(data)),
                    Request::GetCapabilities { offset: 0 } => {
                        let entries = &[0x54, 0, 0, 4, 2, 0, 0, 0, 0, 0xb7, 0, 0, 4, 2, 0, 0, 0, 0];
                        Some(Reply::Capabilities(
                            Capabilities::new(256, 8192, 256, 0, u16::MAX, entries).unwrap(),
                        ))
                    }
                    Request::GetInfo => Some(Reply::Info(ndsif::DeviceInfo {
                        model: "Test NanoDrive8",
                        firmware: "test",
                    })),
                    Request::SetChipClock { .. } => Some(Reply::Complete),
                    Request::AudioData {
                        position, adpcm, ..
                    } => {
                        assert_eq!(position.get(), accepted);
                        accepted += adpcm.len() as u32;
                        None
                    }
                    Request::AudioEvent { .. } => None,
                    Request::AudioStart { .. } => {
                        assert!(!running);
                        running = !fail_start;
                        starts.send(Instant::now()).unwrap();
                        None
                    }
                    Request::AudioStatus { .. } => Some(Reply::AudioStatus(ndsif::AudioStatus {
                        accepted,
                        played: 0,
                        pending: accepted,
                        underflows: 0,
                        overflows: 0,
                        rejected: 0,
                        max_pending: accepted,
                        flags: if running {
                            ndsif::StatusFlags::RUNNING
                        } else {
                            ndsif::StatusFlags::from_bits_retain(0)
                        },
                        late_events: 0,
                        max_event_lag: 0,
                    })),
                    Request::Reset => {
                        accepted = 0;
                        running = false;
                        reset = true;
                        Some(Reply::Complete)
                    }
                    request => panic!("unexpected audio request: {request:?}"),
                };
                let stop = reset
                    && matches!(
                        Request::decode(&frame).unwrap(),
                        Request::AudioStatus { .. }
                    );
                if let Some(reply) = reply {
                    let response = Response::for_request(&frame, reply)
                        .unwrap()
                        .to_frame()
                        .unwrap()
                        .encode();
                    fake.write_all(response.as_bytes()).unwrap();
                }
                if stop {
                    break;
                }
            }
            wait.recv().unwrap();
        });
        (device, port, release, worker)
    }

    fn record_write(recorder: &Recorder, value: u8, frame: usize) {
        let mut recorder = recorder.borrow_mut();
        recorder
            .schedule_register_write(NdsifRegisterEvent {
                chip: Chip::Ym2151,
                write_id: WriteId::YM2151,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x20, value),
                sample_frame: frame,
                sample_rate: 44_100,
            })
            .unwrap();
        recorder.flush_scheduled_until(frame, 44_100).unwrap();
    }

    #[test]
    fn workers_share_start_and_cancel_pending_writes_before_return() {
        let (writes_tx, writes_rx) = mpsc::channel();
        let (first, port1, release1, fake1) = fake_device(1, false, writes_tx.clone());
        let (second, port2, release2, fake2) = fake_device(2, false, writes_tx);
        let service = DeviceService::new();
        service.connect(first).unwrap();
        service.connect(second).unwrap();
        for _ in 0..2 {
            assert!(matches!(
                service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
                DeviceEvent::Connected(_, _, _)
            ));
        }
        let lease = service.playback_handle().acquire(vec![1, 2]).unwrap();
        let recorders = recorders(&lease).unwrap();
        for recorder in &recorders {
            recorder
                .borrow_mut()
                .configure_chip(WriteId::YM2151, 0, 4_000_000)
                .unwrap();
        }
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        for (index, recorder) in recorders.iter().enumerate() {
            record_write(recorder, index as u8 + 1, 0);
        }
        let cancelled = AtomicBool::new(false);
        workers.enqueue(&recorders, &cancelled).unwrap();
        assert!(writes_rx.recv_timeout(Duration::from_millis(50)).is_err());
        timeline
            .start(Instant::now() + Duration::from_millis(30))
            .unwrap();
        let first = writes_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let second = writes_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_ne!(first.0, second.0);
        assert_eq!(first.1, first.0 as u8);
        assert_eq!(second.1, second.0 as u8);
        assert!(
            first.2.max(second.2).duration_since(first.2.min(second.2))
                < Duration::from_millis(250)
        );
        for recorder in &recorders {
            record_write(recorder, 9, 44_100);
        }
        workers.enqueue(&recorders, &cancelled).unwrap();
        timeline.cancel();
        workers.finish().unwrap();
        drop(recorders);
        lease.release().unwrap();
        assert!(writes_rx.recv_timeout(Duration::from_millis(50)).is_err());
        release1.send(()).unwrap();
        release2.send(()).unwrap();
        fake1.join().unwrap();
        fake2.join().unwrap();
        drop((port1, port2));
    }

    #[test]
    fn audio_stream_and_host_device_start_on_the_shared_timeline() {
        exercise_audio_workers(false, false);
    }

    #[test]
    fn audio_stream_is_reset_if_stopped_before_presentation() {
        exercise_audio_workers(true, false);
    }

    #[test]
    fn short_audio_stream_is_prepared_and_started_before_eof_cleanup() {
        exercise_audio_workers(false, true);
    }

    fn exercise_audio_workers(cancel_before_start: bool, short: bool) {
        use chipstream::sequencer::{Instance, VgmSequencer};
        use soundlog::chip::{Okim6258Spec, Ym2151Spec};
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};

        let (starts_tx, starts_rx) = mpsc::channel();
        let (writes_tx, writes_rx) = mpsc::channel();
        let (audio, audio_port, release_audio, fake_audio) = fake_audio_device(1, starts_tx, false);
        let (host, host_port, release_host, fake_host) = fake_device(2, false, writes_tx);
        let service = DeviceService::new();
        service.connect(audio).unwrap();
        service.connect(host).unwrap();
        for _ in 0..2 {
            assert!(matches!(
                service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
                DeviceEvent::Connected(_, _, _)
            ));
        }
        let lease = service.playback_handle().acquire(vec![1, 2]).unwrap();
        let recorders = recorders(&lease).unwrap();
        let routes = [
            ndsif_transport::NdsifRoute {
                chip: Chip::Ym2151,
                source: Instance::Primary,
                device: 0,
                target: Instance::Primary,
            },
            ndsif_transport::NdsifRoute {
                chip: Chip::Okim6258,
                source: Instance::Primary,
                device: 0,
                target: Instance::Primary,
            },
        ];
        let factories =
            ndsif_transport::create_routed_transport_factories(&recorders, &routes).unwrap();
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x20,
                value: 0xc7,
            },
        );
        for (register, value) in [(0x00, 2), (0x01, 0x21)] {
            builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(if short { 441 } else { 20_000 }));
        let mut sequencer =
            VgmSequencer::new_with_factories(builder.finalize(), 44_100, 1024, factories).unwrap();
        recorders[1]
            .borrow_mut()
            .configure_chip(WriteId::YM2151, 0, 4_000_000)
            .unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        let cancelled = AtomicBool::new(false);
        let mut frames = 0;
        for index in 0..4 {
            let block = sequencer.render().unwrap();
            if let Some(block) = &block {
                frames += block.frames().len();
            }
            if index == 0 {
                record_write(&recorders[1], 0x23, 0);
            }
            workers.enqueue(&recorders, &cancelled).unwrap();
            if block.is_none() {
                break;
            }
        }
        workers.prepare_audio(frames, 44_100, &cancelled).unwrap();
        assert!(starts_rx.recv_timeout(Duration::from_millis(30)).is_err());
        assert!(writes_rx.recv_timeout(Duration::from_millis(30)).is_err());
        if cancel_before_start {
            cancelled.store(true, Ordering::Release);
            if !short {
                timeline.cancel();
            }
        } else {
            let origin = Instant::now() + Duration::from_millis(50);
            timeline.start(origin).unwrap();
            let audio_start = starts_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            let (_, value, host_write) = writes_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(value, 0x23);
            assert!(audio_start >= origin);
            assert!(host_write >= origin);
            assert!(
                audio_start
                    .max(host_write)
                    .duration_since(audio_start.min(host_write))
                    < Duration::from_millis(250)
            );
            timeline.cancel();
        }
        workers.finish().unwrap();
        drop(sequencer);
        drop(recorders);
        lease.release().unwrap();
        if cancel_before_start {
            assert!(starts_rx.recv_timeout(Duration::from_millis(30)).is_err());
            assert!(writes_rx.recv_timeout(Duration::from_millis(30)).is_err());
        }
        release_audio.send(()).unwrap();
        release_host.send(()).unwrap();
        fake_audio.join().unwrap();
        fake_host.join().unwrap();
        drop((audio_port, host_port));
    }

    #[test]
    fn insufficient_audio_prebuffer_reports_error_and_returns_device() {
        use chipstream::chip::NdsifAdpcmEvent;
        let (starts_tx, starts_rx) = mpsc::channel();
        let (device, port, release, fake) = fake_audio_device(1, starts_tx, false);
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        let recorders = recorders(&lease).unwrap();
        {
            let mut recorder = recorders[0].borrow_mut();
            recorder
                .configure_chip(WriteId::OKIM6258, 0, 4_000_000)
                .unwrap();
            recorder
                .configure_audio_stream(0, 4_000_000, ndsif::Divider::Div1024, 44_100)
                .unwrap();
        }
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        {
            let mut recorder = recorders[0].borrow_mut();
            recorder
                .schedule_adpcm_data(NdsifAdpcmEvent {
                    instance: 0,
                    position: ndsif::BytePosition::new(0),
                    sample_frame: 0,
                    sample_rate: 44_100,
                    value: 0x12,
                })
                .unwrap();
            recorder.flush_scheduled_until(0, 44_100).unwrap();
        }
        let cancelled = AtomicBool::new(false);
        workers.enqueue(&recorders, &cancelled).unwrap();
        let error = workers
            .prepare_audio(44_100, 44_100, &cancelled)
            .unwrap_err();
        assert!(format!("{error:#}").contains("prebuffer is insufficient"));
        drop(workers);
        drop(recorders);
        lease.release().unwrap();
        assert!(starts_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn failed_audio_start_is_reported_and_device_is_reset() {
        use chipstream::chip::NdsifAdpcmEvent;
        let (starts_tx, starts_rx) = mpsc::channel();
        let (device, port, release, fake) = fake_audio_device(1, starts_tx, true);
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        let recorders = recorders(&lease).unwrap();
        {
            let mut recorder = recorders[0].borrow_mut();
            recorder
                .configure_chip(WriteId::OKIM6258, 0, 4_000_000)
                .unwrap();
            recorder
                .configure_audio_stream(0, 4_000_000, ndsif::Divider::Div1024, 44_100)
                .unwrap();
        }
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        {
            let mut recorder = recorders[0].borrow_mut();
            for position in 0..20 {
                recorder
                    .schedule_adpcm_data(NdsifAdpcmEvent {
                        instance: 0,
                        position: ndsif::BytePosition::new(position),
                        sample_frame: 0,
                        sample_rate: 44_100,
                        value: 0x12,
                    })
                    .unwrap();
            }
            recorder.flush_scheduled_until(441, 44_100).unwrap();
        }
        let cancelled = AtomicBool::new(false);
        workers.enqueue(&recorders, &cancelled).unwrap();
        workers.prepare_audio(441, 44_100, &cancelled).unwrap();
        timeline.start(Instant::now()).unwrap();
        starts_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let error = loop {
            if let Err(error) = workers.check() {
                break error;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        assert!(format!("{error:#}").contains("did not enter the running state"));
        assert!(workers.finish().is_err());
        drop(recorders);
        lease.release().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn setup_failure_is_reported_and_device_is_reset() {
        let (writes_tx, _writes_rx) = mpsc::channel();
        let (device, port, release, fake) = fake_device(1, true, writes_tx);
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        let recorders = recorders(&lease).unwrap();
        recorders[0]
            .borrow_mut()
            .configure_chip(WriteId::YM2151, 0, 4_000_000)
            .unwrap();
        let result =
            HardwareWorkers::start(&lease, &recorders, Arc::new(NdsifHostTimeline::pending()));
        assert!(matches!(result, Err(error) if format!("{error:#}").contains("rejected")));
        drop(recorders);
        lease.release().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn replay_error_stops_worker_and_returns_the_connection() {
        let (writes_tx, _writes_rx) = mpsc::channel();
        let (device, port, release, fake) = fake_device(1, false, writes_tx);
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        let recorders = recorders(&lease).unwrap();
        recorders[0]
            .borrow_mut()
            .configure_chip(WriteId::YM2151, 0, 4_000_000)
            .unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        recorders[0]
            .borrow_mut()
            .configure_chip(WriteId::OKIM6258, 0, 4_000_000)
            .unwrap();
        workers
            .enqueue(&recorders, &AtomicBool::new(false))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let error = loop {
            if let Err(error) = workers.check() {
                break error;
            }

            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(2));
        };
        assert!(format!("{error:#}").contains("0xb7"));
        assert!(workers.finish().is_err());
        assert!(timeline.is_cancelled());
        drop(recorders);
        lease.release().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn full_queue_is_bounded_and_stop_before_start_releases_workers() {
        let (writes_tx, writes_rx) = mpsc::channel();
        let (device, port, release, fake) = fake_device(1, false, writes_tx);
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        let recorders = recorders(&lease).unwrap();
        recorders[0]
            .borrow_mut()
            .configure_chip(WriteId::YM2151, 0, 4_000_000)
            .unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        for value in 0..=QUEUE_BLOCKS {
            record_write(&recorders[0], value as u8, 0);
            workers.enqueue(&recorders, &cancelled).unwrap();
        }
        let flag = Arc::clone(&cancelled);
        let clock = Arc::clone(&timeline);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            flag.store(true, Ordering::Release);
            clock.cancel();
        });
        record_write(&recorders[0], 99, 0);
        let start = Instant::now();
        workers.enqueue(&recorders, &cancelled).unwrap();
        assert!(start.elapsed() >= Duration::from_millis(30));
        canceller.join().unwrap();
        workers.finish().unwrap();
        drop(recorders);
        lease.release().unwrap();
        assert!(writes_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn sequencer_batches_reach_worker_only_after_presentation_starts() {
        use chipstream::sequencer::{Instance, VgmSequencer};
        use soundlog::chip::Ym2151Spec;
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};

        let (writes_tx, writes_rx) = mpsc::channel();
        let (device, port, release, fake) = fake_device(1, false, writes_tx);
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        let recorders = recorders(&lease).unwrap();
        let routes = [ndsif_transport::NdsifRoute {
            chip: Chip::Ym2151,
            source: Instance::Primary,
            device: 0,
            target: Instance::Primary,
        }];
        let factories =
            ndsif_transport::create_routed_transport_factories(&recorders, &routes).unwrap();
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x20,
                value: 0xc7,
            },
        );
        builder.add_vgm_command(WaitSamples(441));
        let mut sequencer = VgmSequencer::from_vgm_with_factories(
            Vec::<u8>::from(builder.finalize()),
            44_100,
            1024,
            factories,
        )
        .unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        loop {
            let block = sequencer.render().unwrap();
            workers
                .enqueue(&recorders, &AtomicBool::new(false))
                .unwrap();
            if block.is_none() {
                break;
            }
        }
        assert!(writes_rx.recv_timeout(Duration::from_millis(50)).is_err());
        timeline.start(Instant::now()).unwrap();
        let (_, value, _) = writes_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(value, 0xc7);
        workers.finish().unwrap();
        drop(sequencer);
        drop(recorders);
        lease.release().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }
}

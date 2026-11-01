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

pub fn supports_master_volume(lease: &DeviceLease<NdsifSerialTransport>) -> Result<bool> {
    lease
        .devices()
        .iter()
        .try_fold(true, |supported, (id, device)| {
            let device = device
                .lock()
                .map_err(|_| anyhow!("NanoDrive device {id} worker failed"))?;
            Ok(supported && device.supports_output_volume())
        })
}

enum Work {
    Batch(NdsifCommandBatch),
    PrepareAudio {
        frames: usize,
        rate: u32,
        ready: mpsc::Sender<Result<(), String>>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DeviceTiming {
    Host,
    Audio,
    Dac,
}

struct DeviceWorker {
    id: u64,
    sender: mpsc::SyncSender<Work>,
    timing: DeviceTiming,
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
            let timing = if setup.configures_audio_stream() {
                DeviceTiming::Audio
            } else if setup.configures_primary_ym2612()
                && device
                    .lock()
                    .map_err(|_| anyhow!("NanoDrive device {id} worker failed"))?
                    .supports_timed_dac()
            {
                DeviceTiming::Dac
            } else {
                DeviceTiming::Host
            };
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
                        device.set_output_fade_timeline(Arc::clone(&timeline))?;
                        device.initialize_output_volume()?;
                        if timing == DeviceTiming::Host {
                            device
                                .set_host_timeline(Arc::clone(&timeline))
                                .map_err(|error| error.to_string())?;
                        }
                        setup
                            .replay(&mut *device)
                            .map_err(|error| error.to_string())?;
                        match timing {
                            DeviceTiming::Audio => device.defer_audio_stream_start_with_cancellation(Arc::clone(&timeline))?,
                            DeviceTiming::Dac => device.defer_timed_dac_start_with_cancellation(Arc::clone(&timeline))?,
                            DeviceTiming::Host => {}
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
                        let result = match receiver.recv_timeout(Duration::from_millis(10)) {
                            Ok(work) => match work {
                                    Work::Batch(batch) => device
                                        .lock()
                                        .map_err(|_| "device lock poisoned".to_owned())
                                        .and_then(|mut device| {
                                            batch.replay(&mut *device).map_err(|error| error.to_string())
                                        }),
                                    Work::PrepareAudio { frames, rate, ready } => {
                                        let result = device.lock()
                                            .map_err(|_| "device lock poisoned".to_owned())
                                            .and_then(|mut device| match timing {
                                                DeviceTiming::Audio => device.prepare_audio_stream_start_for_frames(frames, rate),
                                                DeviceTiming::Dac => device.prepare_timed_dac_start_for_frames(frames, rate),
                                                DeviceTiming::Host => unreachable!("host workers do not prepare streams"),
                                            });
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
                                                            .and_then(|mut device| {
                                                                device.synchronize_output_volume()?;
                                                                match timing {
                                                                    DeviceTiming::Audio => device.start_prepared_audio_stream(),
                                                                    DeviceTiming::Dac => device.start_prepared_timed_dac(),
                                                                    DeviceTiming::Host => unreachable!("host workers do not start streams"),
                                                                }
                                                            })
                                                    })
                                                }
                                            }
                                            Err(error) => {
                                                let _ = ready.send(Err(error.clone()));
                                                Err(error)
                                            }
                                        }
                                    }
                            },
                            Err(mpsc::RecvTimeoutError::Timeout) => device
                                .lock()
                                .map_err(|_| "device lock poisoned".to_owned())
                                .and_then(|mut device| device.update_output_fade()),
                            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
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
                })
                .context("start NanoDrive device worker")?;
            workers.senders.push(DeviceWorker { id, sender, timing });
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
        for worker in self
            .senders
            .iter()
            .filter(|worker| worker.timing != DeviceTiming::Host)
        {
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

    fn fake_dac_device(
        starts: mpsc::Sender<Instant>,
    ) -> (
        Device,
        serialport::TTYPort,
        mpsc::Sender<()>,
        JoinHandle<()>,
    ) {
        fake_dac_device_observed(starts, None)
    }

    fn fake_dac_device_observed(
        starts: mpsc::Sender<Instant>,
        volumes: Option<mpsc::Sender<(u8, Instant)>>,
    ) -> (
        Device,
        serialport::TTYPort,
        mpsc::Sender<()>,
        JoinHandle<()>,
    ) {
        let (mut fake, port) = serialport::TTYPort::pair().unwrap();
        fake.set_timeout(Duration::from_secs(3)).unwrap();
        let device = Device {
            id: 1,
            name: "Test DAC".into(),
            port: port.name().unwrap(),
            baud_rate: crate::device::BAUD_RATE,
        };
        let (release, wait) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut decoder = Decoder::new();
            let mut accepted = 0_u32;
            let mut tick = 0_u64;
            let mut running = false;
            let mut sealed = false;
            let mut reset = false;
            let mut events = 0_u32;
            loop {
                let mut frame = None;
                while frame.is_none() {
                    let mut byte = [0];
                    fake.read_exact(&mut byte).unwrap();
                    decoder.push(&byte, |result| frame = Some(result.unwrap()));
                }
                let frame = frame.unwrap();
                let reply = match Request::decode(&frame).unwrap() {
                    Request::Ping(bytes) => Some(Reply::Ping(bytes)),
                    Request::GetCapabilities { offset: 0 } => Some(Reply::Capabilities(
                        Capabilities::new(
                            256,
                            0,
                            0,
                            ndsif::CONTROL_FEATURE_DAC_PROFILE
                                | if volumes.is_some() {
                                    ndsif::CONTROL_FEATURE_OUTPUT_VOLUME
                                } else {
                                    0
                                },
                            u16::MAX,
                            &[0x52, 0, 0, 4, 2, 0, 0, 0, 0],
                        )
                        .unwrap(),
                    )),
                    Request::GetDacCapabilities => {
                        Some(Reply::DacCapabilities(ndsif::DacCapabilities {
                            profile: ndsif::DAC_PROFILE_YM2612,
                            revision: 40,
                            ticks_per_second: ndsif::DAC_TICKS_PER_SECOND,
                            record_capacity: 64,
                            event_capacity: 32,
                            max_records: 8,
                            record_bytes: 4,
                            max_dac_late_us: 0,
                        }))
                    }
                    Request::GetInfo => Some(Reply::Info(ndsif::DeviceInfo {
                        model: "Test DAC",
                        firmware: "test",
                    })),
                    Request::SetChipClock { .. } => Some(Reply::Complete),
                    Request::SetOutputVolume { attenuation } => {
                        volumes
                            .as_ref()
                            .unwrap()
                            .send((attenuation, Instant::now()))
                            .unwrap();
                        Some(Reply::Complete)
                    }
                    Request::DacData {
                        sequence,
                        position,
                        records,
                        ..
                    } => {
                        assert_eq!((sequence, position), (accepted, tick));
                        for record in records.iter() {
                            tick += u64::from(record.wait_ticks);
                            accepted += 1;
                        }
                        None
                    }
                    Request::DacRegisterEvent { writes, .. } => {
                        assert_eq!(writes, &[0x22, 0x08]);
                        events += 1;
                        None
                    }
                    Request::DacEnd {
                        next_sequence,
                        end_tick,
                        ..
                    } => {
                        assert_eq!((next_sequence, end_tick), (accepted, tick));
                        sealed = true;
                        None
                    }
                    Request::DacStart { .. } => {
                        running = true;
                        starts.send(Instant::now()).unwrap();
                        Some(Reply::Complete)
                    }
                    Request::DacStatus { .. } => {
                        let done = running && sealed;
                        let status = ndsif::DacStatus {
                            profile: ndsif::DAC_PROFILE_YM2612,
                            state: if done {
                                2
                            } else if running {
                                1
                            } else {
                                0
                            },
                            fault: 0,
                            sealed,
                            accepted,
                            consumed: if done { accepted } else { 0 },
                            queued: if done { 0 } else { accepted },
                            free: if done { 64 } else { 64 - accepted },
                            event_queued: if done { 0 } else { events },
                            event_consumed: if done { events } else { 0 },
                            underflows: 0,
                            overflows: 0,
                            rejected: 0,
                            late_dac: 0,
                            max_late_us: 0,
                            accepted_tick: tick,
                            played_tick: if done { tick } else { 0 },
                            executed_tick: if done { tick } else { 0 },
                        };
                        Some(Reply::DacStatus(status))
                    }
                    Request::Reset => {
                        accepted = 0;
                        tick = 0;
                        running = false;
                        sealed = false;
                        events = 0;
                        reset = true;
                        Some(Reply::Complete)
                    }
                    request => panic!("unexpected DAC request: {request:?}"),
                };
                let stop =
                    reset && matches!(Request::decode(&frame).unwrap(), Request::DacStatus { .. });
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

    #[test]
    fn timed_dac_worker_starts_at_presentation_and_returns_device() {
        let (starts, started) = mpsc::channel();
        let (device, port, release, fake) = fake_dac_device(starts);
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
            .configure_chip(WriteId::YM2612, 0, 7_670_454)
            .unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        {
            let mut recorder = recorders[0].borrow_mut();
            for (address, value) in [(0x22, 0x08), (0x2a, 0x80)] {
                recorder
                    .schedule_register_write(NdsifRegisterEvent {
                        chip: Chip::Ym2612,
                        write_id: WriteId::YM2612,
                        instance: 0,
                        page: 0,
                        write: ndsif::RegisterWrite::new(address, value),
                        sample_frame: 0,
                        sample_rate: 44_100,
                    })
                    .unwrap();
            }
            recorder.flush_scheduled_until(441, 44_100).unwrap();
            recorder.finish_dac_stream(441, 44_100).unwrap();
        }
        let cancelled = AtomicBool::new(false);
        workers.enqueue(&recorders, &cancelled).unwrap();
        workers.prepare_audio(441, 44_100, &cancelled).unwrap();
        assert!(started.recv_timeout(Duration::from_millis(30)).is_err());
        let origin = Instant::now() + Duration::from_millis(50);
        timeline.start(origin).unwrap();
        assert!(started.recv_timeout(Duration::from_secs(1)).unwrap() >= origin);
        workers.finish().unwrap();
        drop(recorders);
        lease.release().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn timed_dac_worker_cancelled_before_presentation_does_not_start() {
        let (starts, started) = mpsc::channel();
        let (device, port, release, fake) = fake_dac_device(starts);
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
            .configure_chip(WriteId::YM2612, 0, 7_670_454)
            .unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        recorders[0]
            .borrow_mut()
            .flush_scheduled_until(441, 44_100)
            .unwrap();
        let cancelled = AtomicBool::new(false);
        workers.enqueue(&recorders, &cancelled).unwrap();
        workers.prepare_audio(441, 44_100, &cancelled).unwrap();
        timeline.cancel();
        workers.finish().unwrap();
        assert!(started.recv_timeout(Duration::from_millis(30)).is_err());
        drop(recorders);
        lease.release().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

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
        fake_audio_device_observed(id, starts, fail_start, None, None)
    }

    fn fake_volume_device(
        id: u64,
        supported: bool,
        volumes: mpsc::Sender<(u64, u8, Instant)>,
    ) -> (
        Device,
        serialport::TTYPort,
        mpsc::Sender<()>,
        JoinHandle<()>,
    ) {
        fake_volume_device_with_rejection(id, supported, volumes, false)
    }

    fn fake_volume_device_with_rejection(
        id: u64,
        supported: bool,
        volumes: mpsc::Sender<(u64, u8, Instant)>,
        reject_initial_volume: bool,
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
            name: format!("Volume {id}"),
            port: port.name().unwrap(),
            baud_rate: crate::device::BAUD_RATE,
        };
        let (release, wait) = mpsc::channel();
        let thread = thread::spawn(move || {
            let mut decoder = Decoder::new();
            let mut modified = false;
            let mut reset = false;
            let mut reject_volume = reject_initial_volume;
            loop {
                let mut frame = None;
                while frame.is_none() {
                    let mut byte = [0];
                    fake.read_exact(&mut byte).unwrap();
                    decoder.push(&byte, |result| frame = Some(result.unwrap()));
                }
                let frame = frame.unwrap();
                let request = Request::decode(&frame).unwrap();
                let reply = match request {
                    Request::Ping(data) => Some(Reply::Ping(data)),
                    Request::GetCapabilities { offset: 0 } => Some(Reply::Capabilities(
                        Capabilities::new(
                            256,
                            0,
                            0,
                            if supported {
                                ndsif::CONTROL_FEATURE_OUTPUT_VOLUME
                            } else {
                                0
                            },
                            u16::MAX,
                            &[0x54, 0, 0, 4, 2, 0, 0, 0, 0],
                        )
                        .unwrap(),
                    )),
                    Request::GetInfo => Some(Reply::Info(ndsif::DeviceInfo {
                        model: "Volume test",
                        firmware: "test",
                    })),
                    Request::SetChipClock { .. } => Some(Reply::Complete),
                    Request::SetOutputVolume { attenuation } => {
                        assert!(supported);
                        volumes.send((id, attenuation, Instant::now())).unwrap();
                        modified = true;
                        if reject_volume {
                            reject_volume = false;
                            Some(Reply::Rejected)
                        } else {
                            Some(Reply::Complete)
                        }
                    }
                    Request::WriteRegisters { .. } => None,
                    Request::Reset => {
                        reset = true;
                        Some(Reply::Complete)
                    }
                    request => panic!("unexpected volume test request: {request:?}"),
                };
                let stop = reset
                    && (!modified
                        || matches!(request, Request::SetOutputVolume { attenuation: 0 }));
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

    #[test]
    fn volume_fade_tracks_presentation_on_multiple_workers_and_restores() {
        let (volumes, received) = mpsc::channel();
        let (first, port1, release1, fake1) = fake_volume_device(1, true, volumes.clone());
        let (second, port2, release2, fake2) = fake_volume_device(2, true, volumes.clone());
        let (third, port3, release3, fake3) = fake_volume_device(3, false, volumes);
        let service = DeviceService::new();
        for device in [first, second, third] {
            service.connect(device).unwrap();
            assert!(matches!(
                service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
                DeviceEvent::Connected(_, _, _)
            ));
        }
        let lease = service.playback_handle().acquire(vec![1, 2, 3]).unwrap();
        assert!(!supports_master_volume(&lease).unwrap());
        let recorders = recorders(&lease).unwrap();
        for recorder in &recorders {
            recorder
                .borrow_mut()
                .configure_chip(WriteId::YM2151, 0, 4_000_000)
                .unwrap();
        }
        let timeline = Arc::new(NdsifHostTimeline::pending());
        timeline
            .schedule_output_fade(0, 44_100, Duration::from_millis(300))
            .unwrap();
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        let mut initialized = Vec::new();
        for _ in 0..2 {
            let (id, value, _) = received.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(value, 0);
            initialized.push(id);
        }
        initialized.sort_unstable();
        assert_eq!(initialized, [1, 2]);
        record_write(&recorders[0], 0x7f, 44_100 / 2);
        workers
            .enqueue(&recorders, &AtomicBool::new(false))
            .unwrap();
        let origin = Instant::now() + Duration::from_millis(80);
        timeline.start(origin).unwrap();
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        let mut steps = [Vec::new(), Vec::new()];
        while steps.iter().any(|values| !values.contains(&96)) {
            let (id, value, at) = received.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(at >= origin);
            assert!(id == 1 || id == 2);
            steps[(id - 1) as usize].push(value);
        }
        for values in &steps {
            assert!(values.len() >= 3, "{values:?}");
            assert!(
                values.windows(2).all(|pair| pair[0] < pair[1]),
                "{values:?}"
            );
        }
        workers.finish().unwrap();
        drop(recorders);
        lease.release().unwrap();
        for _ in 0..2 {
            let (id, value, _) = received.recv_timeout(Duration::from_secs(1)).unwrap();
            assert!(id == 1 || id == 2);
            assert_eq!(value, 0);
        }
        for release in [release1, release2, release3] {
            release.send(()).unwrap();
        }
        for fake in [fake1, fake2, fake3] {
            fake.join().unwrap();
        }
        drop((port1, port2, port3));
    }

    #[test]
    fn mdx_loop_fade_reaches_nanodrive_output_and_restores_volume() {
        use chipstream::sequencer::{
            Instance, MdxSequencer, MdxSequencerOptions, measure_mdx_duration_frames,
        };
        use soundlog::mdx::command::{MdxCommand, MdxOpmRegisterWrite, MdxRelativeOffset, MdxRest};
        use soundlog::mdx::document::MdxBuilder;
        use soundlog::mdx::package::MdxPackage;

        let mut builder = MdxBuilder::new();
        builder
            .add_mdx_command(
                0,
                MdxOpmRegisterWrite {
                    register: 0x20,
                    value: 0xc7,
                },
            )
            .add_mdx_command(0, MdxRest { ticks: 1 })
            .add_mdx_command(
                0,
                MdxCommand::EndOfTrackLoop(MdxRelativeOffset {
                    opcode: 0xf1,
                    offset: -7,
                }),
            );
        let package = MdxPackage {
            mdx: builder.finalize().unwrap(),
            pdx: None,
        };
        let options = MdxSequencerOptions {
            loop_count: Some(2),
            ..Default::default()
        };
        let boundary = measure_mdx_duration_frames(package.clone(), 44_100, options, None)
            .unwrap()
            .unwrap() as usize;

        let (volumes, received) = mpsc::channel();
        let (device, port, release, fake) = fake_volume_device(1, true, volumes);
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
        let mut sequencer = MdxSequencer::with_options_and_fadeout_with_factories(
            package,
            44_100,
            1024,
            options,
            Some(88_200),
            factories,
        )
        .unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().1, 0);
        let cancelled = AtomicBool::new(false);
        for _ in 0..4 {
            assert!(sequencer.render().unwrap().is_some());
            workers.enqueue(&recorders, &cancelled).unwrap();
        }
        assert_eq!(sequencer.fade_start_frame(), Some(boundary));
        timeline
            .schedule_output_fade(boundary, 44_100, Duration::from_secs(2))
            .unwrap();
        let origin = Instant::now() + Duration::from_millis(80);
        timeline.start(origin).unwrap();
        assert!(received.recv_timeout(Duration::from_millis(50)).is_err());
        let fade_start = origin + Duration::from_secs_f64(boundary as f64 / 44_100.0);
        let mut steps = Vec::new();
        while !steps.contains(&96) {
            let (id, value, at) = received.recv_timeout(Duration::from_secs(3)).unwrap();
            assert_eq!(id, 1);
            assert!(at >= fade_start);
            steps.push(value);
        }
        assert!(steps.len() >= 3, "{steps:?}");
        assert!(steps.windows(2).all(|pair| pair[0] < pair[1]), "{steps:?}");
        workers.finish().unwrap();
        drop(recorders);
        lease.release().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().1, 0);
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn master_volume_is_applied_before_start_and_updated_while_idle() {
        let (volumes, received) = mpsc::channel();
        let (device, port, release, fake) = fake_volume_device(1, true, volumes);
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        assert!(supports_master_volume(&lease).unwrap());
        let recorders = recorders(&lease).unwrap();
        recorders[0]
            .borrow_mut()
            .configure_chip(WriteId::YM2151, 0, 4_000_000)
            .unwrap();
        let gain = Arc::new(ndsif_transport::NdsifOutputGain::new());
        gain.set(0.0).unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        timeline.set_output_gain(Arc::clone(&gain)).unwrap();
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().1, 96);
        gain.set(0.5).unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().1, 6);
        gain.set(0.0).unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().1, 96);
        timeline.cancel();
        workers.finish().unwrap();
        drop(recorders);
        lease.release().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().1, 0);
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn rejected_initial_volume_stops_setup_and_restores_device() {
        let (volumes, received) = mpsc::channel();
        let (device, port, release, fake) =
            fake_volume_device_with_rejection(1, true, volumes, true);
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
        let result = HardwareWorkers::start(&lease, &recorders, timeline);
        assert!(result.err().unwrap().to_string().contains("rejected"));
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().1, 0);
        drop(recorders);
        lease.release().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().1, 0);
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn volume_fade_runs_while_adpcm_stream_is_active() {
        use chipstream::sequencer::{Instance, VgmSequencer};
        use soundlog::chip::Okim6258Spec;
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};

        let (starts, started) = mpsc::channel();
        let (volumes, received) = mpsc::channel();
        let (device, port, release, fake) =
            fake_audio_device_observed(1, starts, false, None, Some(volumes));
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        let recorders = recorders(&lease).unwrap();
        let routes = [ndsif_transport::NdsifRoute {
            chip: Chip::Okim6258,
            source: Instance::Primary,
            device: 0,
            target: Instance::Primary,
        }];
        let factories =
            ndsif_transport::create_routed_transport_factories(&recorders, &routes).unwrap();
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
        for (register, value) in [(0, 2), (1, 0x21)] {
            builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(44_100));
        let mut sequencer =
            VgmSequencer::new_with_factories(builder.finalize(), 44_100, 1024, factories).unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        timeline
            .schedule_output_fade(0, 44_100, Duration::from_millis(300))
            .unwrap();
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().0, 0);
        let cancelled = AtomicBool::new(false);
        let mut frames = 0;
        for _ in 0..4 {
            frames += sequencer.render().unwrap().unwrap().frames().len();
            workers.enqueue(&recorders, &cancelled).unwrap();
        }
        workers.prepare_audio(frames, 44_100, &cancelled).unwrap();
        let origin = Instant::now() + Duration::from_millis(50);
        timeline.start(origin).unwrap();
        assert!(started.recv_timeout(Duration::from_secs(1)).unwrap() >= origin);
        for _ in 0..15 {
            sequencer.render().unwrap().unwrap();
            workers.enqueue(&recorders, &cancelled).unwrap();
            thread::sleep(Duration::from_millis(20));
        }
        let mut steps = Vec::new();
        while let Ok((attenuation, at)) = received.try_recv() {
            assert!(at >= origin);
            steps.push(attenuation);
        }
        assert!(steps.len() >= 3, "{steps:?}");
        assert!(steps.windows(2).all(|pair| pair[0] < pair[1]), "{steps:?}");
        timeline.cancel();
        workers.finish().unwrap();
        drop(sequencer);
        drop(recorders);
        lease.release().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().0, 0);
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    #[test]
    fn volume_fade_runs_while_timed_dac_is_active() {
        let (starts, started) = mpsc::channel();
        let (volumes, received) = mpsc::channel();
        let (device, port, release, fake) = fake_dac_device_observed(starts, Some(volumes));
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
            .configure_chip(WriteId::YM2612, 0, 7_670_454)
            .unwrap();
        let timeline = Arc::new(NdsifHostTimeline::pending());
        timeline
            .schedule_output_fade(0, 44_100, Duration::from_millis(300))
            .unwrap();
        let workers = HardwareWorkers::start(&lease, &recorders, Arc::clone(&timeline)).unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().0, 0);
        let cancelled = AtomicBool::new(false);
        for index in 0..=10 {
            let frame = index * 441;
            {
                let mut recorder = recorders[0].borrow_mut();
                recorder
                    .schedule_register_write(NdsifRegisterEvent {
                        chip: Chip::Ym2612,
                        write_id: WriteId::YM2612,
                        instance: 0,
                        page: 0,
                        write: ndsif::RegisterWrite::new(0x2a, 0x80 + index as u8),
                        sample_frame: frame,
                        sample_rate: 44_100,
                    })
                    .unwrap();
                recorder.flush_scheduled_until(frame + 441, 44_100).unwrap();
            }
            workers.enqueue(&recorders, &cancelled).unwrap();
            if index == 0 {
                workers.prepare_audio(441, 44_100, &cancelled).unwrap();
                let origin = Instant::now() + Duration::from_millis(50);
                timeline.start(origin).unwrap();
                assert!(started.recv_timeout(Duration::from_secs(1)).unwrap() >= origin);
            } else {
                thread::sleep(Duration::from_millis(30));
            }
        }
        let steps: Vec<_> = received.try_iter().map(|(value, _)| value).collect();
        assert!(steps.len() >= 3, "{steps:?}");
        assert!(steps.windows(2).all(|pair| pair[0] < pair[1]), "{steps:?}");
        timeline.cancel();
        workers.finish().unwrap();
        drop(recorders);
        lease.release().unwrap();
        assert_eq!(received.recv_timeout(Duration::from_secs(1)).unwrap().0, 0);
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
    }

    fn fake_audio_device_observed(
        id: u64,
        starts: mpsc::Sender<Instant>,
        fail_start: bool,
        events: Option<mpsc::Sender<(u32, &'static str)>>,
        volumes: Option<mpsc::Sender<(u8, Instant)>>,
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
                            Capabilities::new(
                                256,
                                8192,
                                256,
                                if volumes.is_some() {
                                    ndsif::CONTROL_FEATURE_OUTPUT_VOLUME
                                } else {
                                    0
                                },
                                u16::MAX,
                                entries,
                            )
                            .unwrap(),
                        ))
                    }
                    Request::GetInfo => Some(Reply::Info(ndsif::DeviceInfo {
                        model: "Test NanoDrive8",
                        firmware: "test",
                    })),
                    Request::SetChipClock { .. } => Some(Reply::Complete),
                    Request::SetOutputVolume { attenuation } => {
                        volumes
                            .as_ref()
                            .unwrap()
                            .send((attenuation, Instant::now()))
                            .unwrap();
                        Some(Reply::Complete)
                    }
                    Request::AudioData {
                        position, adpcm, ..
                    } => {
                        assert_eq!(position.get(), accepted);
                        accepted += adpcm.len() as u32;
                        None
                    }
                    Request::AudioEvent {
                        position, event, ..
                    } => {
                        if let Some(events) = &events {
                            let kind = match event {
                                ndsif::RequestEvent::Clock {
                                    clock: ndsif::OkiClock::Mhz8,
                                    divider: ndsif::Divider::Div768,
                                } => Some("clock8"),
                                ndsif::RequestEvent::Clock {
                                    clock: ndsif::OkiClock::Mhz4,
                                    divider: ndsif::Divider::Div512,
                                } => Some("clock4"),
                                ndsif::RequestEvent::Registers { .. } => Some("register"),
                                _ => None,
                            };
                            if let Some(kind) = kind {
                                events.send((position.get(), kind)).unwrap();
                            }
                        }
                        None
                    }
                    Request::AudioStart { clock, divider, .. } => {
                        if events.is_some() {
                            assert_eq!(
                                (clock, divider),
                                (ndsif::OkiClock::Mhz4, ndsif::Divider::Div1024)
                            );
                        }
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
    fn sequencer_clock_changes_and_fm_writes_share_the_device_timeline() {
        use chipstream::sequencer::{Instance, VgmSequencer};
        use soundlog::chip::{Okim6258Spec, Ym2151Spec};
        use soundlog::vgm::{VgmBuilder, command::WaitSamples};

        let (starts, started) = mpsc::channel();
        let (events, received) = mpsc::channel();
        let (device, port, release, fake) =
            fake_audio_device_observed(1, starts, false, Some(events), None);
        let service = DeviceService::new();
        service.connect(device).unwrap();
        assert!(matches!(
            service.events.recv_timeout(Duration::from_secs(2)).unwrap(),
            DeviceEvent::Connected(_, _, _)
        ));
        let lease = service.playback_handle().acquire(vec![1]).unwrap();
        let recorders = recorders(&lease).unwrap();
        let routes: Vec<_> = [Chip::Ym2151, Chip::Okim6258]
            .into_iter()
            .map(|chip| ndsif_transport::NdsifRoute {
                chip,
                source: Instance::Primary,
                device: 0,
                target: Instance::Primary,
            })
            .collect();
        let factories =
            ndsif_transport::create_routed_transport_factories(&recorders, &routes).unwrap();
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.register_chip(Chip::Okim6258, Instance::Primary, 4_000_000);
        builder.add_vgm_command(WaitSamples(220));
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x20,
                value: 0xc7,
            },
        );
        builder.add_vgm_command(WaitSamples(221));
        for (register, value) in (0x08..=0x0b).zip(8_000_000_u32.to_le_bytes()) {
            builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
        }
        builder.add_chip_write(
            Instance::Primary,
            Okim6258Spec {
                register: 0x0c,
                value: 1,
            },
        );
        builder.add_vgm_command(WaitSamples(220));
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x20,
                value: 0xc7,
            },
        );
        builder.add_vgm_command(WaitSamples(221));
        for (register, value) in (0x08..=0x0b).zip(4_000_000_u32.to_le_bytes()) {
            builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
        }
        builder.add_chip_write(
            Instance::Primary,
            Okim6258Spec {
                register: 0x0c,
                value: 2,
            },
        );
        builder.add_vgm_command(WaitSamples(220));
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
        let cancelled = AtomicBool::new(false);
        let mut frames = 0;
        for _ in 0..2 {
            let block = sequencer.render().unwrap().unwrap();
            frames += block.frames().len();
            workers.enqueue(&recorders, &cancelled).unwrap();
        }
        workers.prepare_audio(frames, 44_100, &cancelled).unwrap();
        assert!(started.recv_timeout(Duration::from_millis(30)).is_err());
        timeline.start(Instant::now()).unwrap();
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        while let Some(_) = sequencer.render().unwrap() {
            workers.enqueue(&recorders, &cancelled).unwrap();
        }
        workers.enqueue(&recorders, &cancelled).unwrap();
        let actual: Vec<_> = (0..5)
            .map(|_| received.recv_timeout(Duration::from_secs(1)).unwrap())
            .collect();
        assert_eq!(
            actual,
            [
                (9, "register"),
                (19, "clock8"),
                (45, "register"),
                (71, "clock4"),
                (91, "register")
            ]
        );
        workers.finish().unwrap();
        drop(sequencer);
        drop(recorders);
        lease.release().unwrap();
        release.send(()).unwrap();
        fake.join().unwrap();
        drop(port);
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

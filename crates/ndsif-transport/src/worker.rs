use std::cell::{Cell, RefCell};
use std::error::Error;
use std::fmt;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{NdsifCommandBatch, NdsifHostTimeline, NdsifRecordingTransport, NdsifSerialTransport};

/// A recorder shared with chip factories during rendering.
pub type NdsifRecorder = Rc<RefCell<NdsifRecordingTransport>>;

/// A device connection paired with its recording transport for one playback.
///
/// Clones refer to the same connection and recorder. Create a fresh pair for
/// each playback and drop it before reclaiming the connection.
#[derive(Clone)]
pub struct NdsifWorkerDevice {
    device: Arc<Mutex<NdsifSerialTransport>>,
    recorder: NdsifRecorder,
}

impl NdsifWorkerDevice {
    /// Snapshot the connection's capabilities into a bounded recorder.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection lock is poisoned.
    pub fn new(
        device: Arc<Mutex<NdsifSerialTransport>>,
        operation_limit: NonZeroUsize,
    ) -> Result<Self, NdsifWorkerError> {
        let recorder = {
            let connection = device
                .lock()
                .map_err(|_| NdsifWorkerError("NDSIF device lock poisoned".into()))?;
            Rc::new(RefCell::new(NdsifRecordingTransport::new(
                &connection,
                operation_limit,
            )))
        };
        Ok(Self { device, recorder })
    }

    /// Return the recorder used by the chip factories for this device.
    pub fn recorder(&self) -> NdsifRecorder {
        Rc::clone(&self.recorder)
    }
}

/// An error preparing or running device workers.
#[derive(Debug)]
pub struct NdsifWorkerError(String);

impl fmt::Display for NdsifWorkerError {
    /// Format the worker failure.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for NdsifWorkerError {}

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
    index: usize,
    recorder: NdsifRecorder,
    sender: mpsc::SyncSender<Work>,
    timing: DeviceTiming,
    dac_prebuffer: Option<DacPrebuffer>,
}

struct DacPrebuffer {
    capabilities: ndsif::DacCapabilities,
    usage: Cell<(usize, usize)>,
}

/// Bounded device workers that replay recorded batches against a shared timeline.
///
/// The workers retain their paired recorders on the calling thread and cannot
/// be sent to another thread. Device I/O runs on separate worker threads.
pub struct NdsifHardwareWorkers {
    senders: Vec<DeviceWorker>,
    threads: Vec<JoinHandle<Result<(), String>>>,
    errors: mpsc::Receiver<String>,
    timeline: Arc<NdsifHostTimeline>,
    joined: bool,
    prebuffer_prepared: Cell<bool>,
}

impl NdsifHardwareWorkers {
    /// Return whether any worker uses the device-timed YM2612 DAC profile.
    pub fn has_timed_dac(&self) -> bool {
        self.senders
            .iter()
            .any(|worker| worker.timing == DeviceTiming::Dac)
    }

    /// Start one worker per device, replaying each recorder's setup batch.
    ///
    /// Connections must be unique. Error messages identify devices by their
    /// index in `devices`.
    pub fn start(
        devices: &[NdsifWorkerDevice],
        timeline: Arc<NdsifHostTimeline>,
        queue_blocks: NonZeroUsize,
    ) -> Result<Self, NdsifWorkerError> {
        if devices.is_empty() {
            return Err(NdsifWorkerError(
                "NanoDrive workers require at least one device".into(),
            ));
        }
        for (index, device) in devices.iter().enumerate() {
            for other_device in &devices[..index] {
                if Arc::ptr_eq(&device.device, &other_device.device) {
                    return Err(NdsifWorkerError(
                        "NanoDrive device connections must be unique".into(),
                    ));
                }
            }
        }
        let (errors_tx, errors) = mpsc::channel();
        let (ready_tx, ready) = mpsc::channel();
        let mut workers = Self {
            senders: Vec::new(),
            threads: Vec::new(),
            errors,
            timeline,
            joined: false,
            prebuffer_prepared: Cell::new(false),
        };
        for (index, device) in devices.iter().enumerate() {
            let setup = device
                .recorder
                .try_borrow_mut()
                .map_err(|_| {
                    NdsifWorkerError(format!("NanoDrive recorder {index} is already borrowed"))
                })?
                .take_batch()
                .map_err(|error| NdsifWorkerError(error.to_string()))?;
            let dac_capabilities =
                if !setup.configures_audio_stream() && setup.configures_primary_ym2612() {
                    device
                        .device
                        .lock()
                        .map_err(|_| {
                            NdsifWorkerError(format!("NanoDrive device {index} worker failed"))
                        })?
                        .timed_dac_capabilities()
                } else {
                    None
                };
            let timing = if setup.configures_audio_stream() {
                DeviceTiming::Audio
            } else if dac_capabilities.is_some() {
                DeviceTiming::Dac
            } else {
                DeviceTiming::Host
            };
            let dac_prebuffer = dac_capabilities.map(|capabilities| DacPrebuffer {
                usage: Cell::new(setup.timed_dac_prebuffer_usage(capabilities.ticks_per_second)),
                capabilities,
            });
            let (sender, receiver) = mpsc::sync_channel::<Work>(queue_blocks.get());
            let connection = Arc::clone(&device.device);
            let timeline = Arc::clone(&workers.timeline);
            let ready_tx = ready_tx.clone();
            let errors_tx = errors_tx.clone();
            let handle = thread::Builder::new()
                .name(format!("nanodrive-{index}"))
                .spawn(move || {
                    let setup_result = (|| {
                        let mut device = connection
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
                            DeviceTiming::Audio => device
                                .defer_audio_stream_start_with_cancellation(Arc::clone(
                                    &timeline,
                                ))?,
                            DeviceTiming::Dac => device
                                .defer_timed_dac_start_with_cancellation(Arc::clone(&timeline))?,
                            DeviceTiming::Host => {}
                        }
                        Ok::<(), String>(())
                    })();
                    if let Err(error) = &setup_result {
                        let _ = ready_tx.send(Err(format!("NanoDrive device {index}: {error}")));
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
                        let idle_poll = if timing == DeviceTiming::Dac { 5 } else { 10 };
                        let result = match receiver.recv_timeout(Duration::from_millis(idle_poll)) {
                            Ok(Work::Batch(batch)) => connection
                                .lock()
                                .map_err(|_| "device lock poisoned".to_owned())
                                .and_then(|mut device| {
                                    batch
                                        .replay(&mut *device)
                                        .map_err(|error| error.to_string())
                                }),
                            Ok(Work::PrepareAudio {
                                frames,
                                rate,
                                ready,
                            }) => {
                                let result = connection
                                    .lock()
                                    .map_err(|_| "device lock poisoned".to_owned())
                                    .and_then(|mut device| match timing {
                                        DeviceTiming::Audio => device
                                            .prepare_audio_stream_start_for_frames(frames, rate),
                                        DeviceTiming::Dac => {
                                            device.prepare_timed_dac_start_for_frames(frames, rate)
                                        }
                                        DeviceTiming::Host => {
                                            unreachable!("host workers do not prepare streams")
                                        }
                                    });
                                match result {
                                    Ok(()) => {
                                        if ready.send(Ok(())).is_err() {
                                            Err("playback worker stopped during audio preparation"
                                                .into())
                                        } else {
                                            timeline.wait_until_presentation().and_then(|()| {
                                                if timeline.is_cancelled() {
                                                    return Err(
                                                        "NDSIF host pacing was cancelled".into()
                                                    );
                                                }
                                                connection
                                                    .lock()
                                                    .map_err(|_| "device lock poisoned".to_owned())
                                                    .and_then(|mut device| {
                                                        device.synchronize_output_volume()?;
                                                        match timing {
                                                            DeviceTiming::Audio => {
                                                                device.start_prepared_audio_stream()
                                                            }
                                                            DeviceTiming::Dac => {
                                                                device.start_prepared_timed_dac()
                                                            }
                                                            DeviceTiming::Host => unreachable!(
                                                                "host workers do not start streams"
                                                            ),
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
                            Err(mpsc::RecvTimeoutError::Timeout) => connection
                                .lock()
                                .map_err(|_| "device lock poisoned".to_owned())
                                .and_then(|mut device| {
                                    device.update_output_fade()?;
                                    if timing == DeviceTiming::Dac {
                                        device.service_timed_dac_host_events(false)?;
                                    }
                                    Ok(())
                                }),
                            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                        };
                        if let Err(error) = result {
                            if timeline.is_cancelled() {
                                return Ok(());
                            }
                            let message = format!("NanoDrive device {index}: {error}");
                            let _ = errors_tx.send(message.clone());
                            timeline.cancel();
                            return Err(message);
                        }
                    }
                })
                .map_err(|error| {
                    NdsifWorkerError(format!("start NanoDrive device worker: {error}"))
                })?;
            workers.senders.push(DeviceWorker {
                index,
                recorder: device.recorder(),
                sender,
                timing,
                dac_prebuffer,
            });
            workers.threads.push(handle);
        }
        drop(ready_tx);
        drop(errors_tx);
        for _ in 0..workers.threads.len() {
            ready
                .recv()
                .map_err(|_| NdsifWorkerError("NanoDrive worker stopped during setup".into()))?
                .map_err(NdsifWorkerError)?;
        }
        Ok(workers)
    }

    /// Report a device worker failure, if one has occurred.
    pub fn check(&self) -> Result<(), NdsifWorkerError> {
        match self.errors.try_recv() {
            Ok(error) => Err(NdsifWorkerError(error)),
            Err(mpsc::TryRecvError::Empty) => Ok(()),
            Err(mpsc::TryRecvError::Disconnected) => Err(NdsifWorkerError(
                "NanoDrive workers stopped unexpectedly".into(),
            )),
        }
    }

    /// Drain each paired recorder and enqueue its batch for the same device.
    pub fn enqueue(&self, cancelled: &AtomicBool) -> Result<(), NdsifWorkerError> {
        for worker in &self.senders {
            let batch = worker
                .recorder
                .try_borrow_mut()
                .map_err(|_| NdsifWorkerError("NanoDrive recorder is already borrowed".into()))?
                .take_batch()
                .map_err(|error| NdsifWorkerError(error.to_string()))?;
            if !batch.is_empty() {
                self.send(worker, Work::Batch(batch), cancelled)?;
            }
        }
        Ok(())
    }

    /// Enqueue a startup batch, preparing streams first if a DAC queue would fill.
    ///
    /// `buffered_frames` is the PCM span already enqueued to the workers,
    /// excluding the current recorder batches. Returns `true` when preparation
    /// was performed; thereafter use [`Self::enqueue`] for subsequent batches.
    ///
    /// # Errors
    ///
    /// Returns an error if a recorder cannot be drained or preparation fails.
    /// The first batch is always submitted; an oversized batch is reported by
    /// the device transport.
    pub fn enqueue_prebuffer(
        &self,
        buffered_frames: usize,
        rate: u32,
        cancelled: &AtomicBool,
    ) -> Result<bool, NdsifWorkerError> {
        if self.prebuffer_prepared.get() {
            return Err(NdsifWorkerError(
                "NanoDrive prebuffer is already prepared".into(),
            ));
        }
        let mut estimates = Vec::with_capacity(self.senders.len());
        let mut would_overflow = false;
        for worker in &self.senders {
            let estimate = if let Some(buffer) = &worker.dac_prebuffer {
                let additional = worker
                    .recorder
                    .try_borrow()
                    .map_err(|_| NdsifWorkerError("NanoDrive recorder is already borrowed".into()))?
                    .timed_dac_prebuffer_usage(buffer.capabilities.ticks_per_second);
                let (records, events) = buffer.usage.get();
                let total = (
                    records.saturating_add(additional.0),
                    events.saturating_add(additional.1),
                );
                would_overflow |= total.0 > buffer.capabilities.record_capacity as usize
                    || total.1 > usize::from(buffer.capabilities.event_capacity);
                Some(total)
            } else {
                None
            };
            estimates.push(estimate);
        }
        let prepare = would_overflow && buffered_frames > 0;
        if prepare {
            self.prepare_audio(buffered_frames, rate, cancelled)?;
            self.prebuffer_prepared.set(true);
        }
        self.enqueue(cancelled)?;
        if !prepare {
            for (worker, estimate) in self.senders.iter().zip(estimates) {
                if let (Some(buffer), Some(usage)) = (&worker.dac_prebuffer, estimate) {
                    buffer.usage.set(usage);
                }
            }
        }
        Ok(prepare)
    }

    /// Retry a bounded-channel send while observing cancellation and failures.
    fn send(
        &self,
        worker: &DeviceWorker,
        mut work: Work,
        cancelled: &AtomicBool,
    ) -> Result<(), NdsifWorkerError> {
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
                    return Err(NdsifWorkerError(format!(
                        "NanoDrive device {} worker disconnected",
                        worker.index
                    )));
                }
            }
        }
    }

    /// Wait until each device-timed stream has accepted the initial PCM span.
    pub fn prepare_audio(
        &self,
        frames: usize,
        rate: u32,
        cancelled: &AtomicBool,
    ) -> Result<(), NdsifWorkerError> {
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
                        result.map_err(|error| {
                            NdsifWorkerError(format!("NanoDrive device {}: {error}", worker.index))
                        })?;
                        break;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => self.check()?,
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        self.check()?;
                        return Err(NdsifWorkerError(format!(
                            "NanoDrive device {} stopped during audio preparation",
                            worker.index
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Finish queued work and join all workers.
    ///
    /// Start or cancel a pending timeline before calling this method, otherwise
    /// workers waiting for presentation may not finish. Dropping the workers
    /// instead cancels the timeline before joining them.
    pub fn finish(mut self) -> Result<(), NdsifWorkerError> {
        self.senders.clear();
        let result = self.join();
        self.joined = true;
        result
    }

    /// Join all worker threads and combine their failures.
    fn join(&mut self) -> Result<(), NdsifWorkerError> {
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
            Err(NdsifWorkerError(failures.join("; ")))
        }
    }
}

impl Drop for NdsifHardwareWorkers {
    /// Cancel pending work and join worker threads unless already finished.
    fn drop(&mut self) {
        if !self.joined {
            self.timeline.cancel();
            self.senders.clear();
            let _ = self.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_reject_an_empty_device_list() {
        let result = NdsifHardwareWorkers::start(
            &[],
            Arc::new(NdsifHostTimeline::pending()),
            NonZeroUsize::new(1).unwrap(),
        );
        assert!(matches!(result, Err(error) if error.to_string().contains("at least one device")));
    }
}

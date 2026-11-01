//! Audio stream bookkeeping and device operations.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use chipstream::chip::NdsifAdpcmEvent;
use ndsif::{AudioStreamState, Command, Divider, Reply, Response};

use super::{AUDIO_STREAM_ID, NdsifSerialTransport, STATUS_POLL_INTERVAL};

const AUDIO_PREBUFFER_SECONDS: u64 = 2;

pub(super) struct AudioStream {
    pub(super) instance: u8,
    pub(super) state: AudioStreamState,
    pub(super) prebuffer_bytes: u32,
    pub(super) queued_events: VecDeque<(u32, u32)>,
    pub(super) queued_event_count: u32,
    pub(super) last_status_check: Option<Instant>,
}

impl AudioStream {
    /// Record events queued at an audio byte position.
    pub(super) fn record_queued_events(
        &mut self,
        position: u32,
        count: usize,
    ) -> Result<(), String> {
        let count = u32::try_from(count).map_err(|_| "NDSIF event count overflow".to_owned())?;
        self.queued_event_count = self
            .queued_event_count
            .checked_add(count)
            .ok_or_else(|| "NDSIF queued event count overflow".to_owned())?;
        self.queued_events.push_back((position, count));
        Ok(())
    }

    /// Remove queued-event accounting for positions the device has played.
    fn prune_queued_events(&mut self) {
        let played = self.state.played().get();
        while self
            .queued_events
            .front()
            .is_some_and(|(position, _)| *position <= played)
        {
            let (_, count) = self.queued_events.pop_front().expect("front was checked");
            self.queued_event_count = self.queued_event_count.saturating_sub(count);
        }
    }

    /// Validate audio status and update local progress and queue accounting.
    pub(super) fn update_progress(&mut self, status: ndsif::AudioStatus) -> Result<(), String> {
        self.state
            .update_status(status)
            .map_err(|error| format!("invalid NDSIF audio status: {error}"))?;
        self.prune_queued_events();
        Ok(())
    }
}

/// Compute the prebuffer target while reserving room for another data frame.
pub(super) fn audio_prebuffer_bytes(clock_hz: u32, divider: Divider, capacity: u32) -> u32 {
    let safe_capacity = capacity
        .saturating_sub(ndsif::MAX_AUDIO_DATA_BYTES as u32)
        .max(1);
    (u64::from(clock_hz) * AUDIO_PREBUFFER_SECONDS / (u64::from(divider as u32) * 2))
        .min(u64::from(safe_capacity)) as u32
}

impl NdsifSerialTransport {
    /// Send a contiguous ADPCM data batch and advance the accepted position.
    pub(super) fn send_data(&mut self, events: &[NdsifAdpcmEvent]) -> Result<(), String> {
        if events.is_empty() {
            return Ok(());
        }
        let stream = self
            .audio_stream
            .as_ref()
            .ok_or_else(|| "NDSIF ADPCM data has no configured stream".to_owned())?;
        let instance = stream.instance;
        let position = events[0].position;
        if position != stream.state.accepted() {
            return Err(format!(
                "NDSIF expected ADPCM position {}, got {}",
                stream.state.accepted().get(),
                position.get()
            ));
        }
        for (index, event) in events.iter().enumerate() {
            let expected = position
                .checked_advance(index)
                .map_err(|error| error.to_string())?;
            if event.instance != instance || event.position != expected {
                return Err("NDSIF ADPCM byte positions are not contiguous".to_owned());
            }
        }
        let data = events.iter().map(|event| event.value).collect::<Vec<_>>();
        self.ensure_audio_data_capacity(data.len())?;
        self.request(Command::AudioData {
            stream_id: AUDIO_STREAM_ID,
            instance,
            position,
            adpcm: &data,
        })?;
        let stream = self
            .audio_stream
            .as_mut()
            .expect("stream was checked above");
        stream
            .state
            .record_data_accepted(position, data.len())
            .map_err(|error| format!("invalid accepted NDSIF audio position: {error}"))?;
        Ok(())
    }

    /// Wait for enough audio data capacity before sending a batch.
    fn ensure_audio_data_capacity(&mut self, incoming_bytes: usize) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let Some((instance, accepted, played, started)) =
                self.audio_stream.as_ref().map(|stream| {
                    (
                        stream.instance,
                        stream.state.accepted().get(),
                        stream.state.played().get(),
                        stream.state.started(),
                    )
                })
            else {
                return Err("NDSIF ADPCM data has no configured stream".to_owned());
            };
            let pending = accepted.saturating_sub(played);
            if u64::from(pending) + incoming_bytes as u64 <= u64::from(self.data_capacity) {
                return Ok(());
            }
            if !started {
                return Err(format!(
                    "NDSIF prebuffer exceeds device capacity (pending={pending}, incoming={incoming_bytes}, capacity={})",
                    self.data_capacity
                ));
            }
            let status = self.query_audio_status(instance)?;
            self.update_audio_progress(status)?;
            let pending = accepted.saturating_sub(status.played);
            if u64::from(pending) + incoming_bytes as u64 <= u64::from(self.data_capacity) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for NDSIF audio buffer capacity (pending={pending}, incoming={incoming_bytes}, capacity={})",
                    self.data_capacity
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Query and decode the device's audio-stream status.
    fn query_audio_status(&mut self, instance: u8) -> Result<ndsif::AudioStatus, String> {
        let frame = self.request(Command::AudioStatus {
            stream_id: AUDIO_STREAM_ID,
            instance,
        })?;
        let response = Response::decode(&frame).map_err(|error| error.to_string())?;
        match response.reply {
            Reply::AudioStatus(status) if !status.flags.fault() => Ok(status),
            Reply::AudioStatus(status) => Err(format!(
                "NDSIF audio stream faulted (accepted={}, played={}, pending={}, underflows={}, overflows={}, rejected={}, max_pending={}, flags={:?}, late_events={}, max_event_lag={})",
                status.accepted,
                status.played,
                status.pending,
                status.underflows,
                status.overflows,
                status.rejected,
                status.max_pending,
                status.flags,
                status.late_events,
                status.max_event_lag
            )),
            _ => Err("NDSIF AUDIO_STATUS returned an unexpected response".to_owned()),
        }
    }

    /// Wait for space in the audio stream's event queue.
    pub(super) fn ensure_event_capacity(&mut self, incoming_events: usize) -> Result<(), String> {
        let incoming_events =
            u32::try_from(incoming_events).map_err(|_| "NDSIF event count overflow".to_owned())?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(stream) = self.audio_stream.as_mut() {
                stream.prune_queued_events();
            }
            let queued = self
                .audio_stream
                .as_ref()
                .map_or(0, |stream| stream.queued_event_count);
            if queued + incoming_events <= u32::from(self.event_capacity) {
                return Ok(());
            }
            let Some((instance, accepted, started)) = self.audio_stream.as_ref().map(|stream| {
                (
                    stream.instance,
                    stream.state.accepted().get(),
                    stream.state.started(),
                )
            }) else {
                return Err("NDSIF audio event has no configured stream".to_owned());
            };
            if !started {
                if accepted == 0 {
                    return Err(
                        "NDSIF event capacity exhausted before audio data was queued".to_owned(),
                    );
                }
                self.start_stream_if_ready(true)?;
            }
            let status = self.query_audio_status(instance)?;
            self.update_audio_progress(status)?;
            let queued = self
                .audio_stream
                .as_ref()
                .map_or(0, |stream| stream.queued_event_count);
            if queued + incoming_events <= u32::from(self.event_capacity) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for NDSIF event capacity (queued={}, incoming={}, capacity={})",
                    queued, incoming_events, self.event_capacity
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Validate audio status and update local progress and queue accounting.
    fn update_audio_progress(&mut self, status: ndsif::AudioStatus) -> Result<(), String> {
        if let Some(stream) = self.audio_stream.as_mut() {
            stream.update_progress(status)?;
        }
        Ok(())
    }

    /// Start the audio stream after prebuffering or when explicitly forced.
    pub(super) fn start_stream_if_ready(&mut self, force: bool) -> Result<(), String> {
        let Some((instance, clock, divider)) = self.audio_stream.as_ref().and_then(|stream| {
            stream
                .state
                .should_start(stream.prebuffer_bytes, force)
                .then_some((
                    stream.instance,
                    stream.state.timing().clock(),
                    stream.state.timing().divider(),
                ))
        }) else {
            return Ok(());
        };
        self.request(Command::Ping(b"chipstream-start"))?;
        self.request(Command::AudioStart {
            stream_id: AUDIO_STREAM_ID,
            instance,
            clock,
            divider,
        })?;
        let status = self.query_audio_status(instance)?;
        if !status.flags.running() {
            return Err(format!(
                "NDSIF audio stream did not enter the running state (accepted={}, played={}, pending={}, underflows={}, overflows={}, rejected={}, max_pending={}, flags={:?})",
                status.accepted,
                status.played,
                status.pending,
                status.underflows,
                status.overflows,
                status.rejected,
                status.max_pending,
                status.flags
            ));
        }
        self.update_audio_progress(status)?;
        self.audio_stream
            .as_mut()
            .expect("audio stream was checked above")
            .last_status_check = Some(Instant::now());
        Ok(())
    }

    /// Poll and validate audio progress when the status interval has elapsed.
    pub(super) fn check_status_if_due(&mut self) -> Result<(), String> {
        let Some(instance) = self.audio_stream.as_ref().and_then(|stream| {
            (stream.state.started()
                && stream
                    .last_status_check
                    .is_some_and(|last| last.elapsed() >= STATUS_POLL_INTERVAL))
            .then_some(stream.instance)
        }) else {
            return Ok(());
        };
        let status = self.query_audio_status(instance)?;
        self.update_audio_progress(status)?;
        self.audio_stream
            .as_mut()
            .expect("audio stream was checked above")
            .last_status_check = Some(Instant::now());
        Ok(())
    }
}

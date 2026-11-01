//! Timed DAC stream bookkeeping and device operations.

use std::time::{Duration, Instant};

use chipstream::chip::{NdsifRegisterEvent, NdsifTransport};
use ndsif::{
    Command, DAC_PROFILE_YM2612, DacCapabilities, DacRecord, DacRecordKind, DacStatus,
    MAX_DAC_EVENT_BYTES, MAX_DAC_RECORDS, Reply, Response, WriteId,
};

use super::{
    DAC_DRAIN_TIMEOUT, NdsifSerialTransport, NdsifTransportError, PendingEvent,
    STATUS_POLL_INTERVAL,
};

const DAC_PREBUFFER_MILLISECONDS: u64 = 80;

/// Convert an output frame to a bounded device tick position.
pub(super) fn ticks_for_frame(
    sample_frame: usize,
    sample_rate: u32,
    ticks_per_second: u32,
) -> Result<u64, String> {
    if sample_rate == 0 || ticks_per_second == 0 {
        return Err("NDSIF timed DAC clock or sample rate is zero".to_owned());
    }
    let ticks = (sample_frame as u128) * u128::from(ticks_per_second) / u128::from(sample_rate);
    let ticks =
        u64::try_from(ticks).map_err(|_| "NDSIF timed DAC tick position overflow".to_owned())?;
    if ticks > u64::MAX / 1_000_000 {
        return Err("NDSIF timed DAC tick position exceeds device limits".to_owned());
    }
    Ok(ticks)
}

/// Append bounded wait records to advance the DAC timeline to a target tick.
pub(super) fn append_dac_waits(
    records: &mut Vec<DacRecord>,
    current_tick: &mut u64,
    target_tick: u64,
) -> Result<(), String> {
    if target_tick < *current_tick {
        return Err(format!(
            "NDSIF timed DAC event moves backward (current={}, target={target_tick})",
            *current_tick
        ));
    }
    while *current_tick < target_tick {
        let wait_ticks = (target_tick - *current_tick).min(u64::from(u16::MAX)) as u16;
        records.push(DacRecord {
            kind: DacRecordKind::Wait,
            value: 0,
            wait_ticks,
        });
        *current_tick = current_tick
            .checked_add(u64::from(wait_ticks))
            .ok_or_else(|| "NDSIF timed DAC tick position overflow".to_owned())?;
    }
    Ok(())
}

/// Identify YM2612 writes represented directly by timed DAC records.
pub(super) fn is_timed_dac_data_register(event: &NdsifRegisterEvent) -> bool {
    event.write_id == WriteId::YM2612
        && event.instance == 0
        && event.page == 0
        && matches!(event.write.address, 0x2a | 0x2b)
}

pub(super) struct TimedDacStream {
    pub(super) capabilities: DacCapabilities,
    pub(super) output_rate: Option<u32>,
    pub(super) accepted_tick: u64,
    pub(super) next_sequence: u32,
    pub(super) queued_records: u32,
    pub(super) queued_events: u32,
    pub(super) last_event_tick: u64,
    pub(super) started: bool,
    pub(super) sealed: bool,
    pub(super) finished: bool,
    pub(super) last_status_check: Option<Instant>,
}

impl TimedDacStream {
    /// Initialize counters for a discovered timed DAC profile.
    pub(super) fn new(capabilities: DacCapabilities) -> Self {
        Self {
            capabilities,
            output_rate: None,
            accepted_tick: 0,
            next_sequence: 0,
            queued_records: 0,
            queued_events: 0,
            last_event_tick: 0,
            started: false,
            sealed: false,
            finished: false,
            last_status_check: None,
        }
    }

    /// Compute the target DAC prebuffer lead in device ticks.
    pub(super) fn minimum_lead_ticks(&self) -> u32 {
        (u64::from(self.capabilities.ticks_per_second) * DAC_PREBUFFER_MILLISECONDS / 1000)
            .min(u64::from(u32::MAX)) as u32
    }

    /// Validate device progress against the host timeline and queue limits.
    pub(super) fn update_progress(&mut self, status: DacStatus) -> Result<(), String> {
        if status.state == 2 && !self.sealed {
            return Err("NDSIF timed DAC stream ended before the host sealed it".to_owned());
        }
        if self.started && status.state == 0 {
            return Err("NDSIF timed DAC stream stopped unexpectedly".to_owned());
        }
        if status.accepted != self.next_sequence || status.accepted_tick != self.accepted_tick {
            return Err(format!(
                "NDSIF timed DAC position mismatch (host sequence={}, device sequence={}, host tick={}, device tick={})",
                self.next_sequence, status.accepted, self.accepted_tick, status.accepted_tick
            ));
        }
        if status.accepted.checked_sub(status.consumed) != Some(status.queued)
            || status.queued.checked_add(status.free) != Some(self.capabilities.record_capacity)
            || status.event_queued > u32::from(self.capabilities.event_capacity)
        {
            return Err("NDSIF timed DAC status counters are inconsistent".to_owned());
        }
        self.queued_records = status.queued;
        self.queued_events = status.event_queued;
        Ok(())
    }
}

impl NdsifSerialTransport {
    /// Convert due events into timed DAC records and event batches.
    pub(super) fn flush_timed_dac_until(
        &mut self,
        events: Vec<PendingEvent>,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        let Some(stream) = self.timed_dac_stream.as_mut() else {
            return Err("NDSIF timed DAC stream is not configured".to_owned());
        };
        if stream
            .output_rate
            .is_some_and(|configured_rate| configured_rate != output_rate)
        {
            return Err("NDSIF timed DAC output rate changed during playback".to_owned());
        }
        stream.output_rate = Some(output_rate);
        let ticks_per_second = stream.capabilities.ticks_per_second;
        let mut record_tick = stream.accepted_tick;
        let target_tick = ticks_for_frame(sample_frame, output_rate, ticks_per_second)?;
        if target_tick < record_tick {
            return Err(format!(
                "NDSIF timed DAC timeline moved backward (accepted={}, target={target_tick})",
                record_tick
            ));
        }

        let due = Self::order_events(events, None)?;
        let mut records = Vec::new();
        let mut index = 0;
        while index < due.len() {
            match &due[index] {
                PendingEvent::Register(first) if is_timed_dac_data_register(first) => {
                    if first.sample_rate != output_rate {
                        return Err(
                            "NDSIF timed DAC event rate differs from output rate".to_owned()
                        );
                    }
                    let tick = ticks_for_frame(first.sample_frame, output_rate, ticks_per_second)?;
                    append_dac_waits(&mut records, &mut record_tick, tick)?;
                    records.push(DacRecord {
                        kind: if first.write.address == 0x2a {
                            DacRecordKind::Dac
                        } else {
                            DacRecordKind::Select
                        },
                        value: if first.write.address == 0x2b {
                            first.write.value & 0x80
                        } else {
                            first.write.value
                        },
                        wait_ticks: 0,
                    });
                    index += 1;
                }
                PendingEvent::Register(first) => {
                    if first.sample_rate != output_rate {
                        return Err(
                            "NDSIF timed DAC event rate differs from output rate".to_owned()
                        );
                    }
                    let tick = ticks_for_frame(first.sample_frame, output_rate, ticks_per_second)?;
                    let mut writes = Vec::new();
                    let mut end = index;
                    while end < due.len() && writes.len() < MAX_DAC_EVENT_BYTES {
                        let PendingEvent::Register(next) = &due[end] else {
                            break;
                        };
                        if is_timed_dac_data_register(next)
                            || next.write_id != first.write_id
                            || next.instance != first.instance
                            || next.page != first.page
                            || next.sample_rate != output_rate
                            || ticks_for_frame(next.sample_frame, output_rate, ticks_per_second)?
                                != tick
                        {
                            break;
                        }
                        writes.extend([next.write.address, next.write.value]);
                        end += 1;
                    }
                    self.send_timed_dac_register_event(
                        tick,
                        first.write_id,
                        first.instance,
                        first.page,
                        &writes,
                    )?;
                    index = end;
                }
                PendingEvent::Psg(first) => {
                    if first.sample_rate != output_rate {
                        return Err(
                            "NDSIF timed DAC event rate differs from output rate".to_owned()
                        );
                    }
                    let tick = ticks_for_frame(first.sample_frame, output_rate, ticks_per_second)?;
                    let mut writes = Vec::new();
                    let mut end = index;
                    while end < due.len() && writes.len() < MAX_DAC_EVENT_BYTES {
                        let PendingEvent::Psg(next) = &due[end] else {
                            break;
                        };
                        if next.write_id != first.write_id
                            || next.instance != first.instance
                            || next.page != first.page
                            || next.sample_rate != output_rate
                            || ticks_for_frame(next.sample_frame, output_rate, ticks_per_second)?
                                != tick
                        {
                            break;
                        }
                        writes.push(next.value);
                        end += 1;
                    }
                    self.send_timed_dac_register_event(
                        tick,
                        first.write_id,
                        first.instance,
                        first.page,
                        &writes,
                    )?;
                    index = end;
                }
                PendingEvent::Data(_) | PendingEvent::Control(_) => {
                    return Err(
                        "NDSIF timed DAC profile cannot share an OKIM6258 audio stream".to_owned(),
                    );
                }
            }
        }
        append_dac_waits(&mut records, &mut record_tick, target_tick)?;
        self.send_timed_dac_records(&records)
    }

    /// Query timed DAC status using the playback error boundary.
    fn query_timed_dac_status(&mut self) -> Result<DacStatus, String> {
        self.query_timed_dac_status_typed()
            .map_err(|error| error.to_string())
    }

    /// Query and decode timed DAC status with typed errors.
    pub(super) fn query_timed_dac_status_typed(
        &mut self,
    ) -> Result<DacStatus, NdsifTransportError> {
        let frame = self.request_typed(Command::DacStatus {
            profile: DAC_PROFILE_YM2612,
        })?;
        let response =
            Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid NDSIF DAC_STATUS response",
                source,
            })?;
        match response.reply {
            Reply::DacStatus(status) if status.profile == DAC_PROFILE_YM2612 => Ok(status),
            Reply::DacStatus(status) => Err(NdsifTransportError::UnexpectedDacProfile {
                profile: status.profile,
            }),
            _ => Err(NdsifTransportError::UnexpectedResponse {
                operation: "DAC_STATUS",
            }),
        }
    }

    /// Validate device DAC progress and update local queue counters.
    fn update_timed_dac_progress(&mut self, status: DacStatus) -> Result<(), String> {
        if status.fault != 0 || status.state == 3 {
            return Err(format!(
                "NDSIF timed DAC stream faulted (state={}, fault={}, accepted={}, consumed={}, queued={}, event_queued={}, underflows={}, overflows={}, rejected={}, late_dac={}, max_late_us={})",
                status.state,
                status.fault,
                status.accepted,
                status.consumed,
                status.queued,
                status.event_queued,
                status.underflows,
                status.overflows,
                status.rejected,
                status.late_dac,
                status.max_late_us
            ));
        }
        self.timed_dac_stream
            .as_mut()
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?
            .update_progress(status)
    }

    /// Wait for room in the device's timed DAC record queue.
    fn ensure_timed_dac_record_capacity(&mut self, incoming: usize) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let Some((capacity, queued, started)) = self.timed_dac_stream.as_ref().map(|stream| {
                (
                    stream.capabilities.record_capacity,
                    stream.queued_records,
                    stream.started,
                )
            }) else {
                return Err("NDSIF timed DAC stream is not configured".to_owned());
            };
            if u64::from(queued) + incoming as u64 <= u64::from(capacity) {
                return Ok(());
            }
            if !started {
                return Err(format!(
                    "NDSIF timed DAC prebuffer exceeds device capacity (queued={queued}, incoming={incoming}, capacity={capacity})"
                ));
            }
            let status = self.query_timed_dac_status()?;
            self.update_timed_dac_progress(status)?;
            let queued = self
                .timed_dac_stream
                .as_ref()
                .expect("stream was checked above")
                .queued_records;
            if u64::from(queued) + incoming as u64 <= u64::from(capacity) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for NDSIF DAC record capacity (queued={queued}, incoming={incoming}, capacity={capacity})"
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait for room in the device's timed DAC event queue.
    fn ensure_timed_dac_event_capacity(&mut self, incoming: usize) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let Some((capacity, queued, started)) = self.timed_dac_stream.as_ref().map(|stream| {
                (
                    stream.capabilities.event_capacity,
                    stream.queued_events,
                    stream.started,
                )
            }) else {
                return Err("NDSIF timed DAC stream is not configured".to_owned());
            };
            if u64::from(queued) + incoming as u64 <= u64::from(capacity) {
                return Ok(());
            }
            if !started {
                return Err(format!(
                    "NDSIF timed DAC prebuffer exceeds event capacity (queued={queued}, incoming={incoming}, capacity={capacity})"
                ));
            }
            let status = self.query_timed_dac_status()?;
            self.update_timed_dac_progress(status)?;
            let queued = self
                .timed_dac_stream
                .as_ref()
                .expect("stream was checked above")
                .queued_events;
            if u64::from(queued) + incoming as u64 <= u64::from(capacity) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out waiting for NDSIF DAC event capacity (queued={queued}, incoming={incoming}, capacity={capacity})"
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Send DAC records in bounded batches and update the accepted timeline.
    fn send_timed_dac_records(&mut self, records: &[DacRecord]) -> Result<(), String> {
        let maximum = self
            .timed_dac_stream
            .as_ref()
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?
            .capabilities
            .max_records
            .min(MAX_DAC_RECORDS as u8) as usize;
        if maximum == 0 {
            return Err("NDSIF timed DAC profile advertises no record capacity".to_owned());
        }
        for batch in records.chunks(maximum) {
            self.ensure_timed_dac_record_capacity(batch.len())?;
            let (sequence, position) = self
                .timed_dac_stream
                .as_ref()
                .map(|stream| (stream.next_sequence, stream.accepted_tick))
                .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?;
            self.request(Command::DacData {
                profile: DAC_PROFILE_YM2612,
                write_id: WriteId::YM2612,
                instance: 0,
                sequence,
                position,
                records: batch,
            })?;
            let mut end_tick = position;
            for record in batch {
                end_tick = end_tick
                    .checked_add(u64::from(record.wait_ticks))
                    .ok_or_else(|| "NDSIF DAC tick position overflow".to_owned())?;
            }
            let stream = self
                .timed_dac_stream
                .as_mut()
                .expect("stream was checked above");
            stream.next_sequence = stream
                .next_sequence
                .checked_add(batch.len() as u32)
                .ok_or_else(|| "NDSIF DAC record sequence overflow".to_owned())?;
            stream.accepted_tick = end_tick;
            stream.queued_records = stream
                .queued_records
                .checked_add(batch.len() as u32)
                .ok_or_else(|| "NDSIF queued DAC record count overflow".to_owned())?;
        }
        Ok(())
    }

    /// Send a register or PSG event on the timed DAC timeline.
    fn send_timed_dac_register_event(
        &mut self,
        tick: u64,
        write_id: WriteId,
        instance: u8,
        page: u8,
        writes: &[u8],
    ) -> Result<(), String> {
        let count = if matches!(write_id.get(), 0x50 | 0x30) {
            writes.len()
        } else {
            writes.len() / 2
        };
        let (accepted_tick, last_event_tick) = self
            .timed_dac_stream
            .as_ref()
            .map(|stream| (stream.accepted_tick, stream.last_event_tick))
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?;
        if tick < accepted_tick || tick < last_event_tick {
            return Err(format!(
                "NDSIF timed DAC event is behind the accepted timeline (event={tick}, data={accepted_tick}, previous_event={last_event_tick})"
            ));
        }
        self.ensure_timed_dac_event_capacity(count)?;
        self.request(Command::DacRegisterEvent {
            profile: DAC_PROFILE_YM2612,
            tick,
            write_id,
            instance,
            page,
            writes,
        })?;
        let stream = self
            .timed_dac_stream
            .as_mut()
            .expect("stream was checked above");
        stream.last_event_tick = tick;
        stream.queued_events = stream
            .queued_events
            .checked_add(count as u32)
            .ok_or_else(|| "NDSIF queued DAC event count overflow".to_owned())?;
        Ok(())
    }

    /// Start the DAC profile when sufficiently buffered or explicitly forced.
    pub(super) fn start_timed_dac_if_ready(&mut self, force: bool) -> Result<(), String> {
        let Some((ready, minimum_lead_ticks)) = self.timed_dac_stream.as_ref().map(|stream| {
            (
                !stream.started
                    && stream.next_sequence > 0
                    && (force || stream.accepted_tick >= u64::from(stream.minimum_lead_ticks())),
                stream.minimum_lead_ticks(),
            )
        }) else {
            return Ok(());
        };
        if !ready {
            return Ok(());
        }
        self.request(Command::Ping(b"chipstream-dac-start"))?;
        self.request(Command::DacStart {
            profile: DAC_PROFILE_YM2612,
            minimum_lead_ticks,
        })?;
        let status = self.query_timed_dac_status()?;
        if !matches!(status.state, 1 | 2) {
            return Err(format!(
                "NDSIF timed DAC stream did not start (state={}, fault={}, accepted={}, consumed={}, queued={}, accepted_tick={})",
                status.state,
                status.fault,
                status.accepted,
                status.consumed,
                status.queued,
                status.accepted_tick
            ));
        }
        self.update_timed_dac_progress(status)?;
        let stream = self
            .timed_dac_stream
            .as_mut()
            .expect("stream was checked above");
        stream.started = true;
        stream.last_status_check = Some(Instant::now());
        Ok(())
    }

    /// Poll and validate DAC progress when the status interval has elapsed.
    pub(super) fn check_timed_dac_status_if_due(&mut self) -> Result<(), String> {
        let due = self.timed_dac_stream.as_ref().is_some_and(|stream| {
            stream.started
                && stream
                    .last_status_check
                    .is_some_and(|last| last.elapsed() >= STATUS_POLL_INTERVAL)
        });
        if due {
            let status = self.query_timed_dac_status()?;
            self.update_timed_dac_progress(status)?;
            self.timed_dac_stream
                .as_mut()
                .expect("stream was checked above")
                .last_status_check = Some(Instant::now());
        }
        Ok(())
    }

    /// Seal the timed DAC profile and wait for its queued work to drain.
    pub(super) fn finish_timed_dac_stream(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        self.flush_scheduled_until(sample_frame, output_rate)?;
        let Some((finished, has_records, has_events)) =
            self.timed_dac_stream.as_ref().map(|stream| {
                (
                    stream.finished,
                    stream.next_sequence > 0,
                    stream.queued_events > 0,
                )
            })
        else {
            return Ok(());
        };
        if finished || (!has_records && !has_events) {
            if let Some(stream) = self.timed_dac_stream.as_mut() {
                stream.finished = true;
            }
            return Ok(());
        }
        if !has_records {
            self.send_timed_dac_records(&[DacRecord {
                kind: DacRecordKind::Wait,
                value: 0,
                wait_ticks: 1,
            }])?;
        }
        let (next_sequence, end_tick) = self
            .timed_dac_stream
            .as_ref()
            .map(|stream| (stream.next_sequence, stream.accepted_tick))
            .expect("stream was checked above");
        self.request(Command::DacEnd {
            profile: DAC_PROFILE_YM2612,
            next_sequence,
            end_tick,
        })?;
        self.timed_dac_stream
            .as_mut()
            .expect("stream was checked above")
            .sealed = true;
        self.start_timed_dac_if_ready(true)?;

        let deadline = Instant::now() + DAC_DRAIN_TIMEOUT;
        loop {
            let status = self.query_timed_dac_status()?;
            self.update_timed_dac_progress(status)?;
            if status.state == 2 {
                self.timed_dac_stream
                    .as_mut()
                    .expect("stream was checked above")
                    .finished = true;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "timed out draining NDSIF DAC stream (state={}, accepted={}, consumed={}, queued={}, accepted_tick={}, played_tick={})",
                    status.state,
                    status.accepted,
                    status.consumed,
                    status.queued,
                    status.accepted_tick,
                    status.played_tick
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

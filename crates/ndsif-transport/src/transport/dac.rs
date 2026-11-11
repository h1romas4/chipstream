//! Timed DAC stream bookkeeping and device operations.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{NdsifRegisterEvent, NdsifTransport};
use ndsif::{
    Command, DAC_PROFILE_YM2612, DacCapabilities, DacRecord, DacRecordKind, DacStatus,
    MAX_DAC_EVENT_BYTES, MAX_DAC_RECORDS, Reply, Response, WriteId,
};

use super::{
    DAC_DRAIN_TIMEOUT, NdsifHostTimeline, NdsifSerialTransport, NdsifTransportError, PendingEvent,
};

const DAC_PREBUFFER_MILLISECONDS: u64 = 80;
const DAC_PRESTART_WINDOW_BYTES: usize = 2048;
const DAC_RUNNING_WINDOW_BYTES: usize = 4096;
const DAC_HOST_SERIAL_WINDOW_BYTES: usize = 1024;
const DAC_SYNC_RESERVE_BYTES: usize = 64;
const DAC_STATUS_INTERVAL: Duration = Duration::from_millis(5);
const DAC_STATUS_RETRY_INTERVAL: Duration = Duration::from_millis(20);
const DAC_STATUS_TIMEOUT: Duration = Duration::from_millis(500);
pub(crate) const DAC_CHUNK_MILLISECONDS: u64 = 5;
const DAC_CHUNK_SAMPLE_LIMIT: usize = 59;
const DAC_HOST_QUEUE_LIMIT: usize = 65_536;
const DAC_HOST_EVENT_BUDGET: usize = 128;
const DAC_HOST_FRAME_OVERHEAD: usize = 32;
const DAC_HOST_WINDOW_BYTES: u64 = 256;
const DAC_HOST_MIN_LEAD_MILLISECONDS: u64 = 20;

struct StatusFence {
    transaction_id: u16,
    sequence: u32,
    tick: u64,
    events: u32,
    ordinal: u64,
    epoch: u64,
    host_bytes: u64,
    requested_at: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DacStartMode {
    Automatic,
    Deferred,
    Prepared,
    Attempted,
}

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

/// Advance the DAC timeline, packing waits into the preceding record when possible.
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
    if let Some(record) = records.last_mut() {
        let available = u64::from(u16::MAX - record.wait_ticks);
        let wait_ticks = (target_tick - *current_tick).min(available) as u16;
        record.wait_ticks += wait_ticks;
        *current_tick += u64::from(wait_ticks);
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
pub(crate) fn is_timed_dac_data_register(event: &NdsifRegisterEvent) -> bool {
    event.write_id == WriteId::YM2612
        && event.instance == 0
        && event.page == 0
        && matches!(event.write.address, 0x2a | 0x2b)
}

/// Convert a reported timed-DAC fault state into a descriptive error.
fn check_timed_dac_fault(status: DacStatus) -> Result<(), String> {
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
    Ok(())
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
    pub(super) events_sent: u32,
    last_consumed: u32,
    last_event_consumed: u32,
    status_fences: Vec<StatusFence>,
    status_ordinal: u64,
    applied_status_ordinal: u64,
    status_epoch: u64,
    last_valid_status: Option<Instant>,
    host_registers: bool,
    pub(super) host_events: VecDeque<PendingEvent>,
    pub(super) host_bytes_sent: u64,
    host_bytes_confirmed: u64,
    pub(super) started_at: Option<Instant>,
    initial_events: Vec<PendingEvent>,
    initial_finished: bool,
    start_mode: DacStartMode,
    cancellation: Option<Arc<NdsifHostTimeline>>,
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
            events_sent: 0,
            last_consumed: 0,
            last_event_consumed: 0,
            status_fences: Vec::new(),
            status_ordinal: 0,
            applied_status_ordinal: 0,
            status_epoch: 0,
            last_valid_status: None,
            host_registers: false,
            host_events: VecDeque::new(),
            host_bytes_sent: 0,
            host_bytes_confirmed: 0,
            started_at: None,
            initial_events: Vec::new(),
            initial_finished: false,
            start_mode: DacStartMode::Automatic,
            cancellation: None,
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
        self.last_consumed = status.consumed;
        self.last_event_consumed = status.event_consumed;
        Ok(())
    }
}

impl NdsifSerialTransport {
    pub(super) fn schedule_timed_dac_initial_event(
        &mut self,
        event: PendingEvent,
    ) -> Result<(), String> {
        let stream = self
            .timed_dac_stream
            .as_mut()
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?;
        if stream.initial_finished || stream.started {
            return Err("NDSIF timed DAC initialization has already ended".into());
        }
        if stream.initial_events.len() >= 65_536 {
            return Err("NDSIF timed DAC initialization exceeds its event limit".into());
        }
        stream.initial_events.push(event);
        Ok(())
    }

    pub(super) fn finish_timed_dac_initialization(&mut self, has_wait: bool) -> Result<(), String> {
        let stream = self
            .timed_dac_stream
            .as_mut()
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?;
        if stream.initial_finished {
            return Ok(());
        }
        stream.initial_finished = true;
        let events = std::mem::take(&mut stream.initial_events);
        let mut keys: [Option<(usize, NdsifRegisterEvent)>; 8] = std::array::from_fn(|_| None);
        let mut dac = None;
        let mut index = 0;
        while index < events.len() {
            match &events[index] {
                PendingEvent::Register(event) if is_timed_dac_data_register(event) => {
                    if event.write.address == 0x2a {
                        dac = Some(event.clone());
                    } else {
                        self.pending.push(events[index].clone());
                    }
                    index += 1;
                }
                PendingEvent::Register(event)
                    if event.write_id == WriteId::YM2612
                        && event.instance == 0
                        && event.page == 0
                        && event.write.address == 0x28 =>
                {
                    keys[usize::from(event.write.value & 7)] = Some((index, event.clone()));
                    index += 1;
                }
                PendingEvent::Register(first) => {
                    let mut writes = Vec::new();
                    while index < events.len() && writes.len() < ndsif::MAX_REGISTER_WRITES {
                        let PendingEvent::Register(next) = &events[index] else {
                            break;
                        };
                        if is_timed_dac_data_register(next)
                            || (next.write_id == WriteId::YM2612
                                && next.instance == 0
                                && next.page == 0
                                && next.write.address == 0x28)
                            || next.write_id != first.write_id
                            || next.instance != first.instance
                            || next.page != first.page
                        {
                            break;
                        }
                        writes.push(next.write);
                        index += 1;
                    }
                    self.request_timed_dac(Command::WriteRegisters {
                        write_id: first.write_id,
                        instance: first.instance,
                        page: first.page,
                        writes: &writes,
                    })?;
                }
                PendingEvent::Psg(first) => {
                    let mut bytes = Vec::new();
                    while index < events.len() && bytes.len() < ndsif::MAX_REGISTER_BYTES {
                        let PendingEvent::Psg(next) = &events[index] else {
                            break;
                        };
                        if next.write_id != first.write_id
                            || next.instance != first.instance
                            || next.page != first.page
                        {
                            break;
                        }
                        bytes.push(next.value);
                        index += 1;
                    }
                    self.request_timed_dac(Command::WriteBytes {
                        write_id: first.write_id,
                        instance: first.instance,
                        page: first.page,
                        bytes: &bytes,
                    })?;
                }
                _ => return Err("invalid NDSIF timed DAC initialization event".into()),
            }
        }
        if has_wait {
            let mut keys: Vec<_> = keys.into_iter().flatten().collect();
            keys.sort_by_key(|(index, _)| *index);
            self.pending.extend(
                keys.into_iter()
                    .map(|(_, event)| PendingEvent::Register(event)),
            );
            if let Some(event) = dac {
                self.pending.push(PendingEvent::Register(event));
            }
        }
        Ok(())
    }

    /// Keep unanswered DAC traffic within the device's receive window.
    fn request_timed_dac(&mut self, command: Command<'_>) -> Result<(), String> {
        self.check_timed_dac_cancellation()?;
        let started = self
            .timed_dac_stream
            .as_ref()
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?
            .started;
        let window = if started {
            DAC_RUNNING_WINDOW_BYTES
        } else {
            DAC_PRESTART_WINDOW_BYTES
        };
        let size = command
            .to_frame(0)
            .map_err(|error| error.to_string())?
            .encode()
            .as_bytes()
            .len();
        if size + DAC_SYNC_RESERVE_BYTES > window {
            return Err("NDSIF timed DAC frame exceeds the transmit window".into());
        }
        while self
            .session
            .unconfirmed_bytes()
            .saturating_add(size)
            .saturating_add(DAC_SYNC_RESERVE_BYTES)
            > window
        {
            if started {
                self.check_timed_dac_status_if_due()?;
                if self
                    .session
                    .unconfirmed_bytes()
                    .saturating_add(size)
                    .saturating_add(DAC_SYNC_RESERVE_BYTES)
                    > window
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
            } else {
                self.request(Command::Ping(b"chipstream-dac-sync"))?;
            }
        }
        self.request(command)?;
        Ok(())
    }

    /// Convert due events into timed DAC records and event batches.
    pub(super) fn flush_timed_dac_until(
        &mut self,
        events: Vec<PendingEvent>,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        self.check_timed_dac_cancellation()?;
        let Some(stream) = self.timed_dac_stream.as_mut() else {
            return Err("NDSIF timed DAC stream is not configured".to_owned());
        };
        if stream.start_mode == DacStartMode::Attempted && !stream.started {
            return Err("NDSIF timed DAC start failed; reset or close the device".into());
        }
        if stream
            .output_rate
            .is_some_and(|configured_rate| configured_rate != output_rate)
        {
            return Err("NDSIF timed DAC output rate changed during playback".to_owned());
        }
        stream.output_rate = Some(output_rate);
        let ticks_per_second = stream.capabilities.ticks_per_second;
        let record_tick = stream.accepted_tick;
        let target_tick = ticks_for_frame(sample_frame, output_rate, ticks_per_second)?;
        if target_tick < record_tick {
            return Err(format!(
                "NDSIF timed DAC timeline moved backward (accepted={}, target={target_tick})",
                record_tick
            ));
        }

        let mut due: VecDeque<_> = Self::order_events(events, None)?.into();
        let chunk_ticks = (u64::from(ticks_per_second) * DAC_CHUNK_MILLISECONDS / 1000).max(1);
        while !due.is_empty()
            || self
                .timed_dac_stream
                .as_ref()
                .expect("stream was checked above")
                .accepted_tick
                < target_tick
        {
            let current = self
                .timed_dac_stream
                .as_ref()
                .expect("stream was checked above")
                .accepted_tick;
            let normal_end = current.saturating_add(chunk_ticks).min(target_tick);
            let last_tick = due
                .back()
                .map(|event| ticks_for_frame(event.sample_frame(), output_rate, ticks_per_second))
                .transpose()?;
            let first_tick = due
                .front()
                .map(|event| ticks_for_frame(event.sample_frame(), output_rate, ticks_per_second))
                .transpose()?;
            let mut end_tick = if last_tick.is_none_or(|tick| tick < normal_end) {
                target_tick
            } else if first_tick.is_some_and(|tick| tick > normal_end) {
                first_tick
                    .expect("first tick was checked above")
                    .min(target_tick)
            } else {
                normal_end
            };
            let mut chunk = Vec::new();
            let mut samples = 0;
            while let Some(event) = due.front() {
                let tick = ticks_for_frame(event.sample_frame(), output_rate, ticks_per_second)?;
                if tick > target_tick || tick < current {
                    return Err("NDSIF timed DAC event falls outside its flush interval".to_owned());
                }
                if tick > end_tick || (tick == end_tick && end_tick < target_tick) {
                    break;
                }
                let is_sample = matches!(event, PendingEvent::Register(register) if is_timed_dac_data_register(register));
                if is_sample && samples == DAC_CHUNK_SAMPLE_LIMIT {
                    end_tick = tick;
                    break;
                }
                samples += usize::from(is_sample);
                chunk.push(due.pop_front().expect("front was checked above"));
            }
            if chunk.is_empty() && end_tick == current {
                return Err("NDSIF timed DAC chunk cannot advance".to_owned());
            }
            self.flush_timed_dac_chunk(chunk, end_tick, output_rate, ticks_per_second)?;
            self.service_timed_dac_host_events(false)?;
        }
        Ok(())
    }

    /// Encode, send, and account for one bounded timed-DAC chunk.
    fn flush_timed_dac_chunk(
        &mut self,
        due: Vec<PendingEvent>,
        target_tick: u64,
        output_rate: u32,
        ticks_per_second: u32,
    ) -> Result<(), String> {
        let stream = self
            .timed_dac_stream
            .as_mut()
            .expect("stream was checked above");
        let mut record_tick = stream.accepted_tick;
        let event_count = due
            .iter()
            .filter(|event| match event {
                PendingEvent::Register(register) => !is_timed_dac_data_register(register),
                PendingEvent::Psg(_) => true,
                _ => false,
            })
            .count();
        if !stream.host_registers
            && u64::from(stream.queued_events) + event_count as u64
                > u64::from(stream.capabilities.event_capacity)
        {
            stream.host_registers = true;
        }
        if stream.host_registers
            && stream.host_events.len().saturating_add(event_count) > DAC_HOST_QUEUE_LIMIT
        {
            return Err("NDSIF timed DAC host event queue exceeds its limit".to_owned());
        }
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
                    if tick == 0
                        && first.write.address == 0x2a
                        && records.last().is_some_and(|record| {
                            record.kind == DacRecordKind::Dac
                                && record.wait_ticks == 0
                                && record.value == first.write.value
                        })
                    {
                        index += 1;
                        continue;
                    }
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
                    if self
                        .timed_dac_stream
                        .as_ref()
                        .expect("stream was checked above")
                        .host_registers
                    {
                        self.timed_dac_stream
                            .as_mut()
                            .expect("stream was checked above")
                            .host_events
                            .push_back(due[index].clone());
                        index += 1;
                        continue;
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
                    if self
                        .timed_dac_stream
                        .as_ref()
                        .expect("stream was checked above")
                        .host_registers
                    {
                        self.timed_dac_stream
                            .as_mut()
                            .expect("stream was checked above")
                            .host_events
                            .push_back(due[index].clone());
                        index += 1;
                        continue;
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
        check_timed_dac_fault(status)?;
        self.timed_dac_stream
            .as_mut()
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?
            .update_progress(status)?;
        let stream = self
            .timed_dac_stream
            .as_mut()
            .expect("stream was checked above");
        stream.status_epoch = stream
            .status_epoch
            .checked_add(1)
            .ok_or_else(|| "NDSIF DAC status epoch overflow".to_owned())?;
        stream.last_valid_status = Some(Instant::now());
        stream.host_bytes_confirmed = stream.host_bytes_sent;
        Ok(())
    }

    /// Wait for room in the device's timed DAC record queue.
    fn ensure_timed_dac_record_capacity(&mut self, incoming: usize) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            self.check_timed_dac_cancellation()?;
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
            self.check_timed_dac_status_if_due()?;
            self.service_timed_dac_host_events(false)?;
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
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Wait for room in the device's timed DAC event queue.
    fn ensure_timed_dac_event_capacity(&mut self, incoming: usize) -> Result<(), String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            self.check_timed_dac_cancellation()?;
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
            self.check_timed_dac_status_if_due()?;
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
            std::thread::sleep(Duration::from_millis(1));
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
            self.request_timed_dac(Command::DacData {
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
        self.request_timed_dac(Command::DacRegisterEvent {
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
        stream.events_sent = stream
            .events_sent
            .checked_add(count as u32)
            .ok_or_else(|| "NDSIF DAC event count overflow".to_owned())?;
        Ok(())
    }

    /// Send due host-paced writes only after earlier device events have drained.
    pub(crate) fn service_timed_dac_host_events(&mut self, startup: bool) -> Result<(), String> {
        self.check_timed_dac_cancellation()?;
        if !startup {
            self.check_timed_dac_status_if_due()?;
        }
        let Some(stream) = self.timed_dac_stream.as_ref() else {
            return Ok(());
        };
        if stream.host_events.is_empty()
            || (startup && stream.events_sent != 0)
            || (!startup && (!stream.started || stream.last_event_consumed != stream.events_sent))
        {
            return Ok(());
        }
        let ticks_per_second = stream.capabilities.ticks_per_second;
        let now_tick = if startup {
            0
        } else {
            let started = stream
                .started_at
                .ok_or_else(|| "NDSIF timed DAC host clock is not initialized".to_owned())?;
            let tick = started.elapsed().as_nanos() * u128::from(ticks_per_second) / 1_000_000_000;
            u64::try_from(tick).map_err(|_| "NDSIF timed DAC host clock overflow".to_owned())?
        };
        let lead = u64::from(ticks_per_second) * DAC_HOST_MIN_LEAD_MILLISECONDS / 1000;
        if !startup && !stream.sealed && stream.accepted_tick < now_tick.saturating_add(lead) {
            return Ok(());
        }
        let mut budget = DAC_HOST_EVENT_BUDGET;
        while budget > 0 {
            let stream = self
                .timed_dac_stream
                .as_ref()
                .expect("stream was checked above");
            let Some(first) = stream.host_events.front() else {
                break;
            };
            let first_tick = ticks_for_frame(
                first.sample_frame(),
                stream
                    .output_rate
                    .ok_or_else(|| "NDSIF timed DAC output rate is not configured".to_owned())?,
                ticks_per_second,
            )?;
            if first_tick > now_tick {
                break;
            }
            let (write_id, instance, page, psg) = match first {
                PendingEvent::Register(event) => {
                    (event.write_id, event.instance, event.page, false)
                }
                PendingEvent::Psg(event) => (event.write_id, event.instance, event.page, true),
                _ => return Err("invalid NDSIF timed DAC host event".to_owned()),
            };
            let available = if startup {
                usize::MAX
            } else {
                let serial = DAC_HOST_SERIAL_WINDOW_BYTES
                    .saturating_sub(self.session.unconfirmed_bytes())
                    .saturating_sub(DAC_SYNC_RESERVE_BYTES + DAC_HOST_FRAME_OVERHEAD);
                let fm = DAC_HOST_WINDOW_BYTES
                    .saturating_sub(stream.host_bytes_sent - stream.host_bytes_confirmed)
                    .saturating_sub(DAC_HOST_FRAME_OVERHEAD as u64);
                serial.min(usize::try_from(fm).unwrap_or(usize::MAX))
            };
            let width = if psg { 1 } else { 2 };
            if available < width {
                break;
            }
            let mut writes = Vec::new();
            let mut bytes = Vec::new();
            let rate = stream.output_rate.expect("output rate was checked above");
            for event in &stream.host_events {
                if writes.len() + bytes.len() >= budget
                    || (writes.len() * 2 + bytes.len() + width) > available
                    || ticks_for_frame(event.sample_frame(), rate, ticks_per_second)? > now_tick
                {
                    break;
                }
                match event {
                    PendingEvent::Register(event)
                        if !psg
                            && event.write_id == write_id
                            && event.instance == instance
                            && event.page == page
                            && writes.len() < ndsif::MAX_REGISTER_WRITES =>
                    {
                        writes.push(event.write)
                    }
                    PendingEvent::Psg(event)
                        if psg
                            && event.write_id == write_id
                            && event.instance == instance
                            && event.page == page
                            && bytes.len() < ndsif::MAX_REGISTER_BYTES =>
                    {
                        bytes.push(event.value)
                    }
                    _ => break,
                }
            }
            let count = writes.len() + bytes.len();
            if count == 0 {
                return Err("NDSIF timed DAC host batch made no progress".to_owned());
            }
            if psg {
                self.request_timed_dac(Command::WriteBytes {
                    write_id,
                    instance,
                    page,
                    bytes: &bytes,
                })?;
            } else {
                self.request_timed_dac(Command::WriteRegisters {
                    write_id,
                    instance,
                    page,
                    writes: &writes,
                })?;
            }
            let stream = self
                .timed_dac_stream
                .as_mut()
                .expect("stream was checked above");
            for _ in 0..count {
                stream.host_events.pop_front();
            }
            if !startup {
                stream.host_bytes_sent = stream
                    .host_bytes_sent
                    .checked_add((count * width + DAC_HOST_FRAME_OVERHEAD) as u64)
                    .ok_or_else(|| "NDSIF timed DAC host byte count overflow".to_owned())?;
            }
            budget -= count;
        }
        Ok(())
    }

    /// Queue host register events at tick zero before starting the DAC profile.
    fn prepare_timed_dac_host_startup(&mut self) -> Result<(), String> {
        loop {
            let Some(stream) = self.timed_dac_stream.as_ref() else {
                return Ok(());
            };
            let Some(event) = stream.host_events.front() else {
                return Ok(());
            };
            if stream.events_sent != 0
                || ticks_for_frame(
                    event.sample_frame(),
                    stream.output_rate.ok_or_else(|| {
                        "NDSIF timed DAC output rate is not configured".to_owned()
                    })?,
                    stream.capabilities.ticks_per_second,
                )? != 0
            {
                return Ok(());
            }
            self.service_timed_dac_host_events(true)?;
        }
    }

    /// Start the DAC profile when sufficiently buffered or explicitly forced.
    pub(super) fn start_timed_dac_if_ready(&mut self, force: bool) -> Result<(), String> {
        let Some((ready, minimum_lead_ticks)) = self.timed_dac_stream.as_ref().map(|stream| {
            (
                stream.start_mode == DacStartMode::Automatic
                    && !stream.started
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
        self.prepare_timed_dac_host_startup()?;
        self.request(Command::Ping(b"chipstream-dac-start"))?;
        self.start_timed_dac_after_ping(minimum_lead_ticks)
    }

    /// Start the DAC profile and verify its initial device status.
    fn start_timed_dac_after_ping(&mut self, minimum_lead_ticks: u32) -> Result<(), String> {
        self.request(Command::DacStart {
            profile: DAC_PROFILE_YM2612,
            minimum_lead_ticks,
        })?;
        self.timed_dac_stream
            .as_mut()
            .expect("stream was checked above")
            .started_at = Some(Instant::now());
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

    /// Return an error if the playback timeline has been cancelled.
    fn check_timed_dac_cancellation(&self) -> Result<(), String> {
        if self.timed_dac_stream.as_ref().is_some_and(|stream| {
            stream
                .cancellation
                .as_ref()
                .is_some_and(|timeline| timeline.is_cancelled())
        }) {
            return Err("NDSIF timed DAC stream was cancelled".into());
        }
        Ok(())
    }

    /// Defer the timed DAC clock and associate it with playback cancellation.
    ///
    /// # Errors
    ///
    /// Fails if the stream is absent or has already buffered or started.
    pub fn defer_timed_dac_start_with_cancellation(
        &mut self,
        timeline: Arc<NdsifHostTimeline>,
    ) -> Result<(), String> {
        let stream = self
            .timed_dac_stream
            .as_mut()
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?;
        if stream.start_mode != DacStartMode::Automatic
            || stream.next_sequence != 0
            || stream.queued_events != 0
            || stream.started
        {
            return Err(
                "NDSIF timed DAC stream cannot be deferred after buffering or starting".into(),
            );
        }
        stream.start_mode = DacStartMode::Deferred;
        stream.cancellation = Some(timeline);
        Ok(())
    }

    /// Verify that buffered DAC ticks cover the specified PCM frames and synchronize
    /// and validate the device's queued records and events.
    ///
    /// # Errors
    ///
    /// Fails if the stream is not deferred, the lead is insufficient, device
    /// progress does not match the host, or synchronization fails.
    pub fn prepare_timed_dac_start_for_frames(
        &mut self,
        frames: usize,
        rate: u32,
    ) -> Result<(), String> {
        self.check_timed_dac_cancellation()?;
        let stream = self
            .timed_dac_stream
            .as_ref()
            .ok_or_else(|| "NDSIF timed DAC stream is not configured".to_owned())?;
        if stream.start_mode != DacStartMode::Deferred || stream.started {
            return Err("NDSIF timed DAC stream is not awaiting preparation".into());
        }
        if stream.output_rate != Some(rate) {
            return Err("NDSIF timed DAC prebuffer rate differs from output rate".into());
        }
        let ticks = ticks_for_frame(frames, rate, stream.capabilities.ticks_per_second)?;
        if ticks == 0 || stream.accepted_tick < ticks || stream.next_sequence == 0 {
            return Err(format!(
                "NDSIF timed DAC prebuffer is insufficient (buffered={}, required={ticks})",
                stream.accepted_tick
            ));
        }
        self.prepare_timed_dac_host_startup()?;
        self.request(Command::Ping(b"chipstream-dac-start"))?;
        let status = self.query_timed_dac_status()?;
        let stream = self
            .timed_dac_stream
            .as_ref()
            .expect("stream was checked above");
        if status.state != 0
            || status.queued != stream.queued_records
            || status.event_queued != stream.queued_events
        {
            return Err(format!(
                "NDSIF timed DAC prebuffer does not match device (state={}, records={}/{}, events={}/{})",
                status.state,
                status.queued,
                stream.queued_records,
                status.event_queued,
                stream.queued_events
            ));
        }
        self.update_timed_dac_progress(status)?;
        self.timed_dac_stream
            .as_mut()
            .expect("stream was checked above")
            .start_mode = DacStartMode::Prepared;
        Ok(())
    }

    /// Start a prepared timed DAC stream and drain it if it was sealed before starting.
    ///
    /// # Errors
    ///
    /// Fails on cancellation, invalid state, or device errors.
    pub fn start_prepared_timed_dac(&mut self) -> Result<(), String> {
        self.check_timed_dac_cancellation()?;
        let stream = self
            .timed_dac_stream
            .as_mut()
            .filter(|stream| stream.start_mode == DacStartMode::Prepared && !stream.started)
            .ok_or_else(|| "NDSIF timed DAC stream is not prepared for starting".to_owned())?;
        let minimum = u64::from(stream.minimum_lead_ticks()).min(stream.accepted_tick) as u32;
        stream.start_mode = DacStartMode::Attempted;
        self.start_timed_dac_after_ping(minimum)?;
        if self
            .timed_dac_stream
            .as_ref()
            .is_some_and(|stream| stream.sealed)
        {
            self.drain_timed_dac()?;
        }
        Ok(())
    }

    /// Poll and validate DAC progress when the status interval has elapsed.
    pub(crate) fn check_timed_dac_status_if_due(&mut self) -> Result<(), String> {
        if !self
            .timed_dac_stream
            .as_ref()
            .is_some_and(|stream| stream.started && !stream.finished)
        {
            return Ok(());
        }
        self.check_timed_dac_cancellation()?;
        while let Some(frame) = self
            .session
            .poll_dac_status()
            .map_err(|error| error.to_string())?
        {
            let response = Response::decode(&frame).map_err(|error| error.to_string())?;
            let status = match response.reply {
                Reply::DacStatus(status) if status.profile == DAC_PROFILE_YM2612 => status,
                _ => return Err("unexpected NDSIF DAC_STATUS response".to_owned()),
            };
            let stream = self
                .timed_dac_stream
                .as_mut()
                .expect("stream was checked above");
            let index = stream
                .status_fences
                .iter()
                .position(|fence| fence.transaction_id == frame.transaction_id())
                .ok_or_else(|| "NDSIF DAC_STATUS response has no request fence".to_owned())?;
            let fence = stream.status_fences.remove(index);
            check_timed_dac_fault(status)?;
            if status.accepted != fence.sequence
                || status.accepted_tick != fence.tick
                || status.accepted.checked_sub(status.consumed) != Some(status.queued)
                || status.queued.checked_add(status.free)
                    != Some(stream.capabilities.record_capacity)
                || status.event_consumed.checked_add(status.event_queued) != Some(fence.events)
            {
                return Err(
                    "NDSIF asynchronous DAC_STATUS counters do not match the request fence"
                        .to_owned(),
                );
            }
            stream.host_bytes_confirmed = stream.host_bytes_confirmed.max(fence.host_bytes);
            if fence.epoch == stream.status_epoch && fence.ordinal > stream.applied_status_ordinal {
                if status.consumed < stream.last_consumed
                    || status.event_consumed < stream.last_event_consumed
                    || (status.state == 2 && !stream.sealed)
                    || status.state == 0
                {
                    return Err(
                        "NDSIF asynchronous DAC_STATUS moved backward or stopped unexpectedly"
                            .to_owned(),
                    );
                }
                let queued_records = stream
                    .next_sequence
                    .checked_sub(status.consumed)
                    .ok_or_else(|| {
                        "NDSIF asynchronous DAC record count exceeds the host sequence".to_owned()
                    })?;
                let queued_events = stream
                    .events_sent
                    .checked_sub(status.event_consumed)
                    .ok_or_else(|| {
                        "NDSIF asynchronous DAC event count exceeds host submissions".to_owned()
                    })?;
                if queued_records > stream.capabilities.record_capacity
                    || queued_events > u32::from(stream.capabilities.event_capacity)
                {
                    return Err("NDSIF asynchronous DAC_STATUS exceeds queue capacity".to_owned());
                }
                stream.queued_records = queued_records;
                stream.queued_events = queued_events;
                stream.last_consumed = status.consumed;
                stream.last_event_consumed = status.event_consumed;
                stream.applied_status_ordinal = fence.ordinal;
                stream.last_valid_status = Some(Instant::now());
            }
        }
        let stream = self
            .timed_dac_stream
            .as_mut()
            .expect("stream was checked above");
        if stream
            .last_valid_status
            .is_some_and(|last| last.elapsed() >= DAC_STATUS_TIMEOUT)
        {
            return Err("timed out waiting for NDSIF DAC_STATUS".to_owned());
        }
        let mut retired = Vec::new();
        let applied = stream.applied_status_ordinal;
        let epoch = stream.status_epoch;
        stream.status_fences.retain(|fence| {
            if fence.epoch != epoch
                || fence.ordinal < applied
                || fence.requested_at.elapsed() >= DAC_STATUS_TIMEOUT
            {
                retired.push(fence.transaction_id);
                false
            } else {
                true
            }
        });
        for id in retired {
            self.session
                .retire_dac_status(id)
                .map_err(|error| error.to_string())?;
        }
        let interval = if stream.status_fences.is_empty() {
            DAC_STATUS_INTERVAL
        } else {
            DAC_STATUS_RETRY_INTERVAL
        };
        if stream
            .last_status_check
            .is_none_or(|last| last.elapsed() >= interval)
        {
            if !stream.status_fences.is_empty() {
                self.session.expire_partial_dac_status();
            }
            let transaction_id = self
                .session
                .begin_dac_status(DAC_PROFILE_YM2612)
                .map_err(|error| error.to_string())?;
            stream.status_ordinal = stream
                .status_ordinal
                .checked_add(1)
                .ok_or_else(|| "NDSIF DAC status sequence overflow".to_owned())?;
            let now = Instant::now();
            stream.status_fences.push(StatusFence {
                transaction_id,
                sequence: stream.next_sequence,
                tick: stream.accepted_tick,
                events: stream.events_sent,
                ordinal: stream.status_ordinal,
                epoch: stream.status_epoch,
                host_bytes: stream.host_bytes_sent,
                requested_at: now,
            });
            stream.last_status_check = Some(now);
        }
        Ok(())
    }

    /// Seal the timed DAC profile and wait for its queued work to drain.
    pub(super) fn finish_timed_dac_stream(
        &mut self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), String> {
        if self
            .timed_dac_stream
            .as_ref()
            .is_some_and(|stream| !stream.initial_finished && !stream.initial_events.is_empty())
        {
            self.finish_timed_dac_initialization(false)?;
        }
        if let Some(stream) = self.timed_dac_stream.as_ref()
            && stream.sealed
        {
            return if stream.finished || !stream.started {
                Ok(())
            } else {
                self.drain_timed_dac()
            };
        }
        self.flush_scheduled_until(sample_frame, output_rate)?;
        let Some((finished, has_records, has_events, has_host_events)) =
            self.timed_dac_stream.as_ref().map(|stream| {
                (
                    stream.finished,
                    stream.next_sequence > 0,
                    stream.queued_events > 0,
                    !stream.host_events.is_empty(),
                )
            })
        else {
            return Ok(());
        };
        if !has_records && !has_events && has_host_events {
            self.prepare_timed_dac_host_startup()?;
            if self
                .timed_dac_stream
                .as_ref()
                .expect("stream was checked above")
                .host_events
                .is_empty()
            {
                self.request(Command::Ping(b"chipstream-dac-finish"))?;
                self.timed_dac_stream
                    .as_mut()
                    .expect("stream was checked above")
                    .finished = true;
                return Ok(());
            }
        }
        if finished || (!has_records && !has_events && !has_host_events) {
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
        self.request_timed_dac(Command::DacEnd {
            profile: DAC_PROFILE_YM2612,
            next_sequence,
            end_tick,
        })?;
        self.timed_dac_stream
            .as_mut()
            .expect("stream was checked above")
            .sealed = true;
        if self.timed_dac_stream.as_ref().is_some_and(|stream| {
            stream.start_mode == DacStartMode::Deferred
                || stream.start_mode == DacStartMode::Prepared
        }) {
            return Ok(());
        }
        self.start_timed_dac_if_ready(true)?;

        self.drain_timed_dac()
    }

    /// Poll and service the timed-DAC stream until it drains or times out.
    fn drain_timed_dac(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + DAC_DRAIN_TIMEOUT;
        loop {
            self.check_timed_dac_cancellation()?;
            let status = self.query_timed_dac_status()?;
            self.update_timed_dac_progress(status)?;
            self.service_timed_dac_host_events(false)?;
            if status.state == 2
                && self
                    .timed_dac_stream
                    .as_ref()
                    .expect("stream was checked above")
                    .host_events
                    .is_empty()
            {
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

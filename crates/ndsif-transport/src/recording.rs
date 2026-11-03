//! Bounded recording and ordered replay of scheduled transport operations.

use std::fmt;
use std::num::NonZeroUsize;

use chipstream::chip::{
    NdsifAdpcmEvent, NdsifAudioControlEvent, NdsifPsgEvent, NdsifRegisterEvent, NdsifTransport,
};
use ndsif::{CapabilityEntry, Divider, WriteId};

use crate::NdsifSerialTransport;
use crate::routing::NdsifRoutingTransport;
use crate::transport::{
    DAC_CHUNK_MILLISECONDS, is_timed_dac_data_register, supports_cached_audio,
    supports_cached_clock,
};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Operation {
    ConfigureChip(WriteId, u8, u32),
    Register(NdsifRegisterEvent),
    Psg(NdsifPsgEvent),
    InitialRegister(NdsifRegisterEvent),
    InitialPsg(NdsifPsgEvent),
    FinishVgmInitialization,
    ConfigureAudio(u8, u32, Divider, u32),
    Adpcm(NdsifAdpcmEvent),
    AudioControl(NdsifAudioControlEvent),
    Flush(usize, u32),
    StartAudio(u8),
    FinishAudio(u8, usize, u32),
    FinishDac(usize, u32),
}

/// A bounded in-memory transport for rendering ahead of device I/O.
///
/// It snapshots the serial device's advertised capabilities and records
/// transport calls without issuing serial commands. Drain it after each
/// render block and deliver the resulting batches to the matching physical
/// device in order. Device-specific configuration or I/O errors can only be
/// discovered when the batch is replayed.
pub struct NdsifRecordingTransport {
    capabilities: Vec<CapabilityEntry>,
    data_capacity: u32,
    event_capacity: u16,
    timed_dac_supported: bool,
    primary_ym2612: bool,
    audio_configured: bool,
    vgm_initialization_finished: bool,
    limit: NonZeroUsize,
    operations: Vec<Operation>,
    failed: bool,
}

impl NdsifRecordingTransport {
    /// Snapshot a device's capabilities and set a per-batch operation limit.
    pub fn new(device: &NdsifSerialTransport, limit: NonZeroUsize) -> Self {
        let (data_capacity, event_capacity) = device.recording_capacities();
        Self {
            capabilities: device.capabilities().to_vec(),
            data_capacity,
            event_capacity,
            timed_dac_supported: device.supports_timed_dac(),
            primary_ym2612: false,
            audio_configured: false,
            vgm_initialization_finished: false,
            limit,
            operations: Vec::new(),
            failed: false,
        }
    }

    fn record(&mut self, operation: Operation) -> Result<(), String> {
        if self.failed || self.operations.len() == self.limit.get() {
            self.failed = true;
            return Err(format!(
                "NDSIF recording exceeded its {} operation limit",
                self.limit
            ));
        }
        self.operations.push(operation);
        Ok(())
    }

    /// Remove all operations recorded since the previous drain.
    ///
    /// # Errors
    ///
    /// Returns an error after the operation limit was exceeded. A failed
    /// recorder cannot be drained or resumed; discard the playback instead.
    pub fn take_batch(&mut self) -> Result<NdsifCommandBatch, NdsifRecordingError> {
        if self.failed {
            return Err(NdsifRecordingError { limit: self.limit });
        }
        Ok(NdsifCommandBatch {
            operations: std::mem::take(&mut self.operations),
        })
    }

    /// Conservatively bound timed DAC records and events in the pending batch.
    pub(crate) fn timed_dac_prebuffer_usage(&self, ticks_per_second: u32) -> (usize, usize) {
        timed_dac_prebuffer_usage(&self.operations, ticks_per_second)
    }
}

fn timed_dac_prebuffer_usage(operations: &[Operation], ticks_per_second: u32) -> (usize, usize) {
    let mut dac_writes = 0_usize;
    let mut events = 0_usize;
    let mut flushes = 0_usize;
    let mut max_tick = 0_u128;
    let mut first_event_tick = None;
    let mut last_event_tick = 0_u128;
    let mut record_event_tick = |frame: usize, rate: u32| {
        let tick = if rate == 0 {
            u128::MAX
        } else {
            (frame as u128) * u128::from(ticks_per_second) / u128::from(rate)
        };
        first_event_tick = Some(first_event_tick.map_or(tick, |first: u128| first.min(tick)));
        last_event_tick = last_event_tick.max(tick);
    };
    for operation in operations {
        match operation {
            Operation::Register(event) => {
                if is_timed_dac_data_register(event) {
                    dac_writes = dac_writes.saturating_add(1);
                } else {
                    events = events.saturating_add(1);
                    record_event_tick(event.sample_frame, event.sample_rate);
                }
            }
            Operation::InitialRegister(event) => {
                if is_timed_dac_data_register(event) {
                    dac_writes = dac_writes.saturating_add(1);
                } else if event.write_id == WriteId::YM2612
                    && event.instance == 0
                    && event.page == 0
                    && event.write.address == 0x28
                {
                    events = events.saturating_add(1);
                    record_event_tick(event.sample_frame, event.sample_rate);
                }
            }
            Operation::Psg(event) => {
                events = events.saturating_add(1);
                record_event_tick(event.sample_frame, event.sample_rate);
            }
            Operation::Flush(frame, rate) | Operation::FinishDac(frame, rate) => {
                flushes = flushes.saturating_add(1);
                let tick = if *rate == 0 {
                    u128::MAX
                } else {
                    (*frame as u128) * u128::from(ticks_per_second) / u128::from(*rate)
                };
                max_tick = max_tick.max(tick);
            }
            _ => {}
        }
    }
    let long_waits = usize::try_from(max_tick / u128::from(u16::MAX)).unwrap_or(usize::MAX);
    let chunk_ticks =
        (u128::from(ticks_per_second) * u128::from(DAC_CHUNK_MILLISECONDS) / 1000).max(1);
    let event_chunks = first_event_tick.map_or(0, |first| {
        usize::try_from((last_event_tick.saturating_sub(first) / chunk_ticks).saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(events)
    });
    (
        dac_writes
            .saturating_mul(2)
            .saturating_add(flushes)
            .saturating_add(event_chunks.saturating_mul(2))
            .saturating_add(long_waits),
        events,
    )
}

impl NdsifRoutingTransport for NdsifRecordingTransport {
    fn supports_route_clock(&self, write_id: WriteId, instance: u8, clock: u32) -> bool {
        supports_cached_clock(&self.capabilities, write_id, instance, clock)
    }

    fn supports_route_audio(&self, instance: u8, clock: u32) -> bool {
        supports_cached_audio(
            &self.capabilities,
            self.data_capacity,
            self.event_capacity,
            instance,
            clock,
        )
    }
}

impl NdsifTransport for NdsifRecordingTransport {
    fn supports_vgm_initialization(&self) -> bool {
        self.timed_dac_supported && self.primary_ym2612 && !self.audio_configured
    }

    fn configure_chip(&mut self, id: WriteId, instance: u8, clock: u32) -> Result<(), String> {
        self.record(Operation::ConfigureChip(id, instance, clock))?;
        if id == WriteId::YM2612 && instance == 0 {
            self.primary_ym2612 = true;
        }
        Ok(())
    }

    fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
        self.record(Operation::Register(event))
    }

    fn schedule_psg_write(&mut self, event: NdsifPsgEvent) -> Result<(), String> {
        self.record(Operation::Psg(event))
    }

    fn schedule_vgm_initial_register_write(
        &mut self,
        event: NdsifRegisterEvent,
    ) -> Result<(), String> {
        self.record(Operation::InitialRegister(event))
    }

    fn schedule_vgm_initial_psg_write(&mut self, event: NdsifPsgEvent) -> Result<(), String> {
        self.record(Operation::InitialPsg(event))
    }

    fn finish_vgm_initialization(&mut self) -> Result<(), String> {
        if !self.vgm_initialization_finished {
            self.record(Operation::FinishVgmInitialization)?;
            self.vgm_initialization_finished = true;
        }
        Ok(())
    }

    fn configure_audio_stream(
        &mut self,
        instance: u8,
        clock: u32,
        divider: Divider,
        rate: u32,
    ) -> Result<(), String> {
        self.record(Operation::ConfigureAudio(instance, clock, divider, rate))?;
        self.audio_configured = true;
        Ok(())
    }

    fn schedule_adpcm_data(&mut self, event: NdsifAdpcmEvent) -> Result<(), String> {
        self.record(Operation::Adpcm(event))
    }

    fn schedule_audio_control(&mut self, event: NdsifAudioControlEvent) -> Result<(), String> {
        self.record(Operation::AudioControl(event))
    }

    fn flush_scheduled_until(&mut self, frame: usize, rate: u32) -> Result<(), String> {
        self.record(Operation::Flush(frame, rate))
    }

    fn start_audio_stream(&mut self, instance: u8) -> Result<(), String> {
        self.record(Operation::StartAudio(instance))
    }

    fn finish_audio_stream(&mut self, instance: u8, frame: usize, rate: u32) -> Result<(), String> {
        self.record(Operation::FinishAudio(instance, frame, rate))
    }

    fn finish_dac_stream(&mut self, frame: usize, rate: u32) -> Result<(), String> {
        self.record(Operation::FinishDac(frame, rate))
    }
}

/// A batch of recorded operations, in their original call order.
///
/// Batches can be sent to a device worker. Replay can block while the target
/// paces or communicates with the device.
pub struct NdsifCommandBatch {
    operations: Vec<Operation>,
}

impl NdsifCommandBatch {
    pub(crate) fn timed_dac_prebuffer_usage(&self, ticks_per_second: u32) -> (usize, usize) {
        timed_dac_prebuffer_usage(&self.operations, ticks_per_second)
    }

    /// Return the number of operations in this batch.
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    /// Return whether this batch contains no operations.
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// Return whether this batch configures an OKIM6258 audio stream.
    pub fn configures_audio_stream(&self) -> bool {
        self.operations
            .iter()
            .any(|operation| matches!(operation, Operation::ConfigureAudio(..)))
    }

    /// Return whether this batch configures the primary YM2612 target.
    pub fn configures_primary_ym2612(&self) -> bool {
        self.operations.iter().any(|operation| {
            matches!(operation, Operation::ConfigureChip(id, 0, _) if *id == WriteId::YM2612)
        })
    }

    /// Apply this batch to its corresponding target in call order.
    ///
    /// # Errors
    ///
    /// Stops at the first target error and reports its zero-based operation
    /// index. Earlier operations may already have reached the device; stop
    /// playback and reset or close it rather than replaying the batch.
    pub fn replay<T: NdsifTransport + ?Sized>(
        self,
        target: &mut T,
    ) -> Result<(), NdsifReplayError> {
        for (index, operation) in self.operations.into_iter().enumerate() {
            let result = match operation {
                Operation::ConfigureChip(id, instance, clock) => {
                    target.configure_chip(id, instance, clock)
                }
                Operation::Register(event) => target.schedule_register_write(event),
                Operation::Psg(event) => target.schedule_psg_write(event),
                Operation::InitialRegister(event) => {
                    target.schedule_vgm_initial_register_write(event)
                }
                Operation::InitialPsg(event) => target.schedule_vgm_initial_psg_write(event),
                Operation::FinishVgmInitialization => target.finish_vgm_initialization(),
                Operation::ConfigureAudio(instance, clock, divider, rate) => {
                    target.configure_audio_stream(instance, clock, divider, rate)
                }
                Operation::Adpcm(event) => target.schedule_adpcm_data(event),
                Operation::AudioControl(event) => target.schedule_audio_control(event),
                Operation::Flush(frame, rate) => target.flush_scheduled_until(frame, rate),
                Operation::StartAudio(instance) => target.start_audio_stream(instance),
                Operation::FinishAudio(instance, frame, rate) => {
                    target.finish_audio_stream(instance, frame, rate)
                }
                Operation::FinishDac(frame, rate) => target.finish_dac_stream(frame, rate),
            };
            result.map_err(|message| NdsifReplayError { index, message })?;
        }
        Ok(())
    }
}

/// A recorder failed because its per-batch operation limit was exceeded.
#[derive(Debug)]
pub struct NdsifRecordingError {
    /// Maximum number of operations in a batch.
    pub limit: NonZeroUsize,
}

impl fmt::Display for NdsifRecordingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "NDSIF recording exceeded its {} operation limit",
            self.limit
        )
    }
}

impl std::error::Error for NdsifRecordingError {}

/// A replay failed after zero or more operations had been applied.
#[derive(Debug)]
pub struct NdsifReplayError {
    /// Zero-based index of the failed operation in the batch.
    pub index: usize,
    /// Error returned by the target transport.
    pub message: String,
}

impl fmt::Display for NdsifReplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "NDSIF operation {} failed: {}", self.index, self.message)
    }
}

impl std::error::Error for NdsifReplayError {}

#[cfg(test)]
mod tests {
    use super::*;
    use ndsif::{BytePosition, Pan};

    fn recorder(limit: usize) -> NdsifRecordingTransport {
        NdsifRecordingTransport {
            capabilities: Vec::new(),
            data_capacity: 0,
            event_capacity: 0,
            timed_dac_supported: false,
            primary_ym2612: false,
            audio_configured: false,
            vgm_initialization_finished: false,
            limit: NonZeroUsize::new(limit).unwrap(),
            operations: Vec::new(),
            failed: false,
        }
    }

    #[test]
    fn timed_dac_prebuffer_estimates_event_chunk_waits() {
        let mut recorder = recorder(8);
        for frame in [0, 220, 440] {
            recorder
                .schedule_register_write(NdsifRegisterEvent {
                    chip: chipstream::sequencer::Chip::Ym2612,
                    write_id: WriteId::YM2612,
                    instance: 0,
                    page: 0,
                    write: ndsif::RegisterWrite::new(0x22, 0),
                    sample_frame: frame,
                    sample_rate: 44_100,
                })
                .unwrap();
        }
        recorder.flush_scheduled_until(660, 44_100).unwrap();
        assert_eq!(recorder.timed_dac_prebuffer_usage(44_100), (7, 3));
    }

    #[derive(Default)]
    struct Sink {
        calls: Vec<Operation>,
        fail_at: Option<usize>,
    }

    impl Sink {
        fn record(&mut self, call: Operation) -> Result<(), String> {
            if self.fail_at == Some(self.calls.len()) {
                return Err("device failed".into());
            }
            self.calls.push(call);
            Ok(())
        }
    }

    impl NdsifTransport for Sink {
        fn configure_chip(&mut self, id: WriteId, instance: u8, clock: u32) -> Result<(), String> {
            self.record(Operation::ConfigureChip(id, instance, clock))
        }
        fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
            self.record(Operation::Register(event))
        }
        fn schedule_psg_write(&mut self, event: NdsifPsgEvent) -> Result<(), String> {
            self.record(Operation::Psg(event))
        }
        fn configure_audio_stream(
            &mut self,
            instance: u8,
            clock: u32,
            divider: Divider,
            rate: u32,
        ) -> Result<(), String> {
            self.record(Operation::ConfigureAudio(instance, clock, divider, rate))
        }
        fn schedule_adpcm_data(&mut self, event: NdsifAdpcmEvent) -> Result<(), String> {
            self.record(Operation::Adpcm(event))
        }
        fn schedule_audio_control(&mut self, event: NdsifAudioControlEvent) -> Result<(), String> {
            self.record(Operation::AudioControl(event))
        }
        fn flush_scheduled_until(&mut self, frame: usize, rate: u32) -> Result<(), String> {
            self.record(Operation::Flush(frame, rate))
        }
        fn start_audio_stream(&mut self, instance: u8) -> Result<(), String> {
            self.record(Operation::StartAudio(instance))
        }
        fn finish_audio_stream(
            &mut self,
            instance: u8,
            frame: usize,
            rate: u32,
        ) -> Result<(), String> {
            self.record(Operation::FinishAudio(instance, frame, rate))
        }
        fn finish_dac_stream(&mut self, frame: usize, rate: u32) -> Result<(), String> {
            self.record(Operation::FinishDac(frame, rate))
        }
    }

    fn all_operations(recorder: &mut NdsifRecordingTransport) {
        use chipstream::chip::NdsifAudioControl;
        use chipstream::sequencer::Chip;

        recorder
            .configure_chip(WriteId::YM2151, 0, 4_000_000)
            .unwrap();
        recorder
            .schedule_register_write(NdsifRegisterEvent {
                chip: Chip::Ym2151,
                write_id: WriteId::YM2151,
                instance: 0,
                page: 0,
                write: ndsif::RegisterWrite::new(0x20, 0xc7),
                sample_frame: 0,
                sample_rate: 44_100,
            })
            .unwrap();
        recorder
            .schedule_psg_write(NdsifPsgEvent {
                write_id: WriteId::SN76489,
                instance: 0,
                page: 0,
                sample_frame: 0,
                sample_rate: 44_100,
                value: 0x90,
            })
            .unwrap();
        recorder
            .configure_audio_stream(0, 4_000_000, Divider::Div1024, 44_100)
            .unwrap();
        recorder
            .schedule_adpcm_data(NdsifAdpcmEvent {
                instance: 0,
                position: BytePosition::new(0),
                sample_frame: 0,
                sample_rate: 44_100,
                value: 0x12,
            })
            .unwrap();
        recorder
            .schedule_audio_control(NdsifAudioControlEvent {
                instance: 0,
                position: BytePosition::new(0),
                sample_frame: 0,
                sample_rate: 44_100,
                control: NdsifAudioControl::Pan(Pan::Both),
            })
            .unwrap();
        recorder.flush_scheduled_until(100, 44_100).unwrap();
        recorder.start_audio_stream(0).unwrap();
        recorder.finish_audio_stream(0, 100, 44_100).unwrap();
        recorder.finish_dac_stream(100, 44_100).unwrap();
    }

    #[test]
    fn replay_preserves_every_operation_and_batch_order() {
        let mut recorder = recorder(10);
        all_operations(&mut recorder);
        let mut expected = recorder.operations.clone();
        let first = recorder.take_batch().unwrap();
        assert_eq!(first.len(), 10);
        assert!(recorder.take_batch().unwrap().is_empty());
        recorder.flush_scheduled_until(200, 44_100).unwrap();
        expected.push(Operation::Flush(200, 44_100));
        let second = recorder.take_batch().unwrap();
        let mut sink = Sink::default();
        first.replay(&mut sink).unwrap();
        second.replay(&mut sink).unwrap();
        assert_eq!(sink.calls, expected);
    }

    #[test]
    fn batches_can_be_replayed_on_a_separate_worker() {
        let mut recorder = recorder(1);
        recorder.flush_scheduled_until(42, 44_100).unwrap();
        let batch = recorder.take_batch().unwrap();
        let calls = std::thread::spawn(move || {
            let mut sink = Sink::default();
            batch.replay(&mut sink).unwrap();
            sink.calls
        })
        .join()
        .unwrap();
        assert_eq!(calls, [Operation::Flush(42, 44_100)]);
    }

    #[test]
    fn recording_limit_fails_closed_and_replay_stops_on_error() {
        let mut bounded = recorder(1);
        bounded.flush_scheduled_until(10, 44_100).unwrap();
        assert!(bounded.flush_scheduled_until(20, 44_100).is_err());
        assert_eq!(bounded.take_batch().err().unwrap().limit.get(), 1);
        assert!(bounded.flush_scheduled_until(30, 44_100).is_err());

        let mut recorder = recorder(10);
        all_operations(&mut recorder);
        let expected = recorder.operations[..3].to_vec();
        let mut sink = Sink {
            calls: Vec::new(),
            fail_at: Some(3),
        };
        let error = recorder
            .take_batch()
            .unwrap()
            .replay(&mut sink)
            .unwrap_err();
        assert_eq!(error.index, 3);
        assert_eq!(error.message, "device failed");
        assert_eq!(sink.calls, expected);
    }
}

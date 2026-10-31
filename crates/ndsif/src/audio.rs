use crate::{AudioStatus, AudioTiming, BytePosition, Error, MAX_AUDIO_DATA_BYTES};

/// Local protocol state for one continuous NDSIF ADPCM stream.
///
/// This type tracks byte positions and stream progress but performs no I/O,
/// allocation, scheduling, or device communication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioStreamState {
    timing: AudioTiming,
    output_rate: u32,
    accepted: BytePosition,
    played: BytePosition,
    last_event_position: Option<BytePosition>,
    started: bool,
    ended: bool,
}

impl AudioStreamState {
    pub fn new(timing: AudioTiming, output_rate: u32) -> Result<Self, Error> {
        if output_rate == 0 {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            timing,
            output_rate,
            accepted: BytePosition::new(0),
            played: BytePosition::new(0),
            last_event_position: None,
            started: false,
            ended: false,
        })
    }

    pub const fn timing(self) -> AudioTiming {
        self.timing
    }

    pub const fn output_rate(self) -> u32 {
        self.output_rate
    }

    pub const fn accepted(self) -> BytePosition {
        self.accepted
    }

    pub const fn played(self) -> BytePosition {
        self.played
    }

    pub const fn pending_bytes(self) -> u32 {
        self.accepted.get() - self.played.get()
    }

    pub const fn started(self) -> bool {
        self.started
    }

    pub const fn ended(self) -> bool {
        self.ended
    }

    /// Maps an absolute output frame to its containing ADPCM byte position.
    pub fn position_for_frame(
        &self,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<BytePosition, Error> {
        if output_rate != self.output_rate {
            return Err(Error::InvalidArgument);
        }
        let ticks = u64::try_from(sample_frame).map_err(|_| Error::InvalidArgument)?;
        let position = self.timing.bytes_from_ticks(ticks, output_rate)?;
        u32::try_from(position)
            .map(BytePosition::new)
            .map_err(|_| Error::InvalidArgument)
    }

    /// Returns whether the stream has enough accepted data to start.
    ///
    /// The prebuffer threshold is supplied by the caller because it is a
    /// playback policy, not a protocol constant.
    pub const fn should_start(&self, prebuffer_bytes: u32, force: bool) -> bool {
        !self.started
            && self.accepted.get() > 0
            && (force || self.accepted.get() >= prebuffer_bytes)
    }

    /// Records a successfully accepted contiguous AUDIO_DATA chunk.
    pub fn record_data_accepted(
        &mut self,
        position: BytePosition,
        bytes: usize,
    ) -> Result<(), Error> {
        if self.ended || position != self.accepted || bytes == 0 || bytes > MAX_AUDIO_DATA_BYTES {
            return Err(Error::InvalidArgument);
        }
        self.accepted = position.checked_advance(bytes)?;
        Ok(())
    }

    /// Checks whether an event position is covered by accepted audio data.
    pub const fn event_is_ready(&self, position: BytePosition) -> bool {
        !self.ended && position.get() <= self.accepted.get()
    }

    /// Validates event order before sending an AUDIO_EVENT.
    pub fn validate_event_position(&self, position: BytePosition) -> Result<(), Error> {
        if self.ended || self.last_event_position.is_some_and(|last| position < last) {
            return Err(Error::InvalidArgument);
        }
        Ok(())
    }

    /// Records an AUDIO_EVENT after it has been successfully written.
    pub fn record_event_position(&mut self, position: BytePosition) -> Result<(), Error> {
        self.validate_event_position(position)?;
        self.last_event_position = Some(position);
        Ok(())
    }

    /// Validates the final event, which must be positioned at accepted audio.
    pub fn validate_end_position(&self, position: BytePosition) -> Result<(), Error> {
        if position != self.accepted {
            return Err(Error::InvalidArgument);
        }
        self.validate_event_position(position)
    }

    /// Records a successfully written END event.
    pub fn record_end(&mut self, position: BytePosition) -> Result<(), Error> {
        self.validate_end_position(position)?;
        self.last_event_position = Some(position);
        self.ended = true;
        Ok(())
    }

    /// Applies a non-faulted AUDIO_STATUS response to the local stream state.
    pub fn update_status(&mut self, status: AudioStatus) -> Result<(), Error> {
        if status.accepted != self.accepted.get()
            || status.played > status.accepted
            || status.pending != status.accepted - status.played
        {
            return Err(Error::InvalidArgument);
        }
        self.played = BytePosition::new(status.played);
        self.started |= status.flags.running();
        self.ended |= status.flags.ended();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Divider, OkiClock, StatusFlags};

    fn status(accepted: u32, played: u32, running: bool) -> AudioStatus {
        AudioStatus {
            accepted,
            played,
            pending: accepted - played,
            underflows: 0,
            overflows: 0,
            rejected: 0,
            max_pending: accepted,
            flags: if running {
                StatusFlags::RUNNING
            } else {
                StatusFlags::default()
            },
            late_events: 0,
            max_event_lag: 0,
        }
    }

    #[test]
    fn maps_output_frames_to_adpcm_byte_positions() {
        let state =
            AudioStreamState::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100)
                .unwrap();

        assert_eq!(
            state.position_for_frame(441, 44_100).unwrap(),
            BytePosition::new(19)
        );
        assert_eq!(
            state.position_for_frame(882, 44_100).unwrap(),
            BytePosition::new(39)
        );
        assert_eq!(
            state.position_for_frame(441, 48_000),
            Err(Error::InvalidArgument)
        );
    }

    #[test]
    fn tracks_contiguous_data_and_event_positions() {
        let mut state =
            AudioStreamState::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100)
                .unwrap();
        state.record_data_accepted(BytePosition::new(0), 2).unwrap();
        assert_eq!(state.accepted(), BytePosition::new(2));
        assert!(state.event_is_ready(BytePosition::new(2)));
        assert!(!state.event_is_ready(BytePosition::new(3)));

        state.record_event_position(BytePosition::new(1)).unwrap();
        state.record_event_position(BytePosition::new(1)).unwrap();
        assert_eq!(
            state.record_event_position(BytePosition::new(0)),
            Err(Error::InvalidArgument)
        );
        assert_eq!(
            state.record_data_accepted(BytePosition::new(0), 1),
            Err(Error::InvalidArgument)
        );
    }

    #[test]
    fn status_updates_progress_and_start_state() {
        let mut state =
            AudioStreamState::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100)
                .unwrap();
        state.record_data_accepted(BytePosition::new(0), 4).unwrap();

        assert!(state.should_start(4, false));
        state.update_status(status(4, 1, true)).unwrap();
        assert_eq!(state.played(), BytePosition::new(1));
        assert_eq!(state.pending_bytes(), 3);
        assert!(!state.should_start(1, true));
        assert_eq!(
            state.update_status(status(3, 1, true)),
            Err(Error::InvalidArgument)
        );
    }

    #[test]
    fn end_requires_the_accepted_position_and_closes_the_stream() {
        let mut state =
            AudioStreamState::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100)
                .unwrap();
        state.record_data_accepted(BytePosition::new(0), 2).unwrap();
        assert_eq!(
            state.validate_end_position(BytePosition::new(1)),
            Err(Error::InvalidArgument)
        );
        state.record_end(BytePosition::new(2)).unwrap();
        assert!(state.ended());
        assert_eq!(
            state.record_data_accepted(BytePosition::new(2), 1),
            Err(Error::InvalidArgument)
        );
        assert_eq!(
            state.record_event_position(BytePosition::new(2)),
            Err(Error::InvalidArgument)
        );
        assert!(state.should_start(0, true));
    }
}

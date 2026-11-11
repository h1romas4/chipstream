//! Local bookkeeping for ADPCM stream positions and lifecycle.
//!
//! This module neither performs I/O nor validates remote device state.

use crate::{AudioStatus, AudioTiming, BytePosition, Error, MAX_AUDIO_DATA_BYTES};

// Common denominator for /512, /768, and /1024 without losing byte fractions.
const DIVIDER_LCM: u128 = 3072;

/// A piece of an ADPCM byte timeline, retaining fractional bytes across changes.
///
/// The clock change is quantized to an ADPCM byte position. `minimum_position`
/// accounts for bytes already scheduled ahead of the nominal frame position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPositionClock {
    timing: AudioTiming,
    output_rate: u32,
    frame: usize,
    numerator: u128,
}

impl AudioPositionClock {
    /// Create a timeline starting at frame and byte position zero.
    ///
    /// # Errors
    ///
    /// Returns an error if `output_rate` is zero.
    pub fn new(timing: AudioTiming, output_rate: u32) -> Result<Self, Error> {
        if output_rate == 0 {
            return Err(Error::InvalidArgument);
        }
        Ok(Self {
            timing,
            output_rate,
            frame: 0,
            numerator: 0,
        })
    }

    /// Return the common denominator for the clock-to-position fraction.
    fn denominator(&self) -> u128 {
        u128::from(self.output_rate) * DIVIDER_LCM * 2
    }

    /// Compute the accumulated numerator at an output frame.
    fn numerator_at(&self, frame: usize, output_rate: u32) -> Result<u128, Error> {
        if output_rate != self.output_rate || frame < self.frame {
            return Err(Error::InvalidArgument);
        }
        let weight =
            u128::from(self.timing.clock() as u32) * (DIVIDER_LCM / self.timing.divider() as u128);
        let elapsed = (frame - self.frame) as u128;
        self.numerator
            .checked_add(elapsed.checked_mul(weight).ok_or(Error::InvalidArgument)?)
            .ok_or(Error::InvalidArgument)
    }

    /// Return the containing ADPCM byte at an absolute output frame.
    ///
    /// # Errors
    ///
    /// Returns an error for an earlier frame, a mismatched rate, or overflow.
    pub fn position_for_frame(
        &self,
        frame: usize,
        output_rate: u32,
    ) -> Result<BytePosition, Error> {
        let position = self.numerator_at(frame, output_rate)? / self.denominator();
        u32::try_from(position)
            .map(BytePosition::new)
            .map_err(|_| Error::InvalidArgument)
    }

    /// Return the first frame at which `position` is reached, assuming this
    /// segment's timing remains in effect.
    ///
    /// Positions already reached at the beginning of the segment map to the
    /// segment's first frame.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame position overflows.
    pub fn first_frame_for_position(&self, position: BytePosition) -> Result<usize, Error> {
        let target = u128::from(position.get())
            .checked_mul(self.denominator())
            .ok_or(Error::InvalidArgument)?;
        let remaining = target.saturating_sub(self.numerator);
        let weight =
            u128::from(self.timing.clock() as u32) * (DIVIDER_LCM / self.timing.divider() as u128);
        let elapsed = remaining.div_ceil(weight);
        let elapsed = usize::try_from(elapsed).map_err(|_| Error::InvalidArgument)?;
        self.frame
            .checked_add(elapsed)
            .ok_or(Error::InvalidArgument)
    }

    /// Produce the next timeline segment at a byte-positioned clock change.
    ///
    /// # Errors
    ///
    /// Returns an error for an earlier frame, a mismatched rate, a position
    /// behind the nominal timeline, or overflow.
    pub fn change_at(
        &self,
        frame: usize,
        output_rate: u32,
        position: BytePosition,
        timing: AudioTiming,
    ) -> Result<Self, Error> {
        let numerator = self.numerator_at(frame, output_rate)?;
        let denominator = self.denominator();
        let nominal = numerator / denominator;
        if u128::from(position.get()) < nominal {
            return Err(Error::InvalidArgument);
        }
        let numerator = numerator.max(u128::from(position.get()) * denominator);
        Ok(Self {
            timing,
            output_rate,
            frame,
            numerator,
        })
    }

    /// Return the first frame covered by this timeline segment.
    pub const fn frame(&self) -> usize {
        self.frame
    }
}

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
    /// Create a stream with zero positions and a nonzero host output rate.
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

    /// Return the ADPCM clock and divider used for position conversion.
    pub const fn timing(self) -> AudioTiming {
        self.timing
    }

    /// Return the host output frame rate in frames per second.
    pub const fn output_rate(self) -> u32 {
        self.output_rate
    }

    /// Return the end position of audio data recorded as accepted.
    pub const fn accepted(self) -> BytePosition {
        self.accepted
    }

    /// Return the most recently reported played byte position.
    pub const fn played(self) -> BytePosition {
        self.played
    }

    /// Return the accepted but not yet played byte count.
    pub const fn pending_bytes(self) -> u32 {
        self.accepted.get() - self.played.get()
    }

    /// Return whether a status has reported the stream running.
    pub const fn started(self) -> bool {
        self.started
    }

    /// Return whether an end event was recorded or status reported an end.
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

    /// Applies AUDIO_STATUS position and lifecycle fields to the local state.
    ///
    /// The caller must check `status.flags.fault()` separately; this method
    /// validates position consistency but does not reject faulted status.
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

    #[test]
    fn position_clock_matches_fixed_timing_for_all_supported_dividers() {
        for clock in [OkiClock::Mhz4, OkiClock::Mhz8] {
            for divider in [Divider::Div512, Divider::Div768, Divider::Div1024] {
                let timing = AudioTiming::new(clock, divider);
                let timeline = AudioPositionClock::new(timing, 44_100).unwrap();
                for frame in [0, 1, 123, 4410, 44_100, 1_000_000] {
                    assert_eq!(
                        u64::from(timeline.position_for_frame(frame, 44_100).unwrap().get()),
                        timing.bytes_from_ticks(frame as u64, 44_100).unwrap()
                    );
                }
            }
        }
        assert!(
            AudioPositionClock::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 0).is_err()
        );
    }

    #[test]
    fn first_frame_for_position_matches_forward_timeline() {
        for rate in [44_100, 48_000, 96_000] {
            for clock in [OkiClock::Mhz4, OkiClock::Mhz8] {
                for divider in [Divider::Div512, Divider::Div768, Divider::Div1024] {
                    let initial =
                        AudioPositionClock::new(AudioTiming::new(clock, divider), rate).unwrap();
                    let position = initial.position_for_frame(4410, rate).unwrap();
                    let changed = initial
                        .change_at(
                            4410,
                            rate,
                            position,
                            AudioTiming::new(clock, Divider::Div768),
                        )
                        .unwrap();
                    for segment in [initial, changed] {
                        let start = segment
                            .position_for_frame(segment.frame(), rate)
                            .unwrap()
                            .get();
                        for offset in [0, 1, 2, 101, 100_000] {
                            let position = BytePosition::new(start + offset);
                            let frame = segment.first_frame_for_position(position).unwrap();
                            assert!(frame >= segment.frame());
                            assert!(
                                segment.position_for_frame(frame, rate).unwrap().get()
                                    >= position.get()
                            );
                            if frame > segment.frame() {
                                assert!(
                                    segment.position_for_frame(frame - 1, rate).unwrap().get()
                                        < position.get()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn position_clock_preserves_fractional_bytes_across_rate_changes() {
        let initial =
            AudioPositionClock::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100)
                .unwrap();
        let first = initial.position_for_frame(4410, 44_100).unwrap();
        assert_eq!(first.get(), 195);
        let faster = initial
            .change_at(
                4410,
                44_100,
                first,
                AudioTiming::new(OkiClock::Mhz8, Divider::Div768),
            )
            .unwrap();
        assert_eq!(faster.position_for_frame(6615, 44_100).unwrap().get(), 455);
        let second = faster.position_for_frame(8820, 44_100).unwrap();
        assert_eq!(second.get(), 716);
        let next = faster
            .change_at(
                8820,
                44_100,
                second,
                AudioTiming::new(OkiClock::Mhz4, Divider::Div512),
            )
            .unwrap();
        assert_eq!(next.position_for_frame(13230, 44_100).unwrap().get(), 1106);
        assert!(next.position_for_frame(8819, 44_100).is_err());
        assert!(next.position_for_frame(8820, 48_000).is_err());
        assert!(
            initial
                .change_at(
                    4410,
                    44_100,
                    BytePosition::new(194),
                    AudioTiming::new(OkiClock::Mhz4, Divider::Div512),
                )
                .is_err()
        );
    }

    #[test]
    fn position_clock_accounts_for_audio_already_queued_ahead() {
        let initial =
            AudioPositionClock::new(AudioTiming::new(OkiClock::Mhz4, Divider::Div1024), 44_100)
                .unwrap();
        let next = initial
            .change_at(
                0,
                44_100,
                BytePosition::new(3),
                AudioTiming::new(OkiClock::Mhz4, Divider::Div512),
            )
            .unwrap();
        assert_eq!(next.position_for_frame(0, 44_100).unwrap().get(), 3);
        assert_eq!(next.position_for_frame(4410, 44_100).unwrap().get(), 393);
    }

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

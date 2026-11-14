//! Explicit sessions for draft NDSIF live profiles.

use super::*;
use ndsif::{
    CONTROL_FEATURE_LIVE_DAC, CONTROL_FEATURE_ORDERED_LIVE, LIVE_DAC_PROFILE,
    LIVE_TICKS_PER_SECOND, LiveDacRecord, ORDERED_LIVE_PROFILE, OrderedLiveCapabilities,
    OrderedLiveRecord, OrderedLiveStatus, OrderedLiveWrite, RegisterWrite,
};

#[cfg(all(test, unix))]
#[path = "../../tests/unit/live.rs"]
mod tests;

fn ym_write_id(page: u8) -> Option<WriteId> {
    match page {
        0 => Some(WriteId::YM2612),
        1 => WriteId::try_from(0x53).ok(),
        _ => None,
    }
}

fn supports_live_write(
    transport: &NdsifSerialTransport,
    id: WriteId,
    instance: u8,
    page: u8,
) -> bool {
    transport.capabilities.iter().any(|entry| {
        entry.write_id == id.get()
            && entry.instance == instance
            && entry.page == page
            && entry.features & 1 != 0
    })
}

impl NdsifSerialTransport {
    pub(super) fn ensure_scheduled_mode(&mut self) -> Result<(), String> {
        if self.live_needs_reset {
            return Err(NdsifTransportError::LiveResetRequired.to_string());
        }
        self.live_reset_ready = false;
        Ok(())
    }

    fn ensure_live_ready(&self) -> Result<(), NdsifTransportError> {
        if !self.live_reset_ready
            || self.live_needs_reset
            || self.configured_chip
            || self.audio_stream.is_some()
            || self.timed_dac_stream.is_some()
            || !self.pending.is_empty()
            || self.host_timeline.is_some()
        {
            return Err(NdsifTransportError::LiveResetRequired);
        }
        Ok(())
    }

    /// Configure a chip clock after reset and before beginning a live session.
    ///
    /// Unlike scheduled playback configuration, this does not create a timed
    /// DAC stream. For a fixed-clock chip the requested clock must match.
    ///
    /// # Errors
    ///
    /// Returns an error when reset is required, the clock is unsupported, or
    /// the device rejects or fails to receive the clock command.
    pub fn configure_live_clock(
        &mut self,
        write_id: WriteId,
        instance: u8,
        clock_hz: u32,
    ) -> Result<(), NdsifTransportError> {
        self.ensure_live_ready()?;
        if clock_hz == 0 || !self.supports_clock(write_id, instance, clock_hz) {
            return Err(NdsifTransportError::InvalidLiveOperation {
                reason: "chip or clock is not advertised",
            });
        }
        let capability = self.capability(write_id, instance).ok_or(
            NdsifTransportError::InvalidLiveOperation {
                reason: "chip is not advertised",
            },
        )?;
        if capability.clock_mode == ClockMode::Variable {
            self.live_reset_ready = false;
            self.live_needs_reset = true;
            self.request_typed(Command::SetChipClock {
                write_id,
                instance,
                hz: clock_hz,
                response: true,
            })?;
            self.live_needs_reset = false;
            self.live_reset_ready = true;
        }
        Ok(())
    }

    /// Discover and begin a draft profile 2 live-DAC session.
    ///
    /// A successful reset is required before this call; configure clocks with
    /// [`Self::configure_live_clock`] after resetting. Dropping the session
    /// does not reset the device. Call [`Self::reset_for_reuse`] before
    /// scheduled playback or a new live session.
    ///
    /// # Errors
    ///
    /// Returns an error if reset is required, the device or target does not
    /// support the profile, or capability discovery fails validation or I/O.
    pub fn begin_live_dac(
        &mut self,
        instance: u8,
    ) -> Result<NdsifLiveDacSession<'_>, NdsifTransportError> {
        self.ensure_live_ready()?;
        if self.control_features & CONTROL_FEATURE_LIVE_DAC == 0 {
            return Err(NdsifTransportError::UnsupportedLiveProfile {
                profile: LIVE_DAC_PROFILE,
            });
        }
        let mut offset = 0;
        let mut metadata = None;
        let mut targets = Vec::new();
        loop {
            let frame = self.request_typed(Command::GetLiveDacCapabilities { offset })?;
            let response =
                Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                    operation: "invalid NDSIF GET_LIVE_DAC_CAPABILITIES response",
                    source,
                })?;
            let Reply::LiveDacCapabilities(page) = response.reply else {
                return Err(NdsifTransportError::UnexpectedResponse {
                    operation: "GET_LIVE_DAC_CAPABILITIES",
                });
            };
            if page.profile != LIVE_DAC_PROFILE
                || page.revision != 1
                || page.ticks_per_second != LIVE_TICKS_PER_SECOND
                || usize::from(page.max_records)
                    > usize::from(self.max_payload).saturating_sub(3) / 5
            {
                return Err(NdsifTransportError::InvalidLiveCapabilities {
                    reason: "unsupported live-DAC profile, revision, rate, or limit",
                });
            }
            let current = (
                page.profile,
                page.revision,
                page.ticks_per_second,
                page.record_capacity,
                page.max_records,
            );
            if metadata.is_some_and(|previous| previous != current) {
                return Err(NdsifTransportError::InvalidLiveCapabilities {
                    reason: "live-DAC capability pages disagree",
                });
            }
            metadata = Some(current);
            for target in page.entries() {
                if target.0 != WriteId::YM2612.get() || targets.contains(&target) {
                    return Err(NdsifTransportError::InvalidLiveCapabilities {
                        reason: "invalid or duplicate live-DAC target",
                    });
                }
                targets.push(target);
            }
            if page.next_offset == u16::MAX {
                break;
            }
            if page.next_offset <= offset {
                return Err(NdsifTransportError::InvalidCapabilitiesPagination);
            }
            offset = page.next_offset;
        }
        if !targets.contains(&(WriteId::YM2612.get(), instance)) {
            return Err(NdsifTransportError::InvalidLiveOperation {
                reason: "YM2612 live-DAC instance is not advertised",
            });
        }
        let (_, _, _, record_capacity, max_records) =
            metadata.expect("at least one capability page");
        self.live_needs_reset = true;
        self.live_reset_ready = false;
        Ok(NdsifLiveDacSession {
            transport: self,
            instance,
            max_records,
            record_capacity,
            first: true,
            poisoned: false,
        })
    }

    /// Discover and begin draft profile 3, revision 4 or 5.
    ///
    /// A successful reset is required before this call. Dropping the session
    /// does not reset the device; call [`Self::reset_for_reuse`] before reuse.
    ///
    /// # Errors
    ///
    /// Returns an error if reset is required, the profile is unavailable or
    /// unsupported, or capability discovery fails validation or I/O.
    pub fn begin_ordered_live(
        &mut self,
    ) -> Result<NdsifOrderedLiveSession<'_>, NdsifTransportError> {
        self.ensure_live_ready()?;
        if self.control_features & CONTROL_FEATURE_ORDERED_LIVE == 0 {
            return Err(NdsifTransportError::UnsupportedLiveProfile {
                profile: ORDERED_LIVE_PROFILE,
            });
        }
        let frame = self.request_typed(Command::GetOrderedLiveCapabilities)?;
        let response =
            Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid NDSIF GET_ORDERED_LIVE_CAPABILITIES response",
                source,
            })?;
        let Reply::OrderedLiveCapabilities(capabilities) = response.reply else {
            return Err(NdsifTransportError::UnexpectedResponse {
                operation: "GET_ORDERED_LIVE_CAPABILITIES",
            });
        };
        if capabilities.profile != ORDERED_LIVE_PROFILE
            || !matches!(capabilities.revision, 4 | 5)
            || capabilities.ticks_per_second != LIVE_TICKS_PER_SECOND
            || usize::from(capabilities.max_records)
                > usize::from(self.max_payload).saturating_sub(5) / 2
        {
            return Err(NdsifTransportError::InvalidLiveCapabilities {
                reason: "unsupported ordered-live profile, revision, rate, or limit",
            });
        }
        if !supports_live_write(self, WriteId::YM2612, 0, 0) {
            return Err(NdsifTransportError::InvalidLiveCapabilities {
                reason: "primary YM2612 is not advertised",
            });
        }
        self.live_needs_reset = true;
        self.live_reset_ready = false;
        Ok(NdsifOrderedLiveSession {
            transport: self,
            capabilities,
            sequence: 0,
            first: true,
            poisoned: false,
        })
    }
}

/// Exclusive profile 2 session. Writes are unacknowledged and can be dropped
/// by a full device FIFO, even when the serial write succeeds.
pub struct NdsifLiveDacSession<'a> {
    transport: &'a mut NdsifSerialTransport,
    instance: u8,
    max_records: u8,
    record_capacity: u32,
    first: bool,
    poisoned: bool,
}

impl NdsifLiveDacSession<'_> {
    /// Advertised device FIFO capacity in records, not its current free space.
    pub fn record_capacity(&self) -> u32 {
        self.record_capacity
    }

    /// Maximum records in a single data frame.
    pub fn max_records(&self) -> u8 {
        self.max_records
    }

    /// Send one frame of DAC records. The first wait must be zero.
    ///
    /// A successful return only confirms the serial write, not device
    /// acceptance or playback. After an I/O failure, reset or close rather
    /// than retrying an ambiguously delivered frame.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid frame, a previous ambiguous write, or
    /// a failed serial write.
    pub fn send(&mut self, records: &[LiveDacRecord]) -> Result<(), NdsifTransportError> {
        if self.poisoned {
            return Err(NdsifTransportError::LiveWriteIndeterminate);
        }
        if records.is_empty()
            || records.len() > usize::from(self.max_records)
            || (self.first && records[0].wait_ticks != 0)
        {
            return Err(NdsifTransportError::InvalidLiveOperation {
                reason: "invalid live-DAC record count or initial wait",
            });
        }
        self.poisoned = true;
        self.transport.request_typed(Command::LiveDacData {
            instance: self.instance,
            records,
        })?;
        self.poisoned = false;
        self.first = false;
        Ok(())
    }

    /// Send immediate YM2612 register writes, excluding page 0 `$2A`.
    ///
    /// These writes are not ordered relative to queued DAC samples.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported write, a previous ambiguous write,
    /// or a failed serial write.
    pub fn write_registers(
        &mut self,
        page: u8,
        writes: &[RegisterWrite],
    ) -> Result<(), NdsifTransportError> {
        if self.poisoned {
            return Err(NdsifTransportError::LiveWriteIndeterminate);
        }
        let Some(write_id) = ym_write_id(page) else {
            return Err(NdsifTransportError::InvalidLiveOperation {
                reason: "unsupported YM2612 page",
            });
        };
        if writes.is_empty()
            || (page == 0 && writes.iter().any(|write| write.address == 0x2a))
            || 2 + writes.len() * 2 > usize::from(self.transport.max_payload)
            || !supports_live_write(self.transport, write_id, self.instance, page)
        {
            return Err(NdsifTransportError::InvalidLiveOperation {
                reason: "unsupported immediate YM2612 register write",
            });
        }
        let command = Command::WriteRegisters {
            write_id,
            instance: self.instance,
            page,
            writes,
        };
        command
            .to_frame(0)
            .map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid immediate YM2612 write",
                source,
            })?;
        self.poisoned = true;
        self.transport.request_typed(command)?;
        self.poisoned = false;
        Ok(())
    }

    /// Send immediate PSG bytes; these are not ordered relative to DAC samples.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported write, a previous ambiguous write,
    /// or a failed serial write.
    pub fn write_psg(&mut self, instance: u8, bytes: &[u8]) -> Result<(), NdsifTransportError> {
        if self.poisoned {
            return Err(NdsifTransportError::LiveWriteIndeterminate);
        }
        if bytes.is_empty()
            || bytes.len() > usize::from(self.transport.max_payload).saturating_sub(2)
            || !supports_live_write(self.transport, WriteId::SN76489, instance, 0)
        {
            return Err(NdsifTransportError::InvalidLiveOperation {
                reason: "unsupported immediate PSG write",
            });
        }
        let command = Command::WriteBytes {
            write_id: WriteId::SN76489,
            instance,
            page: 0,
            bytes,
        };
        command
            .to_frame(0)
            .map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid immediate PSG write",
                source,
            })?;
        self.poisoned = true;
        self.transport.request_typed(command)?;
        self.poisoned = false;
        Ok(())
    }
}

/// Exclusive profile 3 session with automatic frame sequencing.
pub struct NdsifOrderedLiveSession<'a> {
    transport: &'a mut NdsifSerialTransport,
    capabilities: OrderedLiveCapabilities,
    sequence: u32,
    first: bool,
    poisoned: bool,
}

impl NdsifOrderedLiveSession<'_> {
    /// Return the negotiated profile revision and device limits.
    pub fn capabilities(&self) -> OrderedLiveCapabilities {
        self.capabilities
    }

    /// Send one unacknowledged ordered frame. The first wait must be zero.
    ///
    /// Records must fit both the advertised record count and payload size.
    /// PSG writes require revision 5 and an advertised primary PSG. A
    /// successful return confirms only the serial write, not device playback.
    /// If writing fails, the session cannot be reused without a reset.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid records, unsupported targets, a previous
    /// ambiguous write, or a failed serial write.
    pub fn send(&mut self, records: &[OrderedLiveRecord]) -> Result<(), NdsifTransportError> {
        if self.poisoned {
            return Err(NdsifTransportError::LiveWriteIndeterminate);
        }
        if records.is_empty()
            || records.len() > usize::from(self.capabilities.max_records)
            || (self.first && records[0].wait_ticks != 0)
        {
            return Err(NdsifTransportError::InvalidLiveOperation {
                reason: "invalid ordered-live record count or initial wait",
            });
        }
        for record in records {
            let (write_id, page) = match record.write {
                OrderedLiveWrite::Dac(_) => (WriteId::YM2612, 0),
                OrderedLiveWrite::Register { page, .. } => (
                    ym_write_id(page).ok_or(NdsifTransportError::InvalidLiveOperation {
                        reason: "unsupported YM2612 page",
                    })?,
                    page,
                ),
                OrderedLiveWrite::Psg(_) if self.capabilities.revision == 5 => {
                    (WriteId::SN76489, 0)
                }
                OrderedLiveWrite::Psg(_) => {
                    return Err(NdsifTransportError::InvalidLiveOperation {
                        reason: "ordered PSG requires revision 5",
                    });
                }
            };
            if !supports_live_write(self.transport, write_id, 0, page) {
                return Err(NdsifTransportError::InvalidLiveOperation {
                    reason: "ordered target page is not advertised",
                });
            }
        }
        let command = Command::OrderedLiveData {
            sequence: self.sequence,
            records,
        };
        let frame = command
            .to_frame(0)
            .map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid ordered-live records",
                source,
            })?;
        if frame.payload().len() > usize::from(self.transport.max_payload) {
            return Err(NdsifTransportError::InvalidLiveOperation {
                reason: "ordered-live payload exceeds device limit",
            });
        }
        self.poisoned = true;
        self.transport.request_typed(command)?;
        self.poisoned = false;
        self.first = false;
        self.sequence = self.sequence.wrapping_add(1);
        Ok(())
    }

    /// Request a diagnostic snapshot. This is not a playback-completion fence.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid response, a previous ambiguous write,
    /// or a failed request. Reset or close after an unsuccessful status query.
    pub fn status(&mut self) -> Result<OrderedLiveStatus, NdsifTransportError> {
        if self.poisoned {
            return Err(NdsifTransportError::LiveWriteIndeterminate);
        }
        self.poisoned = true;
        let frame = self
            .transport
            .request_typed(Command::GetOrderedLiveStatus)?;
        let response =
            Response::decode(&frame).map_err(|source| NdsifTransportError::Protocol {
                operation: "invalid NDSIF GET_ORDERED_LIVE_STATUS response",
                source,
            })?;
        let Reply::OrderedLiveStatus(status) = response.reply else {
            return Err(NdsifTransportError::UnexpectedResponse {
                operation: "GET_ORDERED_LIVE_STATUS",
            });
        };
        if status.profile != ORDERED_LIVE_PROFILE
            || u32::from(status.queued) + u32::from(status.free)
                != u32::from(self.capabilities.record_capacity)
        {
            return Err(NdsifTransportError::InvalidLiveCapabilities {
                reason: "inconsistent ordered-live status",
            });
        }
        self.poisoned = false;
        Ok(status)
    }
}

use crate::{
    AudioEvent, BytePosition, Command, Error, MAX_AUDIO_DATA_BYTES, MAX_REGISTER_EVENT_WRITES,
    MAX_REGISTER_WRITES, RegisterWrite, WriteId,
};

/// Allocation-free frame sequencing into a caller-owned, infallible byte sink.
/// This encodes requests only; it does not send them or track device acceptance.
pub struct CommandEncoder<F> {
    emit: F,
    transaction_id: u16,
    count: usize,
}

impl<F: FnMut(&[u8])> CommandEncoder<F> {
    pub const fn new(transaction_id: u16, emit: F) -> Self {
        Self {
            emit,
            transaction_id,
            count: 0,
        }
    }

    pub const fn count(&self) -> usize {
        self.count
    }

    pub const fn next_transaction_id(&self) -> u16 {
        self.transaction_id
    }

    /// Invalid commands leave the sink, count and transaction ID unchanged.
    pub fn push(&mut self, command: Command<'_>) -> Result<(), Error> {
        let count = self.count.checked_add(1).ok_or(Error::InvalidArgument)?;
        let frame = command.encode(self.transaction_id)?;
        (self.emit)(frame.as_bytes());
        self.transaction_id = self.transaction_id.wrapping_add(1);
        self.count = count;
        Ok(())
    }

    /// Splits immediate register writes at the protocol limit.
    pub fn register_burst(
        &mut self,
        write_id: WriteId,
        instance: u8,
        page: u8,
        writes: &[RegisterWrite],
    ) -> Result<(), Error> {
        for chunk in writes.chunks(MAX_REGISTER_WRITES) {
            self.push(Command::WriteRegisters {
                write_id,
                instance,
                page,
                writes: chunk,
            })?;
        }
        Ok(())
    }

    /// Splits positioned register writes without changing their order or position.
    pub fn register_event(
        &mut self,
        stream_id: u8,
        stream_instance: u8,
        position: BytePosition,
        write_id: WriteId,
        instance: u8,
        page: u8,
        writes: &[RegisterWrite],
    ) -> Result<(), Error> {
        for chunk in writes.chunks(MAX_REGISTER_EVENT_WRITES) {
            self.push(Command::AudioEvent {
                stream_id,
                instance: stream_instance,
                position,
                event: AudioEvent::Registers {
                    write_id,
                    instance,
                    page,
                    writes: chunk,
                },
            })?;
        }
        Ok(())
    }

    /// Splits DATA into contiguous positions. Overflow is rejected before emission.
    pub fn audio_data(
        &mut self,
        stream_id: u8,
        instance: u8,
        position: BytePosition,
        adpcm: &[u8],
    ) -> Result<BytePosition, Error> {
        let end = position.checked_advance(adpcm.len())?;
        let mut position = position;
        for chunk in adpcm.chunks(MAX_AUDIO_DATA_BYTES) {
            self.push(Command::AudioData {
                stream_id,
                instance,
                position,
                adpcm: chunk,
            })?;
            position = position.checked_advance(chunk.len())?;
        }
        Ok(end)
    }
}

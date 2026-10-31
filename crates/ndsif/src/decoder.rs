use crate::{Error, Frame, frame::MAX_COBS_SIZE};

pub const PARTIAL_FRAME_TIMEOUT_MS: u64 = 500;

/// Incremental parser. Starts by discarding input until the first zero delimiter.
/// The caller owns the clock and expires incomplete frames after 500 ms of inactivity.
#[derive(Debug, Clone)]
pub struct Decoder {
    buffer: [u8; MAX_COBS_SIZE],
    length: usize,
    discarding: bool,
}

impl Decoder {
    pub const fn new() -> Self {
        Self {
            buffer: [0; MAX_COBS_SIZE],
            length: 0,
            discarding: true,
        }
    }

    /// Reset after a transport change; wait for a new synchronization delimiter.
    pub fn reset(&mut self) {
        self.length = 0;
        self.discarding = true;
    }

    pub const fn has_partial_frame(&self) -> bool {
        self.length != 0
    }

    /// Returns whether an incomplete frame was discarded. No clock or timer is started here.
    pub fn expire_partial_frame(&mut self) -> bool {
        if !self.has_partial_frame() {
            return false;
        }
        self.reset();
        true
    }

    pub fn push_byte(&mut self, byte: u8) -> Option<Result<Frame, Error>> {
        if byte == 0 {
            self.discarding = false;
            if self.length == 0 {
                return None;
            }
            let result = Frame::decode(&self.buffer[..self.length]);
            self.length = 0;
            return Some(result);
        }
        if self.discarding {
            return None;
        }
        if self.length == MAX_COBS_SIZE {
            self.reset();
            return Some(Err(Error::FrameTooLong));
        }
        self.buffer[self.length] = byte;
        self.length += 1;
        None
    }

    /// Delivers every completed frame or parse error, continuing through the entire chunk.
    pub fn push(&mut self, bytes: &[u8], mut on_frame: impl FnMut(Result<Frame, Error>)) {
        for &byte in bytes {
            if let Some(result) = self.push_byte(byte) {
                on_frame(result);
            }
        }
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

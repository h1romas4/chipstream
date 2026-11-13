//! Ordered ADPCM stream delivery for an OKIM6258.

use std::collections::VecDeque;

use soundlog::chip::{Chip, Okim6258Spec};

use super::{
    ChipError, ChipRegisterWrite, Okim6258, SampleRate, SoundChip, SoundChipState, StereoMute,
    restore_cloned_state, save_cloned_state,
};

const MAX_PENDING_BYTES: usize = 65_536;

/// Adapts DAC-stream writes to the native two-nibble OKIM6258 cadence.
///
/// Stream bytes are consumed in order, two native samples per byte. When no
/// byte is available, the last decoded sample is held without advancing the
/// predictor. Direct data writes retain the underlying latch behavior and
/// discard pending stream bytes. Chip playback stop clears the stream state.
/// Stopping a DAC stream discards queued bytes with that stream ID, or all
/// queued bytes for stop-all; a partially decoded byte finishes first.
/// Before the first stream write, the wrapped chip's latch behavior applies.
///
/// The queue is bounded to 65,536 bytes; overflow returns a configuration
/// error rather than dropping input. This adapter does not alter the decoder's
/// gain, precision, or register behavior.
#[derive(Clone)]
pub struct Okim6258Stream {
    chip: Okim6258,
    pending: VecDeque<(u8, u8)>,
    active: bool,
    high_nibble: bool,
    held_sample: i32,
}

impl Okim6258Stream {
    /// Wrap an already configured OKIM6258.
    pub fn new(chip: Okim6258) -> Self {
        Self {
            chip,
            pending: VecDeque::new(),
            active: false,
            high_nibble: false,
            held_sample: 0,
        }
    }

    fn clear_stream(&mut self) {
        self.pending.clear();
        self.active = false;
        self.high_nibble = false;
        self.held_sample = 0;
    }
}

impl SoundChip for Okim6258Stream {
    fn chip_type(&self) -> &Chip {
        self.chip.chip_type()
    }

    fn clock(&self) -> u32 {
        self.chip.clock()
    }

    fn sample_rate(&self) -> u32 {
        self.chip.sample_rate()
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        self.chip.sample_rate_ratio()
    }

    fn channels(&self) -> usize {
        self.chip.channels()
    }

    fn stereo_mute(&self) -> Option<StereoMute> {
        self.chip.stereo_mute()
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let clear = match &write {
            ChipRegisterWrite::Okim6258(Okim6258Spec { register: 1, .. }) => true,
            ChipRegisterWrite::Okim6258(Okim6258Spec { register: 0, value }) => {
                value & 1 != 0 || value & 2 == 0
            }
            _ => false,
        };
        self.chip.apply_register_write(write)?;
        if clear {
            self.clear_stream();
        }
        Ok(())
    }

    fn apply_dac_stream_write(
        &mut self,
        write: ChipRegisterWrite,
        stream_id: u8,
    ) -> Result<(), ChipError> {
        if let ChipRegisterWrite::Okim6258(Okim6258Spec { register: 1, value }) = write {
            if self.chip.status() != 0 {
                return self
                    .chip
                    .apply_register_write(ChipRegisterWrite::Okim6258(Okim6258Spec {
                        register: 1,
                        value,
                    }));
            }
            if self.pending.len() >= MAX_PENDING_BYTES {
                return Err(ChipError::InvalidConfiguration {
                    chip: Chip::Okim6258,
                    reason: "OKIM6258 stream input queue capacity exceeded",
                });
            }
            self.pending.push_back((stream_id, value));
            self.active = true;
            Ok(())
        } else {
            self.apply_register_write(write)
        }
    }

    fn stop_dac_stream(&mut self, stream_id: Option<u8>) -> Result<(), ChipError> {
        self.pending
            .retain(|(id, _)| stream_id.is_some_and(|stream_id| *id != stream_id));
        Ok(())
    }

    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        if !self.active {
            return self.chip.generate(samples);
        }
        for sample in samples {
            if !self.high_nibble {
                let Some((_, value)) = self.pending.pop_front() else {
                    *sample = self.held_sample;
                    continue;
                };
                self.chip.write(Okim6258Spec { register: 1, value })?;
            }
            self.chip.generate(std::slice::from_mut(sample))?;
            self.held_sample = *sample;
            self.high_nibble = !self.high_nibble;
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.chip.reset();
        self.clear_stream();
    }

    fn save_state(&mut self) -> Result<Box<dyn SoundChipState>, ChipError> {
        Ok(save_cloned_state(self))
    }

    fn restore_state(&mut self, state: &dyn SoundChipState) -> Result<(), ChipError> {
        restore_cloned_state(self, state, Chip::Okim6258)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chip::OutputBits;

    fn write(chip: &mut Okim6258Stream, register: u8, value: u8) {
        chip.write(Okim6258Spec { register, value }).unwrap();
    }

    fn stream(chip: &mut Okim6258Stream, value: u8) -> Result<(), ChipError> {
        chip.apply_dac_stream_write(
            ChipRegisterWrite::Okim6258(Okim6258Spec { register: 1, value }),
            0,
        )
    }

    #[test]
    fn streams_preserve_both_nibbles_and_hold_on_underrun() {
        let mut raw = Okim6258::new(8_000_000).unwrap();
        raw.set_output_bits(OutputBits::Bits12);
        let mut chip = Okim6258Stream::new(raw.clone());
        write(&mut chip, 0, 2);
        raw.write(Okim6258Spec {
            register: 0,
            value: 2,
        })
        .unwrap();
        stream(&mut chip, 0x71).unwrap();
        stream(&mut chip, 0xf2).unwrap();
        let mut expected = [0; 4];
        for (byte, samples) in [0x71, 0xf2].into_iter().zip(expected.chunks_mut(2)) {
            raw.write(Okim6258Spec {
                register: 1,
                value: byte,
            })
            .unwrap();
            raw.generate(samples).unwrap();
        }
        let mut actual = [0; 7];
        chip.generate(&mut actual[..1]).unwrap();
        chip.generate(&mut actual[1..]).unwrap();
        assert_eq!(&actual[..4], &expected);
        assert_eq!(&actual[4..], &[expected[3]; 3]);
        stream(&mut chip, 0x13).unwrap();
        let mut resumed = [0; 2];
        chip.generate(&mut resumed).unwrap();
        raw.write(Okim6258Spec {
            register: 1,
            value: 0x13,
        })
        .unwrap();
        let mut expected_resumed = [0; 2];
        raw.generate(&mut expected_resumed).unwrap();
        assert_eq!(resumed, expected_resumed);
    }

    #[test]
    fn direct_data_and_stop_cancel_pending_stream_bytes() {
        let mut chip = Okim6258Stream::new(Okim6258::new(8_000_000).unwrap());
        write(&mut chip, 0, 2);
        stream(&mut chip, 0x71).unwrap();
        stream(&mut chip, 0xf2).unwrap();
        let mut sample = [0];
        chip.generate(&mut sample).unwrap();
        write(&mut chip, 1, 0x21);
        assert!(!chip.active);
        assert!(chip.pending.is_empty());
        chip.generate(&mut sample).unwrap();
        assert_eq!(sample, [128]);
        stream(&mut chip, 0x77).unwrap();
        write(&mut chip, 0, 1);
        assert!(chip.pending.is_empty());
        chip.generate(&mut sample).unwrap();
        assert_eq!(sample, [0]);
    }

    #[test]
    fn snapshot_preserves_partial_byte_and_queue() {
        let mut chip = Okim6258Stream::new(Okim6258::new(8_000_000).unwrap());
        write(&mut chip, 0, 2);
        stream(&mut chip, 0x71).unwrap();
        stream(&mut chip, 0xf2).unwrap();
        chip.generate(&mut [0]).unwrap();
        let snapshot = chip.save_state().unwrap();
        let mut expected = [0; 8];
        chip.generate(&mut expected).unwrap();
        chip.restore_state(snapshot.as_ref()).unwrap();
        let mut actual = [0; 8];
        chip.generate(&mut actual).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn stopping_one_stream_preserves_other_streams_and_current_byte() {
        let mut chip = Okim6258Stream::new(Okim6258::new(8_000_000).unwrap());
        write(&mut chip, 0, 2);
        stream(&mut chip, 0x71).unwrap();
        chip.generate(&mut [0]).unwrap();
        stream(&mut chip, 0xf2).unwrap();
        chip.apply_dac_stream_write(
            ChipRegisterWrite::Okim6258(Okim6258Spec {
                register: 1,
                value: 0x43,
            }),
            1,
        )
        .unwrap();
        chip.stop_dac_stream(Some(0)).unwrap();
        assert_eq!(chip.pending, VecDeque::from([(1, 0x43)]));
        assert!(chip.high_nibble);
        chip.generate(&mut [0; 3]).unwrap();
        assert!(chip.pending.is_empty());
        stream(&mut chip, 0x12).unwrap();
        chip.stop_dac_stream(None).unwrap();
        assert!(chip.pending.is_empty());
    }

    #[test]
    fn overflowing_queue_returns_an_error_without_dropping_data() {
        let mut chip = Okim6258Stream::new(Okim6258::new(8_000_000).unwrap());
        write(&mut chip, 0, 2);
        for _ in 0..MAX_PENDING_BYTES {
            stream(&mut chip, 0x21).unwrap();
        }
        assert!(matches!(
            stream(&mut chip, 0x43),
            Err(ChipError::InvalidConfiguration {
                chip: Chip::Okim6258,
                ..
            })
        ));
        assert_eq!(chip.pending.len(), MAX_PENDING_BYTES);
        assert!(chip.pending.iter().all(|(_, byte)| *byte == 0x21));
    }
}

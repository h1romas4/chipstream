//! Stateful band-limited sample-rate conversion for stereo audio.

use std::collections::VecDeque;
use std::f32::consts::PI;

use crate::chip::SampleRate;
use crate::fixed::{advance, ceil_multiply_divide, divide_ratio, multiply_ratio};

use super::buffer::same_rate;
use super::{AudioError, StereoBlock};

const SINC_LENGTH: usize = 256;
const HALF: usize = SINC_LENGTH / 2;
const PHASES: usize = 128;

/// Stateful, band-limited stereo SRC for a bounded range of input rates.
///
/// The filter is designed for the highest allowed input rate. Input blocks must
/// be continuous and supplied in timeline order. The converter allocates and
/// needs lookahead; individual output blocks do not have a fixed duration.
/// Use one converter per source and include all actual input rates, including
/// fractional rates, in the constructor bounds. Bounds wider than necessary
/// can reduce high frequencies at lower rates.
///
/// Input rate changes apply at block boundaries without resetting filter
/// history. The filter spans both sides of the boundary, so alignment is
/// approximate. The `u32` fractional phase rounds conversion ratios to the
/// nearest 1/2^32; timing error can accumulate and durations at rounding
/// boundaries can differ by one frame from exact rational arithmetic.
/// The `f32` windowed-sinc coefficient table uses approximately 129 KiB per
/// resampler. This implementation requires `std` and allocation; it does not
/// guarantee support for a particular microcontroller.
///
/// Initial filter delay is removed from the returned timeline, but lookahead
/// is needed. [`Self::output_delay`] reports current lookahead in output
/// frames. Call [`Self::finish`] once to drain a finite stream; [`Self::reset`]
/// discards history for reuse. Output block lengths vary, so callers mixing
/// multiple sources must queue and align them.
pub struct StereoResampler {
    output_rate: u32,
    min_input_rate: u32,
    max_input_rate: u32,
    max_chunk_frames: usize,
    coefficients: Vec<f32>,
    input: VecDeque<([f32; 2], SampleRate)>,
    position: usize,
    phase: u32,
    phase_rate: Option<SampleRate>,
    phase_step: u32,
    pending_advance: bool,
    duration: u32,
    due: usize,
    last_rate: Option<SampleRate>,
    finished: bool,
}

pub(crate) struct StereoResamplerState {
    input: VecDeque<([f32; 2], SampleRate)>,
    position: usize,
    phase: u32,
    phase_rate: Option<SampleRate>,
    phase_step: u32,
    pending_advance: bool,
    duration: u32,
    due: usize,
    last_rate: Option<SampleRate>,
    finished: bool,
}

impl StereoResampler {
    /// Create a converter to `output_rate`, accepting the inclusive input-rate
    /// range and processing at most `max_chunk_frames` input frames at a time.
    pub fn new(
        output_rate: u32,
        min_input_rate: u32,
        max_input_rate: u32,
        max_chunk_frames: usize,
    ) -> Result<Self, AudioError> {
        if output_rate == 0 {
            return Err(AudioError::InvalidSampleRate);
        }
        if min_input_rate == 0 || max_input_rate < min_input_rate {
            return Err(AudioError::InvalidRateRange {
                min: min_input_rate,
                max: max_input_rate,
            });
        }
        let chunk =
            u32::try_from(max_chunk_frames).map_err(|_| AudioError::OutputLengthOverflow)?;
        if chunk == 0 {
            return Err(AudioError::InvalidChunkSize);
        }
        ceil_multiply_divide(chunk.max(HALF as u32), output_rate, min_input_rate)
            .ok_or(AudioError::OutputLengthOverflow)?;

        let cutoff = (output_rate as f32 / max_input_rate as f32).min(1.0) * 0.95;
        let mut coefficients = Vec::with_capacity((PHASES + 1) * SINC_LENGTH);
        for phase in 0..=PHASES {
            let start = coefficients.len();
            let fraction = phase as f32 / PHASES as f32;
            let mut sum = 0.0;
            for tap in 0..SINC_LENGTH {
                let distance = tap as f32 - (HALF - 1) as f32 - fraction;
                let x = PI * distance * cutoff;
                let sinc = if x.abs() < 1e-6 { 1.0 } else { x.sin() / x };
                let angle = 2.0 * PI * distance / SINC_LENGTH as f32;
                let window = 0.35875
                    + 0.48829 * angle.cos()
                    + 0.14128 * (2.0 * angle).cos()
                    + 0.01168 * (3.0 * angle).cos();
                let coefficient = cutoff * sinc * window;
                coefficients.push(coefficient);
                sum += coefficient;
            }
            for coefficient in &mut coefficients[start..] {
                *coefficient /= sum;
            }
        }
        Ok(Self {
            output_rate,
            min_input_rate,
            max_input_rate,
            max_chunk_frames,
            coefficients,
            input: VecDeque::new(),
            position: HALF - 1,
            phase: 0,
            phase_rate: None,
            phase_step: 0,
            pending_advance: false,
            duration: 1 << 31,
            due: 0,
            last_rate: None,
            finished: false,
        })
    }

    /// Convert a continuous stereo block, preserving filter history and phase.
    ///
    /// A rate change applies at the input block boundary without resetting the
    /// filter. The filter spans samples on both sides of that boundary.
    pub fn process(&mut self, input: &StereoBlock) -> Result<StereoBlock, AudioError> {
        if self.finished {
            return Err(AudioError::StreamFinished);
        }
        let rate = input.sample_rate();
        let denominator = rate.denominator().get();
        let integer = rate.numerator() / denominator;
        let remainder = rate.numerator() % denominator;
        if integer < self.min_input_rate
            || integer > self.max_input_rate
            || (integer == self.max_input_rate && remainder != 0)
        {
            return Err(AudioError::RateOutOfRange {
                rate,
                min: self.min_input_rate,
                max: self.max_input_rate,
            });
        }
        if input.frames().is_empty() {
            return StereoBlock::new(SampleRate::from_hz(self.output_rate), Vec::new());
        }
        if self.last_rate.is_none() {
            self.input
                .extend(std::iter::repeat_n(([0.0; 2], rate), HALF - 1));
        }
        let (whole_step, fraction_step) =
            multiply_ratio(self.output_rate, denominator, rate.numerator())
                .ok_or(AudioError::OutputLengthOverflow)?;
        self.last_rate = Some(rate);
        let mut output = Vec::new();
        for chunk in input.frames().chunks(self.max_chunk_frames) {
            for frame in chunk {
                let advance = advance(&mut self.duration, whole_step, fraction_step)
                    .ok_or(AudioError::OutputLengthOverflow)?;
                self.due = self
                    .due
                    .checked_add(advance as usize)
                    .ok_or(AudioError::OutputLengthOverflow)?;
                self.input.push_back((*frame, rate));
            }
            self.generate(&mut output)?;
        }
        StereoBlock::new(SampleRate::from_hz(self.output_rate), output)
    }

    /// Drain the filter and return the remaining fixed-point duration, rounded
    /// to frames. Fixed-point rounding can differ from exact rational duration.
    pub fn finish(&mut self) -> Result<StereoBlock, AudioError> {
        if self.finished {
            return Err(AudioError::StreamFinished);
        }
        let mut output = Vec::new();
        if let Some(rate) = self.last_rate {
            self.input
                .extend(std::iter::repeat_n(([0.0; 2], rate), SINC_LENGTH));
            self.generate(&mut output)?;
        }
        if self.due != 0 {
            return Err(AudioError::ResamplerDrainIncomplete);
        }
        self.finished = true;
        StereoBlock::new(SampleRate::from_hz(self.output_rate), output)
    }

    /// Filter lookahead in output frames at the most recent input rate.
    pub fn output_delay(&self) -> usize {
        let rate = self
            .last_rate
            .unwrap_or(SampleRate::from_hz(self.max_input_rate));
        let mut clock = 0;
        let (whole, fraction) =
            multiply_ratio(self.output_rate, rate.denominator().get(), rate.numerator())
                .expect("validated sample rates fit the supported output range");
        let mut frames = 0_usize;
        for _ in 0..HALF {
            frames +=
                advance(&mut clock, whole, fraction).expect("validated output delay fits") as usize;
        }
        frames + usize::from(clock != 0)
    }

    /// Discard the stream and all filter history, allowing a new stream.
    pub fn reset(&mut self) {
        self.input.clear();
        self.position = HALF - 1;
        self.phase = 0;
        self.phase_rate = None;
        self.phase_step = 0;
        self.pending_advance = false;
        self.duration = 1 << 31;
        self.due = 0;
        self.last_rate = None;
        self.finished = false;
    }

    pub(crate) fn save_state(&self) -> StereoResamplerState {
        StereoResamplerState {
            input: self.input.clone(),
            position: self.position,
            phase: self.phase,
            phase_rate: self.phase_rate,
            phase_step: self.phase_step,
            pending_advance: self.pending_advance,
            duration: self.duration,
            due: self.due,
            last_rate: self.last_rate,
            finished: self.finished,
        }
    }

    pub(crate) fn restore_state(&mut self, state: &StereoResamplerState) {
        self.input.clone_from(&state.input);
        self.position = state.position;
        self.phase = state.phase;
        self.phase_rate = state.phase_rate;
        self.phase_step = state.phase_step;
        self.pending_advance = state.pending_advance;
        self.duration = state.duration;
        self.due = state.due;
        self.last_rate = state.last_rate;
        self.finished = state.finished;
    }

    fn generate(&mut self, output: &mut Vec<[f32; 2]>) -> Result<(), AudioError> {
        while self.due != 0 {
            if self.pending_advance {
                if !self.advance_position()? {
                    break;
                }
                self.pending_advance = false;
            }
            if !self
                .position
                .checked_add(HALF)
                .is_some_and(|end| end < self.input.len())
            {
                break;
            }
            let rate = self.input[self.position].1;
            if self.phase_rate.is_none_or(|old| !same_rate(old, rate)) {
                self.phase_step =
                    divide_ratio(rate.numerator(), rate.denominator().get(), self.output_rate)
                        .ok_or(AudioError::OutputLengthOverflow)?
                        .1;
                self.phase_rate = Some(rate);
            }
            let fraction = self.phase;
            let phase = (fraction >> 25) as usize;
            let blend = (fraction & 0x01ff_ffff) as f32 / 33_554_432.0;
            let start = self.position - (HALF - 1);
            let mut sample = [0.0_f32; 2];
            for tap in 0..SINC_LENGTH {
                let a = self.coefficients[phase * SINC_LENGTH + tap];
                let b = self.coefficients[(phase + 1) * SINC_LENGTH + tap];
                let coefficient = a + (b - a) * blend;
                let frame = self.input[start + tap].0;
                sample[0] += frame[0] * coefficient;
                sample[1] += frame[1] * coefficient;
            }
            output.push(sample);
            self.due -= 1;
            self.pending_advance = true;
        }
        Ok(())
    }

    fn advance_position(&mut self) -> Result<bool, AudioError> {
        let rate = self.phase_rate.expect("output has a sample rate");
        let whole_step = rate.numerator() / rate.denominator().get() / self.output_rate;
        let step = self.phase_step;
        let mut next_phase = self.phase;
        let advance =
            advance(&mut next_phase, whole_step, step).ok_or(AudioError::OutputLengthOverflow)?;
        let target = self
            .position
            .checked_add(advance as usize)
            .ok_or(AudioError::OutputLengthOverflow)?;
        let boundary = self.find_rate_change(self.position, target, rate);
        if boundary.is_none() && target >= self.input.len() {
            return Ok(false);
        }
        if let Some(boundary) = boundary {
            // Split one output interval at every input-rate boundary it crosses.
            let fraction = self.phase as f32 / 4_294_967_296.0;
            let distance = (boundary - self.position) as f32 - fraction;
            let step_value = whole_step as f32 + step as f32 / 4_294_967_296.0;
            let mut remaining = (1.0 - distance / step_value).max(0.0);
            let mut position = boundary;
            loop {
                let rate = self.input[position].1;
                let (whole_step, step) =
                    divide_ratio(rate.numerator(), rate.denominator().get(), self.output_rate)
                        .ok_or(AudioError::OutputLengthOverflow)?;
                let step_value = whole_step as f32 + step as f32 / 4_294_967_296.0;
                let distance = remaining * step_value;
                let whole = distance.floor() as usize;
                let target = position
                    .checked_add(whole)
                    .ok_or(AudioError::OutputLengthOverflow)?;
                if let Some(boundary) = self.find_rate_change(position, target, rate) {
                    remaining = (remaining - (boundary - position) as f32 / step_value).max(0.0);
                    position = boundary;
                } else {
                    if target >= self.input.len() {
                        return Ok(false);
                    }
                    self.position = target;
                    self.phase = (distance.fract() * 4_294_967_296.0) as u32;
                    self.phase_rate = Some(rate);
                    self.phase_step = step;
                    break;
                }
            }
        } else {
            self.position = target;
            self.phase = next_phase;
        }
        let discard = self.position.saturating_sub(HALF - 1).min(self.input.len());
        self.input.drain(..discard);
        self.position -= discard;
        Ok(true)
    }

    fn find_rate_change(&self, position: usize, target: usize, rate: SampleRate) -> Option<usize> {
        let end = target.min(self.input.len().saturating_sub(1));
        ((position + 1)..=end).find(|index| !same_rate(self.input[*index].1, rate))
    }
}

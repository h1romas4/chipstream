//! Per-output and grouped stereo effects for sequencer voices.

use std::any::Any;
use std::error::Error;
use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use crate::audio::{
    AudioError, NativeBlock, NativeEffect, StereoBlock, StereoEffect, StereoMatrix,
};
use crate::chip::SampleRate;

/// A typed handle to an effect installed in a sequencer.
pub struct EffectId<T> {
    /// Identity shared with the installed entry.
    id: Arc<()>,
    /// Associates the identity with the concrete effect type.
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for EffectId<T> {
    fn clone(&self) -> Self {
        Self {
            id: Arc::clone(&self.id),
            marker: PhantomData,
        }
    }
}

/// A handle to a group of native outputs within one chip instance.
#[derive(Clone, Debug)]
pub struct OutputGroupId(Arc<()>);

/// Invalid effect configuration or handle.
#[derive(Debug)]
pub enum EffectError {
    /// An output index is outside the native output layout.
    OutputOutOfRange {
        /// Requested native output index.
        output: usize,
        /// Number of native outputs.
        channels: usize,
    },
    /// A group must contain at least one output.
    EmptyGroup,
    /// An output was repeated or already belongs to a group.
    OutputAlreadyGrouped {
        /// Repeated or already grouped native output index.
        output: usize,
    },
    /// The group does not exist in this sequencer.
    GroupNotFound,
    /// The effect does not exist in this sequencer.
    EffectNotFound,
    /// The requested position is outside the effect chain.
    PositionOutOfRange {
        /// Requested position in the chain.
        position: usize,
        /// Number of effects in the chain.
        length: usize,
    },
    /// Pan must be finite and between -1 and 1.
    InvalidPan,
}

impl fmt::Display for EffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutputOutOfRange { output, channels } => {
                write!(f, "output {output} is outside the {channels}-output layout")
            }
            Self::EmptyGroup => write!(f, "an output group must not be empty"),
            Self::OutputAlreadyGrouped { output } => {
                write!(f, "output {output} is already grouped")
            }
            Self::GroupNotFound => write!(f, "output group not found"),
            Self::EffectNotFound => write!(f, "effect not found"),
            Self::PositionOutOfRange { position, length } => {
                write!(
                    f,
                    "effect position {position} is outside the chain of length {length}"
                )
            }
            Self::InvalidPan => write!(f, "pan must be finite and between -1 and 1"),
        }
    }
}

impl Error for EffectError {}

/// Type-erased native effect with checkpoint-compatible cloning.
trait NativeProcessor {
    /// Process one native output in place.
    fn process(&mut self, samples: &mut [f32], rate: SampleRate) -> Result<(), AudioError>;
    /// Expose the concrete effect for typed handles.
    fn any_mut(&mut self) -> &mut dyn Any;
    /// Clone the effect's processing state.
    fn clone_box(&self) -> Box<dyn NativeProcessor>;
}

impl<T: NativeEffect + Clone + 'static> NativeProcessor for T {
    fn process(&mut self, samples: &mut [f32], rate: SampleRate) -> Result<(), AudioError> {
        NativeEffect::process_native(self, samples, rate)
    }
    fn any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn clone_box(&self) -> Box<dyn NativeProcessor> {
        Box::new(self.clone())
    }
}

/// Type-erased stereo effect with checkpoint-compatible cloning.
trait StereoProcessor {
    /// Process a grouped stereo block.
    fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError>;
    /// Expose the concrete effect for typed handles.
    fn any_mut(&mut self) -> &mut dyn Any;
    /// Clone the effect's processing state.
    fn clone_box(&self) -> Box<dyn StereoProcessor>;
}

impl<T: StereoEffect + Clone + 'static> StereoProcessor for T {
    fn process(&mut self, block: &mut StereoBlock) -> Result<(), AudioError> {
        StereoEffect::process(self, block)
    }
    fn any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn clone_box(&self) -> Box<dyn StereoProcessor> {
        Box::new(self.clone())
    }
}

/// Native effect and its stable handle identity.
struct NativeEntry {
    id: Arc<()>,
    effect: Box<dyn NativeProcessor>,
}

impl Clone for NativeEntry {
    fn clone(&self) -> Self {
        Self {
            id: Arc::clone(&self.id),
            effect: self.effect.clone_box(),
        }
    }
}

/// Stereo effect and its stable handle identity.
struct StereoEntry {
    id: Arc<()>,
    effect: Box<dyn StereoProcessor>,
}

impl Clone for StereoEntry {
    fn clone(&self) -> Self {
        Self {
            id: Arc::clone(&self.id),
            effect: self.effect.clone_box(),
        }
    }
}

#[derive(Clone)]
/// Disjoint native outputs and their stereo effect chain.
struct Group {
    /// Identity used by group handles and checkpoints.
    id: Arc<()>,
    /// Native output indices belonging to the group.
    outputs: Vec<usize>,
    /// Effects applied after grouped outputs are mixed to stereo.
    effects: Vec<StereoEntry>,
}

#[derive(Clone)]
/// Effect chains and pan settings for one chip voice.
pub(super) struct EffectRack {
    /// Per-output native effect chains.
    native: Vec<Vec<NativeEntry>>,
    /// Optional per-output pan overrides.
    pans: Vec<Option<f32>>,
    /// Output groups and their stereo effects.
    groups: Vec<Group>,
}

impl EffectRack {
    /// Initialize an empty rack for the native output count.
    pub(super) fn new(channels: usize) -> Self {
        Self {
            native: vec![Vec::new(); channels],
            pans: vec![None; channels],
            groups: Vec::new(),
        }
    }

    /// Validate a native output index.
    fn check_output(&self, output: usize) -> Result<(), EffectError> {
        if output >= self.native.len() {
            return Err(EffectError::OutputOutOfRange {
                output,
                channels: self.native.len(),
            });
        }
        Ok(())
    }

    /// Set or clear an output's pan override.
    pub(super) fn set_pan(&mut self, output: usize, pan: Option<f32>) -> Result<(), EffectError> {
        self.check_output(output)?;
        if pan.is_some_and(|p| !p.is_finite() || !(-1.0..=1.0).contains(&p)) {
            return Err(EffectError::InvalidPan);
        }
        self.pans[output] = pan;
        Ok(())
    }

    /// Append an effect to one native output.
    pub(super) fn add_native<T: NativeEffect + Clone + 'static>(
        &mut self,
        output: usize,
        effect: T,
    ) -> Result<EffectId<T>, EffectError> {
        self.check_output(output)?;
        let id = Arc::new(());
        self.native[output].push(NativeEntry {
            id: Arc::clone(&id),
            effect: Box::new(effect),
        });
        Ok(EffectId {
            id,
            marker: PhantomData,
        })
    }

    /// Create a nonempty group whose outputs are not already grouped.
    pub(super) fn create_group(&mut self, outputs: &[usize]) -> Result<OutputGroupId, EffectError> {
        if outputs.is_empty() {
            return Err(EffectError::EmptyGroup);
        }
        for (index, &output) in outputs.iter().enumerate() {
            self.check_output(output)?;
            if outputs[..index].contains(&output)
                || self
                    .groups
                    .iter()
                    .any(|group| group.outputs.contains(&output))
            {
                return Err(EffectError::OutputAlreadyGrouped { output });
            }
        }
        let id = Arc::new(());
        self.groups.push(Group {
            id: Arc::clone(&id),
            outputs: outputs.to_vec(),
            effects: Vec::new(),
        });
        Ok(OutputGroupId(id))
    }

    /// Check whether this rack owns a group handle.
    pub(super) fn contains_group(&self, id: &OutputGroupId) -> bool {
        self.groups
            .iter()
            .any(|group| Arc::ptr_eq(&group.id, &id.0))
    }

    /// Remove a group and its stereo effects.
    pub(super) fn remove_group(&mut self, id: &OutputGroupId) -> Result<(), EffectError> {
        let index = self
            .groups
            .iter()
            .position(|group| Arc::ptr_eq(&group.id, &id.0))
            .ok_or(EffectError::GroupNotFound)?;
        self.groups.remove(index);
        Ok(())
    }

    /// Append an effect to a group's stereo chain.
    pub(super) fn add_stereo<T: StereoEffect + Clone + 'static>(
        &mut self,
        group: &OutputGroupId,
        effect: T,
    ) -> Result<EffectId<T>, EffectError> {
        let group = self
            .groups
            .iter_mut()
            .find(|g| Arc::ptr_eq(&g.id, &group.0))
            .ok_or(EffectError::GroupNotFound)?;
        let id = Arc::new(());
        group.effects.push(StereoEntry {
            id: Arc::clone(&id),
            effect: Box::new(effect),
        });
        Ok(EffectId {
            id,
            marker: PhantomData,
        })
    }

    /// Find a typed effect by its handle identity.
    pub(super) fn effect_mut<T: 'static>(&mut self, id: &EffectId<T>) -> Option<&mut T> {
        for chain in &mut self.native {
            for entry in chain {
                if Arc::ptr_eq(&entry.id, &id.id) {
                    return entry.effect.any_mut().downcast_mut();
                }
            }
        }
        for group in &mut self.groups {
            for entry in &mut group.effects {
                if Arc::ptr_eq(&entry.id, &id.id) {
                    return entry.effect.any_mut().downcast_mut();
                }
            }
        }
        None
    }

    /// Remove an effect if this rack owns its handle.
    pub(super) fn remove_effect<T>(&mut self, id: &EffectId<T>) -> bool {
        for chain in &mut self.native {
            if let Some(index) = chain
                .iter()
                .position(|entry| Arc::ptr_eq(&entry.id, &id.id))
            {
                chain.remove(index);
                return true;
            }
        }
        for group in &mut self.groups {
            if let Some(index) = group
                .effects
                .iter()
                .position(|entry| Arc::ptr_eq(&entry.id, &id.id))
            {
                group.effects.remove(index);
                return true;
            }
        }
        false
    }

    /// Move an effect within its existing chain.
    pub(super) fn move_effect<T>(
        &mut self,
        id: &EffectId<T>,
        position: usize,
    ) -> Result<bool, EffectError> {
        macro_rules! reorder {
            ($chain:expr) => {
                if let Some(index) = $chain
                    .iter()
                    .position(|entry| Arc::ptr_eq(&entry.id, &id.id))
                {
                    if position >= $chain.len() {
                        return Err(EffectError::PositionOutOfRange {
                            position,
                            length: $chain.len(),
                        });
                    }
                    let entry = $chain.remove(index);
                    $chain.insert(position, entry);
                    return Ok(true);
                }
            };
        }
        for chain in &mut self.native {
            reorder!(chain);
        }
        for group in &mut self.groups {
            reorder!(group.effects);
        }
        Ok(false)
    }

    /// Compare group and effect identities and order for checkpoint restoration.
    pub(super) fn same_topology(&self, other: &Self) -> bool {
        self.native.len() == other.native.len()
            && self.native.iter().zip(&other.native).all(|(a, b)| {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| Arc::ptr_eq(&a.id, &b.id))
            })
            && self.groups.len() == other.groups.len()
            && self.groups.iter().zip(&other.groups).all(|(a, b)| {
                Arc::ptr_eq(&a.id, &b.id)
                    && a.outputs == b.outputs
                    && a.effects.len() == b.effects.len()
                    && a.effects
                        .iter()
                        .zip(&b.effects)
                        .all(|(a, b)| Arc::ptr_eq(&a.id, &b.id))
            })
    }

    /// Apply native effects, routing, and grouped stereo effects to a block.
    pub(super) fn process(
        &mut self,
        input: &NativeBlock<'_>,
        matrix: &StereoMatrix,
        base: &[[f32; 2]],
        masks: &[[bool; 2]],
        gains: &[f32],
    ) -> Result<StereoBlock, AudioError> {
        if self.native.iter().all(Vec::is_empty)
            && self.pans.iter().all(Option::is_none)
            && self.groups.iter().all(|group| group.effects.is_empty())
        {
            return matrix.convert_with_gains(input, Some(gains));
        }
        let rate = input.sample_rate_ratio();
        let count = input.frames();
        let mut mixed = vec![[0.0; 2]; count];
        let mut grouped = vec![vec![[0.0; 2]; count]; self.groups.len()];
        for output in 0..input.channels() {
            let mut samples: Vec<f32> = input
                .samples()
                .chunks_exact(input.channels())
                .map(|frame| frame[output] as f32)
                .collect();
            for entry in &mut self.native[output] {
                entry.effect.process(&mut samples, rate)?;
                if samples.iter().any(|sample| !sample.is_finite()) {
                    return Err(AudioError::NonFiniteSample);
                }
            }
            let mut coeff = matrix.coefficients()[output];
            if let Some(pan) = self.pans[output] {
                let level = base[output][0].abs().max(base[output][1].abs());
                coeff = [
                    if masks[output][0] {
                        level * (1.0 - pan.max(0.0))
                    } else {
                        0.0
                    },
                    if masks[output][1] {
                        level * (1.0 + pan.min(0.0))
                    } else {
                        0.0
                    },
                ];
            }
            let destination = if let Some(index) = self
                .groups
                .iter()
                .position(|group| !group.effects.is_empty() && group.outputs.contains(&output))
            {
                &mut grouped[index]
            } else {
                &mut mixed
            };
            for (frame, sample) in destination.iter_mut().zip(samples) {
                frame[0] += sample * coeff[0] * gains[output];
                frame[1] += sample * coeff[1] * gains[output];
            }
        }
        for (group, frames) in self.groups.iter_mut().zip(grouped) {
            if group.effects.is_empty() {
                continue;
            }
            let mut block = StereoBlock::new(rate, frames)?;
            for entry in &mut group.effects {
                entry.effect.process(&mut block)?;
                if block.sample_rate() != rate {
                    return Err(AudioError::RateMismatch {
                        expected: rate,
                        actual: block.sample_rate(),
                    });
                }
                if block.frames().len() != count {
                    return Err(AudioError::FrameMismatch {
                        expected: count,
                        actual: block.frames().len(),
                    });
                }
            }
            for (output, frame) in mixed.iter_mut().zip(block.frames()) {
                output[0] += frame[0];
                output[1] += frame[1];
            }
        }
        if let Some(mute) = input.stereo_mute() {
            for frame in &mut mixed {
                if mute.left {
                    frame[0] = 0.0;
                }
                if mute.right {
                    frame[1] = 0.0;
                }
            }
        }
        StereoBlock::new(rate, mixed)
    }
}

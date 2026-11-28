//! Typed source execution notifications on the VGM generation timeline.

/// A source position observed during command generation.
///
/// `sample` is elapsed time at 44,100 Hz from the beginning of the generated
/// command stream, including repeats. Generators may report events ahead of
/// the commands consumed by a player. This is not a device playback position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionEvent<T> {
    /// Source generation position in 44,100 Hz samples.
    pub sample: u64,
    /// Format-specific source position or other execution data.
    pub data: T,
}

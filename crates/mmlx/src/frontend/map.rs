//! Dense source maps; dialects decide how output locations address their groups.

use crate::source::Span;

/// A dense map from dialect-specific output coordinates to primary source ranges.
///
/// Stored separately from the generated document and allocated only by mapped
/// compilation APIs. Entries identify the primary origin, not all state changes
/// contributing to an output command. Coordinates refer to the finalized document.
#[derive(Debug)]
pub struct SourceMap<Location> {
    groups: Vec<Vec<Option<Span>>>,
    coordinates: fn(&Location) -> (usize, usize),
}

impl<Location> SourceMap<Location> {
    pub(crate) fn new(
        groups: Vec<Vec<Option<Span>>>,
        coordinates: fn(&Location) -> (usize, usize),
    ) -> Self {
        Self {
            groups,
            coordinates,
        }
    }

    /// Synthetic output and invalid locations have no source range.
    ///
    /// Returns a half-open UTF-8 byte range in the original source snapshot.
    /// Document edits after extracting this map can invalidate its coordinates.
    pub fn get(&self, location: &Location) -> Option<Span> {
        let (group, index) = (self.coordinates)(location);
        self.groups.get(group)?.get(index).copied().flatten()
    }
}

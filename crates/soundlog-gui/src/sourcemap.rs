mod mdx;
mod model;
mod raw;
mod vgm;

use anyhow::Result;

pub use mdx::MdxAdapter;
pub use model::{ByteCoordinateSpace, ByteRange, MappedRange, NodeId, SourceNode};
pub use raw::RawBinaryAdapter;
pub use vgm::VgmAdapter;

/// Common parse/serialization contract shared by structured source adapters.
///
/// Format-specific source mapping and tree construction stay on each adapter;
/// this trait only covers the lifecycle shared by all parsed documents.
pub trait SourceAdapter {
    type Document;

    fn parse(bytes: &[u8]) -> Result<Self::Document>;
    fn canonical_bytes(document: &Self::Document) -> Vec<u8>;
}

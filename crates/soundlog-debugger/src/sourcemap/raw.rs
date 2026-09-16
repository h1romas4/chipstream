use super::{MappedRange, SourceNode};

/// Fallback adapter for binary data without a recognized format.
#[derive(Clone, Copy, Debug)]
pub struct RawBinaryAdapter {
    block_size: usize,
}

impl RawBinaryAdapter {
    pub const DEFAULT_BLOCK_SIZE: usize = 256;

    pub const fn new(block_size: usize) -> Self {
        Self { block_size }
    }

    pub const fn block_size(self) -> usize {
        self.block_size
    }

    pub fn parse(self, bytes: &[u8]) -> Vec<SourceNode> {
        let block_size = self.block_size.max(1);
        bytes
            .chunks(block_size)
            .enumerate()
            .map(|(index, chunk)| {
                let start = index * block_size;
                SourceNode::new(
                    index as u64,
                    format!("Block 0x{start:08x}"),
                    format!("{} bytes", chunk.len()),
                )
                .with_range(MappedRange::original(start, chunk.len()))
            })
            .collect()
    }
}

impl Default for RawBinaryAdapter {
    fn default() -> Self {
        Self::new(Self::DEFAULT_BLOCK_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::RawBinaryAdapter;
    use crate::sourcemap::ByteCoordinateSpace;

    #[test]
    fn raw_adapter_maps_each_block_to_original_bytes() {
        let adapter = RawBinaryAdapter::new(4);
        let nodes = adapter.parse(&[0, 1, 2, 3, 4, 5]);

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].label, "Block 0x00000000");
        assert_eq!(nodes[0].range.unwrap().space, ByteCoordinateSpace::Original);
        assert_eq!(nodes[0].range.unwrap().range.start, 0);
        assert_eq!(nodes[0].range.unwrap().range.len, 4);
        assert_eq!(nodes[1].range.unwrap().range.start, 4);
        assert_eq!(nodes[1].range.unwrap().range.len, 2);
    }

    #[test]
    fn raw_adapter_handles_empty_input_and_zero_block_size() {
        assert!(RawBinaryAdapter::default().parse(&[]).is_empty());
        assert_eq!(RawBinaryAdapter::new(0).block_size(), 0);
        assert_eq!(RawBinaryAdapter::new(0).parse(&[1]).len(), 1);
    }
}

use anyhow::{Result, anyhow};
use soundlog::mdx::pdx::PdxDocument;

use super::{ByteCoordinateSpace, ByteRange, MappedRange, SourceAdapter, SourceNode};

/// Entry point for PDX parsing and bank/sample source mapping.
pub struct PdxAdapter;

impl SourceAdapter for PdxAdapter {
    type Document = PdxDocument;

    fn parse(bytes: &[u8]) -> Result<Self::Document> {
        Self::parse(bytes)
    }

    fn canonical_bytes(document: &Self::Document) -> Result<Vec<u8>> {
        Ok(Self::canonical_bytes(document))
    }
}

impl PdxAdapter {
    pub fn parse(bytes: &[u8]) -> Result<PdxDocument> {
        PdxDocument::parse(bytes).map_err(|error| anyhow!("failed to parse PDX: {error}"))
    }

    pub fn canonical_bytes(document: &PdxDocument) -> Vec<u8> {
        document.to_bytes()
    }

    pub fn root_node(document: &PdxDocument) -> SourceNode {
        let space = if document.is_compressed() {
            ByteCoordinateSpace::Logical
        } else {
            ByteCoordinateSpace::Original
        };
        let source_map = document.sourcemap();
        let banks = document
            .banks
            .iter()
            .zip(source_map)
            .enumerate()
            .map(|(bank_index, (bank, bank_map))| {
                let sample_count = bank.entries.iter().filter(|entry| entry.is_some()).count();
                let notes = bank
                    .entries
                    .iter()
                    .enumerate()
                    .filter_map(|(note_index, sample)| {
                        let sample = sample.as_ref()?;
                        let entry_map = bank_map.entries.get(note_index)?;
                        let (table_offset, table_len) = entry_map.table_entry_range;
                        let (sample_offset, sample_len) = entry_map.sample_range?;
                        let note_id = ((bank_index * 96 + note_index) as u64) + 0x10000;
                        let table_entry = SourceNode::new(
                            note_id + 0x10000,
                            "Table entry",
                            format!("start 0x{:x}, size {}", sample.start, sample.size),
                        )
                        .with_range(MappedRange {
                            space,
                            range: ByteRange::new(table_offset, table_len),
                        });
                        let sample_data = SourceNode::new(
                            note_id + 0x20000,
                            "Sample data",
                            format!("{} encoded bytes", sample.size),
                        )
                        .with_range(MappedRange {
                            space,
                            range: ByteRange::new(sample_offset, sample_len),
                        });

                        Some(
                            SourceNode::new(
                                note_id,
                                format!("Note {note_index}"),
                                format!("{} bytes at 0x{:x}", sample.size, sample.start),
                            )
                            .with_children(vec![table_entry, sample_data]),
                        )
                    })
                    .collect();
                let (table_offset, table_len) = bank_map.table_range;

                SourceNode::new(
                    bank_index as u64 + 1,
                    format!("Bank {bank_index}"),
                    format!("{sample_count} samples"),
                )
                .with_range(MappedRange {
                    space,
                    range: ByteRange::new(table_offset, table_len),
                })
                .with_children(notes)
            })
            .collect();
        let decoded_len = document.decoded_bytes().len();
        let compression = if document.is_compressed() {
            "LZ-compressed"
        } else {
            "uncompressed"
        };

        SourceNode::new(
            0,
            "PDX",
            format!(
                "{} banks, {decoded_len} decoded bytes, {compression}",
                document.banks.len()
            ),
        )
        .with_range(MappedRange {
            space,
            range: ByteRange::new(0, decoded_len),
        })
        .with_children(banks)
    }
}

#[cfg(test)]
mod tests {
    use super::PdxAdapter;
    use crate::sourcemap::{ByteCoordinateSpace, ByteRange};
    use soundlog::mdx::pdx::PdxBuilder;

    #[test]
    fn adapter_parses_and_builds_bank_sample_tree_with_ranges() {
        let mut builder = PdxBuilder::new();
        builder
            .set_sample(1, 3, b"PCM1".to_vec())
            .expect("set sample in second bank");
        let bytes = builder.finalize().to_bytes();
        let document = PdxAdapter::parse(&bytes).unwrap();

        let root = PdxAdapter::root_node(&document);

        assert_eq!(root.label, "PDX");
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[1].label, "Bank 1");
        assert_eq!(
            root.children[1].range.unwrap().range,
            ByteRange::new(0x300, 0x300)
        );
        assert_eq!(root.children[1].children.len(), 1);

        let note = &root.children[1].children[0];
        assert_eq!(note.label, "Note 3");
        assert_eq!(note.detail, "4 bytes at 0x600");
        assert_eq!(note.children[0].label, "Table entry");
        assert_eq!(
            note.children[0].range.unwrap().range,
            ByteRange::new(0x318, 8)
        );
        assert_eq!(note.children[1].label, "Sample data");
        assert_eq!(
            note.children[1].range.unwrap().range,
            ByteRange::new(0x600, 4)
        );
    }

    #[test]
    fn adapter_marks_compressed_ranges_as_logical() {
        let mut builder = PdxBuilder::new();
        builder
            .set_sample(0, 0, b"PCM1".to_vec())
            .expect("set sample");
        builder.set_lz_compressed(true);
        let document = PdxAdapter::parse(&builder.finalize().to_bytes()).unwrap();

        let root = PdxAdapter::root_node(&document);

        assert_eq!(
            root.children[0].range.unwrap().space,
            ByteCoordinateSpace::Logical
        );
        assert_eq!(
            root.children[0].children[0].children[1]
                .range
                .unwrap()
                .space,
            ByteCoordinateSpace::Logical
        );
    }
}

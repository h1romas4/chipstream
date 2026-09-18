use anyhow::{Result, anyhow};
use soundlog::mdx::document::MdxDocument;
use soundlog::mdx::tone::MdxTone;

use super::{ByteCoordinateSpace, ByteRange, MappedRange, SourceAdapter, SourceNode};

/// Entry point for MDX parsing and logical source mapping.
pub struct MdxAdapter;

impl SourceAdapter for MdxAdapter {
    type Document = MdxDocument;

    fn parse(bytes: &[u8]) -> Result<Self::Document> {
        Self::parse(bytes)
    }

    fn canonical_bytes(document: &Self::Document) -> Vec<u8> {
        Self::canonical_bytes(document)
    }
}

impl MdxAdapter {
    pub fn coordinate_space(bytes: &[u8]) -> ByteCoordinateSpace {
        if bytes
            .windows(4)
            .any(|window| window == [0x7f, 0xff, 0xff, 0x4c])
        {
            ByteCoordinateSpace::Logical
        } else {
            ByteCoordinateSpace::Original
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<MdxDocument> {
        MdxDocument::parse(bytes).map_err(|error| anyhow!("failed to parse MDX: {error}"))
    }

    pub fn canonical_bytes(document: &MdxDocument) -> Vec<u8> {
        document.to_bytes()
    }

    pub fn header_node(document: &MdxDocument, space: ByteCoordinateSpace) -> SourceNode {
        let title_len = document.header.title_raw_bytes.len();
        let pdx_start = title_len + 3;
        let pdx_len = document
            .header
            .pdx_name_raw_bytes
            .as_ref()
            .map_or(0, Vec::len);
        let header_len = document.header.base_offset + 2 + document.header.track_count() * 2;

        let mut children = vec![
            SourceNode::new(0, "Title", document.header.title.clone()).with_range(MappedRange {
                space,
                range: ByteRange::new(0, title_len),
            }),
            SourceNode::new(
                1,
                "PDX name",
                document.header.pdx_name.clone().unwrap_or_default(),
            )
            .with_range(MappedRange {
                space,
                range: ByteRange::new(pdx_start, pdx_len),
            }),
        ];
        children.push(
            SourceNode::new(
                2,
                "Tone data offset",
                format!("0x{:04x}", document.header.tone_data_offset),
            )
            .with_range(MappedRange {
                space,
                range: ByteRange::new(document.header.base_offset, 2),
            }),
        );
        children.push(
            SourceNode::new(3, "Track count", document.header.track_count().to_string())
                .with_range(MappedRange {
                    space,
                    range: ByteRange::new(
                        document.header.base_offset + 2,
                        document.header.track_count() * 2,
                    ),
                }),
        );

        SourceNode::new(0, "Header", "MDX header")
            .with_range(MappedRange {
                space,
                range: ByteRange::new(0, header_len),
            })
            .with_children(children)
    }

    pub fn tone_node(document: &MdxDocument, space: ByteCoordinateSpace) -> Option<SourceNode> {
        let bytes = document.tone_bank.to_bytes();
        if bytes.is_empty() {
            return None;
        }
        let start = document.header.tone_data_position()?;
        let children = document
            .tone_bank
            .tones
            .iter()
            .enumerate()
            .map(|(index, tone)| {
                SourceNode::new(
                    0x2000_0000 | index as u64,
                    format!("Voice {}", tone.voice_number),
                    format!("{tone:?}"),
                )
                .with_range(MappedRange {
                    space,
                    range: ByteRange::new(
                        start + index * MdxTone::BYTE_LENGTH,
                        MdxTone::BYTE_LENGTH,
                    ),
                })
            })
            .collect();
        Some(
            SourceNode::new(1, "Tone data", format!("{} bytes", bytes.len()))
                .with_range(MappedRange {
                    space,
                    range: ByteRange::new(start, bytes.len()),
                })
                .with_children(children),
        )
    }

    pub fn track_nodes(document: &MdxDocument, track: usize) -> Vec<SourceNode> {
        Self::track_nodes_in_space(document, track, ByteCoordinateSpace::Logical)
    }

    pub fn track_nodes_in_space(
        document: &MdxDocument,
        track: usize,
        space: ByteCoordinateSpace,
    ) -> Vec<SourceNode> {
        let Some(commands) = document.tracks.get(track) else {
            return Vec::new();
        };
        let ranges = document
            .sourcemap()
            .into_iter()
            .nth(track)
            .unwrap_or_default();

        commands
            .iter()
            .enumerate()
            .map(|(index, command)| {
                let detail = format!("{command:?}");
                let mut node = SourceNode::new(
                    ((track as u64) << 32) | index as u64,
                    format!("{index}: {detail}"),
                    detail,
                );
                if let Some((start, len)) = ranges.get(index).copied() {
                    node.range = Some(MappedRange {
                        space,
                        range: ByteRange::new(start, len),
                    });
                }
                node
            })
            .collect()
    }

    pub fn track_node(document: &MdxDocument, track: usize) -> Option<SourceNode> {
        Self::track_node_in_space(document, track, ByteCoordinateSpace::Logical)
    }

    pub fn track_node_in_space(
        document: &MdxDocument,
        track: usize,
        space: ByteCoordinateSpace,
    ) -> Option<SourceNode> {
        let commands = document.tracks.get(track)?;
        let children = Self::track_nodes_in_space(document, track, space);
        let mut node = SourceNode::new(
            0x1000_0000 | track as u64,
            format!("Track {track}"),
            format!("{} commands", commands.len()),
        )
        .with_children(children);

        if let Some(start) = document
            .header
            .track_position(track)
            .filter(|_| !commands.is_empty())
        {
            let len = node
                .children
                .iter()
                .filter_map(|child| child.range.map(|range| range.range))
                .map(|range| range.len)
                .sum();
            node.range = Some(MappedRange {
                space,
                range: ByteRange::new(start, len),
            });
        }
        Some(node)
    }

    pub fn root_nodes(document: &MdxDocument) -> Vec<SourceNode> {
        Self::root_nodes_in_space(document, ByteCoordinateSpace::Logical)
    }

    pub fn root_nodes_in_space(
        document: &MdxDocument,
        space: ByteCoordinateSpace,
    ) -> Vec<SourceNode> {
        (0..document.tracks.len())
            .filter_map(|track| Self::track_node_in_space(document, track, space))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::MdxAdapter;
    use soundlog::mdx::command::MdxRest;
    use soundlog::mdx::document::MdxBuilder;
    use soundlog::mdx::tone::{MdxOperator, MdxTone};

    #[test]
    fn adapter_parses_and_maps_track_commands() {
        let mut builder = MdxBuilder::new();
        builder.add_mdx_command(0, MdxRest::new(12).unwrap());
        let bytes = builder.finalize().to_bytes();
        let document = MdxAdapter::parse(&bytes).unwrap();

        let nodes = MdxAdapter::track_nodes(&document, 0);

        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].label, "0: Rest(MdxRest { ticks: 12 })");
        assert_eq!(
            nodes[0].range.unwrap().space,
            super::ByteCoordinateSpace::Logical
        );
        assert!(nodes[0].range.unwrap().range.len > 0);
    }

    #[test]
    fn adapter_returns_empty_nodes_for_unknown_track() {
        let document = MdxBuilder::new().finalize();

        assert!(MdxAdapter::track_nodes(&document, 99).is_empty());
        assert!(MdxAdapter::track_node(&document, 99).is_none());
    }

    #[test]
    fn header_node_maps_offset_and_track_table_fields() {
        let mut builder = MdxBuilder::new();
        builder.add_mdx_command(0, MdxRest::new(12).unwrap());
        let document = MdxAdapter::parse(&builder.finalize().to_bytes()).unwrap();
        let header = MdxAdapter::header_node(&document, super::ByteCoordinateSpace::Original);

        assert_eq!(
            header.children[2].range.unwrap().range,
            super::ByteRange::new(document.header.base_offset, 2)
        );
        assert_eq!(
            header.children[3].range.unwrap().range,
            super::ByteRange::new(
                document.header.base_offset + 2,
                document.header.track_count() * 2,
            )
        );
    }

    #[test]
    fn tone_node_lists_tones_by_voice_number() {
        let mut builder = MdxBuilder::new();
        builder.append_tone(MdxTone {
            voice_number: 3,
            con: 1,
            fl: 2,
            op: 4,
            operators: [MdxOperator::default(); 4],
        });
        builder.append_tone(MdxTone {
            voice_number: 7,
            con: 5,
            fl: 6,
            op: 8,
            operators: [MdxOperator::default(); 4],
        });
        let document = MdxAdapter::parse(&builder.finalize().to_bytes()).unwrap();

        let node = MdxAdapter::tone_node(&document, super::ByteCoordinateSpace::Original).unwrap();

        assert_eq!(node.children.len(), 2);
        assert_eq!(node.children[0].label, "Voice 3");
        assert!(node.children[0].detail.contains("voice_number: 3"));
        assert_eq!(
            node.children[0].range.unwrap().range,
            super::ByteRange::new(node.range.unwrap().range.start, MdxTone::BYTE_LENGTH,)
        );
        assert_eq!(node.children[1].label, "Voice 7");
        assert!(node.children[1].detail.contains("voice_number: 7"));
    }

    #[test]
    fn adapter_distinguishes_original_and_logical_input_spaces() {
        let mut builder = MdxBuilder::new();
        builder.add_mdx_command(0, MdxRest::new(12).unwrap());
        let original = builder.finalize().to_bytes();

        let mut compressed_builder = MdxBuilder::new();
        compressed_builder.add_mdx_command(0, MdxRest::new(12).unwrap());
        compressed_builder.set_lz_compressed(true);
        let compressed = compressed_builder.finalize().to_bytes();

        assert_eq!(
            MdxAdapter::coordinate_space(&original),
            super::ByteCoordinateSpace::Original
        );
        assert_eq!(
            MdxAdapter::coordinate_space(&compressed),
            super::ByteCoordinateSpace::Logical
        );
        assert!(MdxAdapter::parse(&compressed).is_ok());
    }
}

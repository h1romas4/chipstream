use anyhow::{Result, anyhow};
use soundlog::VgmDocument;

use super::{ByteRange, MappedRange, SourceAdapter, SourceNode};
use soundlog::vgm::VgmHeaderField;
use soundlog::vgm::command::VgmCommand;
use soundlog::vgm::detail::{DataBlockType, parse_data_block};

/// Entry point for VGM parsing and byte-level source mapping.
pub struct VgmAdapter;

impl SourceAdapter for VgmAdapter {
    type Document = VgmDocument;

    fn parse(bytes: &[u8]) -> Result<Self::Document> {
        Self::parse(bytes)
    }

    fn canonical_bytes(document: &Self::Document) -> Vec<u8> {
        Self::canonical_bytes(document)
    }
}

impl VgmAdapter {
    pub fn parse(bytes: &[u8]) -> Result<VgmDocument> {
        VgmDocument::try_from(bytes).map_err(|error| anyhow!("failed to parse VGM: {error:?}"))
    }

    pub fn canonical_bytes(document: &VgmDocument) -> Vec<u8> {
        document.into()
    }

    pub fn command_ranges(document: &VgmDocument) -> Vec<ByteRange> {
        document
            .sourcemap()
            .into_iter()
            .map(|(start, len)| ByteRange::new(start, len))
            .collect()
    }

    pub fn command_nodes(document: &VgmDocument, start: usize, count: usize) -> Vec<SourceNode> {
        let ranges = Self::command_ranges(document);
        document
            .iter()
            .enumerate()
            .skip(start)
            .take(count)
            .map(|(index, command)| {
                let (label, detail) = match command {
                    VgmCommand::DataBlock(data_block) => {
                        match parse_data_block(*data_block.clone()) {
                            Ok(data_block_type) => {
                                let detail = match data_block_type {
                                    DataBlockType::UncompressedStream(stream) => {
                                        format!("{stream:?}")
                                    }
                                    DataBlockType::CompressedStream(stream) => {
                                        format!("{stream:?}")
                                    }
                                    DataBlockType::DecompressionTable(table) => {
                                        format!("{table:?}")
                                    }
                                    DataBlockType::RomRamDump(dump) => format!("{dump:?}"),
                                    DataBlockType::RamWrite16(write) => format!("{write:?}"),
                                    DataBlockType::RamWrite32(write) => format!("{write:?}"),
                                };
                                (format!("{index}: {detail}"), detail)
                            }
                            Err((_, error)) => (
                                format!("{index}: DataBlock(parse error)"),
                                format!("<DataBlock parse error: {error:?}>"),
                            ),
                        }
                    }
                    _ => (format!("{index}: {command:?}"), format!("{command:?}")),
                };

                let mut node = SourceNode::new(index as u64, label, detail);
                if let Some(range) = ranges.get(index).copied() {
                    node.range = Some(MappedRange::original(range.start, range.len));
                }
                node
            })
            .collect()
    }

    pub fn gd3_node(document: &VgmDocument) -> Option<SourceNode> {
        if document.header.gd3_offset == 0 {
            return None;
        }

        let gd3_start = document.header.gd3_offset.wrapping_add(0x14) as usize;
        let mut field_offset = gd3_start + 12;
        let gd3 = document.gd3.as_ref()?;
        let mut children = Vec::new();

        let mut push_field = |id: u64, title: &str, value: &Option<String>| {
            if let Some(value) = value {
                let length = value.encode_utf16().count() * 2;
                let detail = if title == "Notes" {
                    value.replace('\n', " ")
                } else {
                    value.clone()
                };
                children.push(
                    SourceNode::new(id, title, detail)
                        .with_range(MappedRange::original(field_offset, length)),
                );
                field_offset = field_offset.saturating_add(length + 2);
            } else {
                field_offset = field_offset.saturating_add(2);
            }
        };

        push_field(0, "Track name (EN)", &gd3.track_name_en);
        push_field(1, "Track name (JP)", &gd3.track_name_origin);
        push_field(2, "Game name (EN)", &gd3.game_name_en);
        push_field(3, "Game name (JP)", &gd3.game_name_origin);
        push_field(4, "System name (EN)", &gd3.system_name_en);
        push_field(5, "System name (JP)", &gd3.system_name_origin);
        push_field(6, "Author (EN)", &gd3.author_name_en);
        push_field(7, "Author (JP)", &gd3.author_name_origin);
        push_field(8, "Release date", &gd3.release_date);
        push_field(9, "Creator", &gd3.creator);
        push_field(10, "Notes", &gd3.notes);

        if children.is_empty() {
            return None;
        }

        let mut node = SourceNode::new(0, "GD3", "Metadata").with_children(children);
        let length = gd3.to_bytes().len();
        if length > 0 {
            node.range = Some(MappedRange::original(gd3_start, length));
        }
        Some(node)
    }

    pub fn header_node(document: &VgmDocument) -> SourceNode {
        let mut children = Vec::new();
        let mut next_id = 0;

        let mut push = |title: &str, detail: String| {
            let mut node = SourceNode::new(next_id, title, detail);
            next_id += 1;
            if let Some(range) = Self::header_range(document, title) {
                node.range = Some(MappedRange::original(range.start, range.len));
            }
            children.push(node);
        };

        push(
            "Ident",
            format!("'{}'", String::from_utf8_lossy(&document.header.ident)),
        );

        macro_rules! display_field {
            ($field:ident, $title:literal) => {
                if document.header.$field != 0 {
                    push($title, format!("{}", document.header.$field));
                }
            };
        }
        macro_rules! debug_field {
            ($field:ident, $convert:ty, $title:literal) => {
                if <$convert>::from(document.header.$field) != 0 {
                    push($title, format!("{:?}", document.header.$field));
                }
            };
        }
        macro_rules! bytes_field {
            ($field:ident, $default:expr, $title:literal) => {
                if document.header.$field != $default {
                    push($title, format!("{:?}", document.header.$field));
                }
            };
        }

        display_field!(eof_offset, "EOF offset");
        display_field!(version, "Version");
        display_field!(sn76489_clock, "SN76489 clock");
        display_field!(ym2413_clock, "YM2413 clock");
        display_field!(gd3_offset, "GD3 offset");
        display_field!(total_samples, "Total samples");
        display_field!(loop_offset, "Loop offset");
        display_field!(loop_samples, "Loop samples");
        display_field!(sample_rate, "Sample rate");
        debug_field!(sn76489_feedback, u16, "SN76489 feedback");
        debug_field!(
            sn76489_shift_register_width,
            u8,
            "SN76489 shift register width"
        );
        debug_field!(sn76489_flags, u8, "SN76489 flags");
        display_field!(ym2612_clock, "YM2612 clock");
        display_field!(ym2151_clock, "YM2151 clock");
        display_field!(data_offset, "Data offset");
        display_field!(sega_pcm_clock, "Sega PCM clock");
        display_field!(spcm_interface, "SPCM interface");
        display_field!(rf5c68_clock, "RF5C68 clock");
        display_field!(ym2203_clock, "YM2203 clock");
        display_field!(ym2608_clock, "YM2608 clock");
        display_field!(ym2610b_clock, "YM2610B clock");
        display_field!(ym3812_clock, "YM3812 clock");
        display_field!(ym3526_clock, "YM3526 clock");
        display_field!(y8950_clock, "Y8950 clock");
        display_field!(ymf262_clock, "YMF262 clock");
        display_field!(ymf278b_clock, "YMF278B clock");
        display_field!(ymf271_clock, "YMF271 clock");
        display_field!(ymz280b_clock, "YMZ280B clock");
        display_field!(rf5c164_clock, "RF5C164 clock");
        display_field!(pwm_clock, "PWM clock");
        display_field!(ay8910_clock, "AY8910 clock");
        debug_field!(ay_chip_type, u8, "AY8910 chipType");
        debug_field!(ay8910_flags, u8, "Ay8910Flags");
        debug_field!(ym2203_ay8910_flags, u8, "Ym2203Ay8910Flags");
        debug_field!(ym2608_ay8910_flags, u8, "Ym2608Ay8910Flags");
        display_field!(gb_dmg_clock, "GB DMG clock");
        display_field!(nes_apu_clock, "NES APU clock");
        display_field!(multipcm_clock, "MultiPCM clock");
        display_field!(upd7759_clock, "UPD7759 clock");
        display_field!(okim6258_clock, "OKIM6258 clock");
        debug_field!(okim6258_flags, u8, "OKIM6258 flags");
        display_field!(okim6295_clock, "OKIM6295 clock");
        display_field!(k051649_clock, "K051649 clock");
        display_field!(k054539_clock, "K054539 clock");
        debug_field!(k054539_flags, u8, "K054539 flags");
        display_field!(huc6280_clock, "HuC6280 clock");
        display_field!(c140_clock, "C140 clock");
        debug_field!(c140_chip_type, u8, "C140 chipType");
        display_field!(k053260_clock, "K053260 clock");
        display_field!(pokey_clock, "Pokey clock");
        display_field!(qsound_clock, "QSound clock");
        display_field!(scsp_clock, "SCSP clock");
        display_field!(extra_header_offset, "Extra header offset");
        display_field!(wonderswan_clock, "WonderSwan clock");
        display_field!(vsu_clock, "VSU clock");
        display_field!(saa1099_clock, "SAA1099 clock");
        display_field!(es5503_clock, "ES5503 clock");
        display_field!(es5506_clock, "ES5506 clock");
        display_field!(es5503_output_channels, "ES5506 channels");
        display_field!(c352_clock_divider, "ES5506 CD flags");
        display_field!(x1_010_clock, "X1-010 clock");
        display_field!(c352_clock, "C352 clock");
        display_field!(ga20_clock, "GA20 clock");
        display_field!(mikey_clock, "Mikey clock");
        bytes_field!(reserved_e8_ef, [0u8; 8], "Reserved E8-EF");
        bytes_field!(reserved_f0_ff, [0u8; 16], "Reserved F0-FF");

        let header_len = Self::command_ranges(document)
            .first()
            .map(|range| range.start)
            .or_else(|| {
                (document.header.gd3_offset != 0)
                    .then_some(document.header.gd3_offset.wrapping_add(0x14) as usize)
            });
        let mut node = SourceNode::new(0, "Header", "Header fields").with_children(children);
        if let Some(length) = header_len {
            node.range = Some(MappedRange::original(0, length));
        }
        node
    }

    pub fn header_range(document: &VgmDocument, title: &str) -> Option<ByteRange> {
        let field = match title {
            "Ident" => VgmHeaderField::Ident,
            "EOF offset" => VgmHeaderField::EofOffset,
            "Version" => VgmHeaderField::Version,
            "SN76489 clock" => VgmHeaderField::Sn76489Clock,
            "YM2413 clock" => VgmHeaderField::Ym2413Clock,
            "GD3 offset" => VgmHeaderField::Gd3Offset,
            "Total samples" => VgmHeaderField::TotalSamples,
            "Loop offset" => VgmHeaderField::LoopOffset,
            "Loop samples" => VgmHeaderField::LoopSamples,
            "Sample rate" => VgmHeaderField::SampleRate,
            "SN76489 feedback" => VgmHeaderField::Sn76489Feedback,
            "SN76489 shift register width" => VgmHeaderField::Sn76489ShiftRegisterWidth,
            "SN76489 flags" => VgmHeaderField::Sn76489Flags,
            "YM2612 clock" => VgmHeaderField::Ym2612Clock,
            "YM2151 clock" => VgmHeaderField::Ym2151Clock,
            "Data offset" => VgmHeaderField::DataOffset,
            "Sega PCM clock" => VgmHeaderField::SegaPcmClock,
            "SPCM interface" => VgmHeaderField::SpcmInterface,
            "RF5C68 clock" => VgmHeaderField::Rf5c68Clock,
            "YM2203 clock" => VgmHeaderField::Ym2203Clock,
            "YM2608 clock" => VgmHeaderField::Ym2608Clock,
            "YM2610B clock" => VgmHeaderField::Ym2610bClock,
            "YM3812 clock" => VgmHeaderField::Ym3812Clock,
            "YM3526 clock" => VgmHeaderField::Ym3526Clock,
            "Y8950 clock" => VgmHeaderField::Y8950Clock,
            "YMF262 clock" => VgmHeaderField::Ymf262Clock,
            "YMF278B clock" => VgmHeaderField::Ymf278bClock,
            "YMF271 clock" => VgmHeaderField::Ymf271Clock,
            "YMZ280B clock" => VgmHeaderField::Ymz280bClock,
            "RF5C164 clock" => VgmHeaderField::Rf5c164Clock,
            "PWM clock" => VgmHeaderField::PwmClock,
            "AY8910 clock" => VgmHeaderField::Ay8910Clock,
            "AY8910 chipType" => VgmHeaderField::Ay8910ChipType,
            "Ay8910Flags" => VgmHeaderField::Ay8910Flags,
            "Ym2203Ay8910Flags" => VgmHeaderField::Ym2203Ay8910Flags,
            "Ym2608Ay8910Flags" => VgmHeaderField::Ym2608Ay8910Flags,
            "VolumeModifier" => VgmHeaderField::VolumeModifier,
            "LoopBase" => VgmHeaderField::LoopBase,
            "LoopModifier" => VgmHeaderField::LoopModifier,
            "GB DMG clock" => VgmHeaderField::GbDmgClock,
            "NES APU clock" => VgmHeaderField::NesApuClock,
            "MultiPCM clock" => VgmHeaderField::MultipcmClock,
            "UPD7759 clock" => VgmHeaderField::Upd7759Clock,
            "OKIM6258 clock" => VgmHeaderField::Okim6258Clock,
            "OKIM6258 flags" => VgmHeaderField::Okim6258Flags,
            "OKIM6295 clock" => VgmHeaderField::Okim6295Clock,
            "K051649 clock" => VgmHeaderField::K051649Clock,
            "K054539 clock" => VgmHeaderField::K054539Clock,
            "K054539 flags" => VgmHeaderField::K054539Flags,
            "HuC6280 clock" => VgmHeaderField::Huc6280Clock,
            "C140 chipType" => VgmHeaderField::C140ChipType,
            "C140 clock" => VgmHeaderField::C140Clock,
            "K053260 clock" => VgmHeaderField::K053260Clock,
            "Pokey clock" => VgmHeaderField::PokeyClock,
            "QSound clock" => VgmHeaderField::QsoundClock,
            "SCSP clock" => VgmHeaderField::ScspClock,
            "Extra header offset" => VgmHeaderField::ExtraHeaderOffset,
            "WonderSwan clock" => VgmHeaderField::WonderSwan,
            "VSU clock" => VgmHeaderField::Vsu,
            "SAA1099 clock" => VgmHeaderField::Saa1099,
            "ES5503 clock" => VgmHeaderField::Es5503,
            "ES5506 clock" => VgmHeaderField::Es5506,
            "ES5506 channels" => VgmHeaderField::Es5503OutputChannels,
            "C352 clock divider" => VgmHeaderField::C352ClockDivider,
            "X1-010 clock" => VgmHeaderField::X1_010,
            "C352 clock" => VgmHeaderField::C352,
            "GA20 clock" => VgmHeaderField::Ga20,
            "Mikey clock" => VgmHeaderField::Mikey,
            "Reserved E8-EF" => VgmHeaderField::ReservedE8EF,
            "Reserved F0-FF" => VgmHeaderField::ReservedF0FF,
            _ => return None,
        };

        field
            .byte_range(document.header.version, document.header.data_offset)
            .map(|(start, len)| ByteRange::new(start, len))
    }
}

#[cfg(test)]
mod tests {
    use super::VgmAdapter;
    use soundlog::VgmBuilder;
    use soundlog::vgm::command::WaitSamples;

    #[test]
    fn adapter_parses_serializes_and_maps_vgm_commands() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1));
        let document = builder.finalize();
        let bytes: Vec<u8> = (&document).into();

        let parsed = VgmAdapter::parse(&bytes).unwrap();
        let rebuilt = VgmAdapter::canonical_bytes(&parsed);
        let ranges = VgmAdapter::command_ranges(&parsed);

        assert_eq!(parsed.commands.len(), 2);
        assert_eq!(rebuilt, bytes);
        assert_eq!(ranges.len(), parsed.commands.len());
        assert!(ranges.iter().all(|range| range.len > 0));
    }

    #[test]
    fn adapter_reports_invalid_vgm_bytes() {
        let error = VgmAdapter::parse(&[1, 2, 3]).unwrap_err();

        assert!(error.to_string().contains("VGM"));
    }

    #[test]
    fn adapter_builds_command_nodes_with_source_ranges() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1));
        let document = builder.finalize();
        let bytes: Vec<u8> = (&document).into();
        let parsed = VgmAdapter::parse(&bytes).unwrap();

        let nodes = VgmAdapter::command_nodes(&parsed, 0, 1);

        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].label.starts_with("0: "));
        assert!(nodes[0].range.unwrap().range.len > 0);
    }

    #[test]
    fn adapter_builds_gd3_node_with_field_ranges() {
        use soundlog::meta::Gd3;

        let mut builder = VgmBuilder::new();
        builder.set_gd3(Gd3 {
            track_name_en: Some("Adapter test".to_string()),
            ..Default::default()
        });
        let document = builder.finalize();
        let bytes: Vec<u8> = (&document).into();
        let parsed = VgmAdapter::parse(&bytes).unwrap();
        let node = VgmAdapter::gd3_node(&parsed).unwrap();

        assert_eq!(node.label, "GD3");
        assert_eq!(node.children[0].label, "Track name (EN)");
        assert!(node.children[0].range.unwrap().range.len > 0);
        assert!(node.range.unwrap().range.len > 0);
    }

    #[test]
    fn adapter_maps_known_header_fields() {
        let document = VgmBuilder::new().finalize();

        assert_eq!(
            VgmAdapter::header_range(&document, "Ident"),
            Some(super::ByteRange::new(0, 4))
        );
        assert_eq!(VgmAdapter::header_range(&document, "unknown"), None);
    }

    #[test]
    fn adapter_builds_header_node_with_field_ranges() {
        let mut builder = VgmBuilder::new();
        builder.add_vgm_command(WaitSamples(1));
        let document = builder.finalize();

        let node = VgmAdapter::header_node(&document);

        assert_eq!(node.label, "Header");
        assert_eq!(node.children[0].label, "Ident");
        assert_eq!(
            node.children[0].range.unwrap().range,
            super::ByteRange::new(0, 4)
        );
        assert_eq!(node.range.unwrap().range.start, 0);
        assert!(node.range.unwrap().range.len > 4);
    }
}

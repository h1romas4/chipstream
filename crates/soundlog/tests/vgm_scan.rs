use soundlog::chip::{Chip, Okim6258Spec};
use soundlog::vgm::command::{DataBlock, Instance, VgmCommand, WaitSamples};
use soundlog::vgm::parser::visit_non_data_block_commands;
use soundlog::vgm::{VgmBuilder, VgmDocument, VgmHeader};

#[test]
fn scans_commands_without_visiting_data_blocks() {
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
    builder.add_vgm_command(DataBlock {
        marker: 0x66,
        chip_instance: 1,
        data_type: 0,
        size: 3,
        data: vec![0x61, 0x66, 0x67],
    });
    builder.add_chip_write(
        Instance::Secondary,
        Okim6258Spec {
            register: 0x0a,
            value: 0x60,
        },
    );
    builder.add_vgm_command(WaitSamples(5));
    let bytes: Vec<u8> = builder.finalize().into();
    let header = VgmHeader::from_bytes(&bytes).unwrap();
    let document = VgmDocument::try_from(bytes.as_slice()).unwrap();
    assert_eq!(
        header.extra_header_from_bytes(&bytes).unwrap(),
        document.extra_header
    );
    let mut commands = Vec::new();
    visit_non_data_block_commands(&bytes, &header, |command| commands.push(command.clone()))
        .unwrap();
    let expected: Vec<_> = document
        .commands
        .into_iter()
        .filter(|command| !matches!(command, VgmCommand::DataBlock(_)))
        .collect();
    assert_eq!(commands, expected);
}

#[test]
fn scan_rejects_truncated_blocks_and_other_commands() {
    let mut bytes: Vec<u8> = VgmBuilder::new().finalize().into();
    let header = VgmHeader::from_bytes(&bytes).unwrap();
    let start = VgmHeader::command_start(header.version, header.data_offset);
    for data in [
        &[0x67, 0x66][..],
        &[0x67, 0x66, 0, 5, 0, 0, 0x80, 1][..],
        &[0x61, 1][..],
    ] {
        bytes.truncate(start);
        bytes.extend_from_slice(data);
        assert!(visit_non_data_block_commands(&bytes, &header, |_| {}).is_err());
        assert!(VgmDocument::try_from(bytes.as_slice()).is_err());
    }
}

#[test]
fn extra_header_errors_match_document_parser() {
    let mut bytes: Vec<u8> = VgmBuilder::new().finalize().into();
    bytes[0xbc..0xc0].copy_from_slice(&0x0100_0000_u32.to_le_bytes());
    let header = VgmHeader::from_bytes(&bytes).unwrap();
    assert!(header.extra_header_from_bytes(&bytes).is_err());
    assert!(VgmDocument::try_from(bytes.as_slice()).is_err());
}

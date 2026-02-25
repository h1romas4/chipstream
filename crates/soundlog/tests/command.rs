// chipstream/crates/soundlog/tests/command.rs
//
// Round-trip test for DAC stream control using the Secondary instance.
// This test builds a small VGM document (via VgmBuilder), including a
// SetupStreamControl that targets the SECONDARY instance, then
// serializes the document and feeds it to VgmStream to ensure the
// generated writes are associated with the Secondary instance.

use soundlog::VgmBuilder;
use soundlog::vgm::command::ChipId;
use soundlog::vgm::command::{
    DacStreamChipType, DataBlock, EndOfData, Instance, LengthMode, SetStreamData,
    SetStreamFrequency, SetupStreamControl, StartStream, StopStream, VgmCommand, WaitSamples,
};
use soundlog::vgm::stream::StreamResult;
use soundlog::vgm::stream::VgmStream;

/// Construct a VGM document that routes a data bank to YM2612 Secondary
/// instance and ensure the generated writes are tagged as Secondary.
#[test]
fn test_dac_stream_chip_type_roundtrip_secondary() {
    let mut builder = VgmBuilder::new();

    // Simple uncompressed stream data block (type 0x00 = YM2612 PCM)
    let stream_data = vec![0x01, 0x02, 0x03, 0x04];
    let block = DataBlock {
        marker: 0x66,
        chip_instance: Instance::Primary as u8,
        data_type: 0x00,
        size: stream_data.len() as u32,
        data: stream_data.clone(),
    };
    builder.add_vgm_command(block);

    // Configure stream 0 to write to YM2612 (secondary instance), register 0x2A
    builder.add_vgm_command(SetupStreamControl {
        stream_id: 0,
        chip_type: DacStreamChipType {
            chip_id: ChipId::Ym2612,
            instance: Instance::Secondary,
        },
        write_port: 0,
        write_command: 0x2A,
    });

    // Point stream 0 at data bank 0x00 with step size 1
    builder.add_vgm_command(SetStreamData {
        stream_id: 0,
        data_bank_id: 0x00,
        step_size: 1,
        step_base: 0,
    });

    // Set stream frequency
    builder.add_vgm_command(SetStreamFrequency {
        stream_id: 0,
        frequency: 22050,
    });

    // Start stream: use CommandCount length to process 4 commands
    builder.add_vgm_command(StartStream {
        stream_id: 0,
        data_start_offset: 0,
        length_mode: LengthMode::CommandCount {
            reverse: false,
            looped: false,
        },
        data_length: 4,
    });

    // Wait long enough for the stream writes to be emitted
    builder.add_vgm_command(WaitSamples(100));

    // Stop stream and end
    builder.add_vgm_command(StopStream { stream_id: 0 });
    builder.add_vgm_command(EndOfData);

    // Finalize and serialize
    let doc = builder.finalize();
    let bytes: Vec<u8> = (&doc).into();

    // Parse with VgmStream and check emitted commands
    let mut stream = VgmStream::new();
    stream.push_chunk(&bytes).expect("push chunk");

    let mut seen_secondary_write = false;
    for res in &mut stream {
        match res {
            Ok(StreamResult::Command(cmd)) => {
                if let VgmCommand::Ym2612Write(inst, spec) = cmd
                    && inst == Instance::Secondary
                    && spec.register == 0x2A
                {
                    seen_secondary_write = true;
                    break;
                }
            }
            Ok(_) => {}
            Err(e) => panic!("stream error: {:?}", e),
        }
    }

    assert!(
        seen_secondary_write,
        "Should see YM2612 write for Secondary instance (roundtrip)"
    );
}

//! Example demonstrating VgmCallbackStream usage
//!
//! This example shows how to use VgmCallbackStream to track chip state
//! and receive callbacks for chip register writes with automatic event detection.

use soundlog::chip::Chip;
use soundlog::chip::event::StateEvent;
use soundlog::vgm::command::Instance;
use soundlog::vgm::stream::StreamResult;
use soundlog::{VgmBuilder, VgmCallbackStream, VgmCommand, VgmStream};

fn main() {
    println!("VgmCallbackStream Demo\n");

    // Create a simple VGM document with YM2612 commands
    let doc = create_sample_vgm();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Wrap the stream with VgmCallbackStream and enable state tracking
    let mut callback_stream = VgmCallbackStream::new(stream);
    // Enable state tracking using State type
    callback_stream
        .track_state::<soundlog::chip::state::Ym2612State>(Instance::Primary, 7_670_454.0); // NTSC Genesis clock
    callback_stream.on_write(|inst, spec: soundlog::chip::Ym2612Spec, sample, event| {
        println!(
            "YM2612[{:?}] @ sample {} Port={} Reg=0x{:02X} Val=0x{:02X}",
            inst, sample, spec.port, spec.register, spec.value
        );

        // Handle state events
        if let Some(events) = event {
            for ev in events {
                match ev {
                    StateEvent::KeyOn { channel, tone } => {
                        println!(
                            "  ▶ KeyOn: Channel {} | F-Num={} Block={} | Freq={:.2}Hz",
                            channel,
                            tone.fnum,
                            tone.block,
                            tone.freq_hz.unwrap_or(0.0)
                        );
                    }
                    StateEvent::KeyOff { channel } => {
                        println!("  ■ KeyOff: Channel {}", channel);
                    }
                    StateEvent::ToneChange { channel, tone } => {
                        println!(
                            "  ♪ ToneChange: Channel {} | F-Num={} Block={} | Freq={:.2}Hz",
                            channel,
                            tone.fnum,
                            tone.block,
                            tone.freq_hz.unwrap_or(0.0)
                        );
                    }
                }
            }
        }
    });
    callback_stream.on_any_command(|cmd, _sample| {
        // This callback is called for every command
        match cmd {
            VgmCommand::WaitSamples(w) => {
                println!(
                    "⏱  Wait {} samples ({:.2}ms @ 44.1kHz)",
                    w.0,
                    w.0 as f32 / 44.1
                );
            }
            VgmCommand::EndOfData(_) => {
                println!("⏹  End of data");
            }
            _ => {} // Other commands are already handled by chip-specific callbacks
        }
    });

    println!("Processing VGM stream...\n");

    // Process the stream
    for result in callback_stream {
        match result {
            Ok(StreamResult::Command(_)) => {
                // Callbacks have already been invoked
            }
            Ok(StreamResult::EndOfStream) => {
                println!("\n✓ Stream processing complete");
                break;
            }
            Ok(StreamResult::NeedsMoreData) => {
                println!("⚠ Stream needs more data");
                break;
            }
            Err(e) => {
                eprintln!("✗ Error: {:?}", e);
                break;
            }
        }
    }
}

/// Create a sample VGM document with YM2612 commands
fn create_sample_vgm() -> soundlog::VgmDocument {
    let mut builder = VgmBuilder::new();

    // Register YM2612 chip
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);

    // Channel 0: Set frequency (A4 = 440Hz)
    // Block = 4, F-Num = 0x26D (for A4 @ NTSC clock)
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        soundlog::chip::Ym2612Spec {
            port: 0,
            register: 0xA4, // Block/F-Num high bits
            value: 0x22,    // Block=4, F-Num[10:8]=2
        },
    ));
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        soundlog::chip::Ym2612Spec {
            port: 0,
            register: 0xA0, // F-Num low bits
            value: 0x6D,    // F-Num[7:0]=0x6D
        },
    ));

    // Key on channel 0 (all operators)
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        soundlog::chip::Ym2612Spec {
            port: 0,
            register: 0x28, // Key on/off
            value: 0xF0,    // All operators on, channel 0
        },
    ));

    // Wait a bit
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(44100)); // 1 second @ 44.1kHz

    // Change frequency while key is on (pitch bend to C5)
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        soundlog::chip::Ym2612Spec {
            port: 0,
            register: 0xA4,
            value: 0x25, // Block=4, different F-Num
        },
    ));
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        soundlog::chip::Ym2612Spec {
            port: 0,
            register: 0xA0,
            value: 0xE7,
        },
    ));

    // Wait
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(44100)); // 1 second

    // Key off channel 0
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        soundlog::chip::Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0x00, // All operators off, channel 0
        },
    ));

    // Finalize the document
    builder.finalize()
}

use soundlog::chip::state::Ym3812State;
use soundlog::chip::{
    Ay8910Spec, C352Spec, Es5506U16Spec, GbDmgSpec, Huc6280Spec, K054539Spec, MikeySpec,
    MultiPcmSpec, NesApuSpec, Okim6295Spec, PokeySpec, PsgSpec, QsoundSpec, Rf5c68U16Spec,
    Saa1099Spec, Scc1Spec, SegaPcmSpec, VsuSpec, Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec,
    Ym2608Spec, Ym2610Spec, Ym2612Spec, Ym3526Spec, Ym3812Spec, Ymf262Spec, Ymf271Spec,
    Ymf278bSpec, Ymz280bSpec,
};
use soundlog::chip::{
    event::StateEvent,
    state::{
        Ay8910State, C352State, ChipState, Es5506State, GbDmgState, Huc6280State, K051649State,
        K054539State, MikeyState, MultiPcmState, NesApuState, Okim6295State, PokeyState,
        QsoundState, Rf5c68State, Saa1099State, SegaPcmState, Sn76489State, VsuState, Y8950State,
        Ym2151State, Ym2203State, Ym2413State, Ym2608State, Ym2610bState, Ym2612State, Ym3526State,
        Ymf262State, Ymf271State, Ymf278bState, Ymz280bState,
    },
};
use soundlog::vgm::command::{Instance, VgmCommand};
use soundlog::vgm::stream::StreamResult;
use soundlog::{VgmBuilder, VgmStream};

#[test]
fn test_ym2612_state_tracking() {
    // Create a VGM document with YM2612 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ym2612, Instance::Primary, 7_670_454);

    // Set frequency registers for channel 0 (port 0)
    // Using distinct values to verify port separation
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0xA4,
            value: 0x22, // Block 4, F-num high 2
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0xA0,
            value: 0x11, // F-num low 0x11 (distinct value for port 0)
        },
    );
    // Set frequency registers for channel 3 (port 1)
    // Using clearly different values for same register addresses
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 1,
            register: 0xA4,
            value: 0x33, // Block 6, F-num high 3 (different from port 0's 0x22)
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 1,
            register: 0xA0,
            value: 0x99, // F-num low 0x99 (clearly different from port 0's 0x11)
        },
    );
    // Key on channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF0, // Key on channel 0, all operators
        },
    );
    // Key on channel 3 (port 1)
    builder.add_chip_write(
        Instance::Primary,
        Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF4, // Key on channel 3, all operators
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YM2612 state tracker
    let mut state = Ym2612State::new(7_670_454.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym2612Write(Instance::Primary, spec))) => {
                // YM2612 requires setting the port before writing to registers
                state.set_port(spec.port);
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on events for both channels
    assert_eq!(events.len(), 2);
    match &events[0] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 0);
            // F-num: low 0x11 | (high 2 << 8) = 0x211
            assert_eq!(tone.fnum, 0x211);
            // Block: 4
            assert_eq!(tone.block, 4);
            assert!(tone.freq_hz.is_some());
        }
        _ => panic!("Expected KeyOn event for channel 0"),
    }
    match &events[1] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 3);
            // F-num: low 0x99 | (high 3 << 8) = 0x399
            assert_eq!(tone.fnum, 0x399);
            // Block: 6
            assert_eq!(tone.block, 6);
            assert!(tone.freq_hz.is_some());
        }
        _ => panic!("Expected KeyOn event for channel 3"),
    }

    // Verify registers are stored correctly with port information
    // YM2612 stores port 0 and port 1 registers separately using port-encoded addresses
    // Same register addresses (0xA4, 0xA0) should hold different values per port
    state.set_port(0);
    assert_eq!(state.read_register(0xA4), Some(0x22)); // Port 0: 0x22
    assert_eq!(state.read_register(0xA0), Some(0x11)); // Port 0: 0x11
    assert_eq!(state.read_register(0x28), Some(0xF4)); // Last key on (port independent)
    state.set_port(1);
    assert_eq!(state.read_register(0xA4), Some(0x33)); // Port 1: 0x33 (different!)
    assert_eq!(state.read_register(0xA0), Some(0x99)); // Port 1: 0x99 (different!)
}

#[test]
fn test_ym2413_state_tracking() {
    // Create a VGM document with YM2413 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ym2413, Instance::Primary, 3_579_545);

    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2413Spec {
            register: 0x10,
            value: 0x6D,
        },
    );
    // Key on channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2413Spec {
            register: 0x20,
            value: 0x1F,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YM2413 state tracker
    let mut state = Ym2413State::new(3_579_545.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym2413Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify registers are stored correctly
    assert_eq!(state.read_register(0x10), Some(0x6D));
    assert_eq!(state.read_register(0x20), Some(0x1F));
}

#[test]
fn test_ym2151_state_tracking() {
    // Create a VGM document with YM2151 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ym2151, Instance::Primary, 4_000_000);

    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x28,
            value: 0x22,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x30,
            value: 0x00,
        },
    );
    // Key on channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2151Spec {
            register: 0x08,
            value: 0xF0,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YM2151 state tracker
    let mut state = Ym2151State::new(4_000_000.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym2151Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 128);
            assert_eq!(tone.block, 2);
            assert!(tone.freq_hz.is_some());
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify registers are stored correctly
    assert_eq!(state.read_register(0x28), Some(0x22));
    assert_eq!(state.read_register(0x30), Some(0x00));
    assert_eq!(state.read_register(0x08), Some(0xF0));
}

#[test]
fn test_nes_apu_state_tracking() {
    // Create a VGM document with NES APU register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::NesApu, Instance::Primary, 1_789_773);

    // Set pulse channel 0 registers
    builder.add_chip_write(
        Instance::Primary,
        NesApuSpec {
            register: 0x00,
            value: 0xBF,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        NesApuSpec {
            register: 0x02,
            value: 0x6D,
        },
    );
    // Set length and trigger
    builder.add_chip_write(
        Instance::Primary,
        NesApuSpec {
            register: 0x03,
            value: 0x02,
        },
    );
    // Enable pulse channel 0
    builder.add_chip_write(
        Instance::Primary,
        NesApuSpec {
            register: 0x15,
            value: 0x01,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create NES APU state tracker
    let mut state = NesApuState::new(0.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::NesApuWrite(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify registers are stored correctly
    assert_eq!(state.read_register(0x00), Some(0xBF));
    assert_eq!(state.read_register(0x02), Some(0x6D));
    assert_eq!(state.read_register(0x03), Some(0x02));
    assert_eq!(state.read_register(0x15), Some(0x01));
}

#[test]
fn test_ym3812_state_tracking() {
    // Create a VGM document with YM3812 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ym3812, Instance::Primary, 3_579_545);

    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym3812Spec {
            register: 0xA0,
            value: 0x6D,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym3812Spec {
            register: 0xB0,
            value: 0x21,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YM3812 state tracker
    let mut state = Ym3812State::new(3_579_545.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym3812Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify registers are stored correctly
    assert_eq!(state.read_register(0xB0), Some(0x21));
    assert_eq!(state.read_register(0xA0), Some(0x6D));
}

#[test]
fn test_ay8910_state_tracking() {
    // Create a VGM document with AY8910 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ay8910, Instance::Primary, 1_789_773);

    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ay8910Spec {
            register: 0x00,
            value: 0xCD,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ay8910Spec {
            register: 0x01,
            value: 0x02,
        },
    );
    // Enable channel 0 tone
    builder.add_chip_write(
        Instance::Primary,
        Ay8910Spec {
            register: 0x07,
            value: 0b11111110,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create AY8910 state tracker
    let mut state = Ay8910State::new(1_789_773.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ay8910Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 0);
            assert_eq!(tone.fnum, 0x2CD);
            assert!(tone.freq_hz.is_some());
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (AY-3-8910 has 3 channels)
    assert_eq!(state.channel_count(), 3);

    // Verify that registers are stored correctly
    // AY-3-8910 uses direct register addressing
    assert_eq!(state.read_register(0x00), Some(0xCD)); // Channel A fine tune
    assert_eq!(state.read_register(0x01), Some(0x02)); // Channel A coarse tune
    assert_eq!(state.read_register(0x07), Some(0b11111110)); // Mixer control
}

#[test]
fn test_sn76489_state_tracking() {
    // Create a VGM document with SN76489 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Sn76489, Instance::Primary, 3_579_545);

    // Latch channel 0 frequency
    builder.add_chip_write(Instance::Primary, PsgSpec { value: 0x8D });
    // High bits
    builder.add_chip_write(Instance::Primary, PsgSpec { value: 0x26 });
    // Set volume to key on
    builder.add_chip_write(Instance::Primary, PsgSpec { value: 0x90 });

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create SN76489 state tracker
    let mut state = Sn76489State::new(3_579_545.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Sn76489Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(0, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (SN76489 uses latched addressing, register storage is internal)
    assert_eq!(state.channel_count(), 4);

    // Verify that registers are stored correctly in global storage
    // SN76489 stores frequency in registers (channel * 2) and (channel * 2 + 1)
    // Volume is stored in register (8 + channel)
    // Channel 0 frequency: registers 0 and 1
    // Channel 0 volume: register 8
    assert_eq!(state.read_register(0), Some(0x0D)); // Frequency low
    assert_eq!(state.read_register(1), Some(0x26)); // Frequency high
    assert_eq!(state.read_register(8), Some(0x00)); // Volume (attenuation=0)
}

#[test]
fn test_gb_dmg_state_tracking() {
    // Create a VGM document with Game Boy DMG register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::GbDmg, Instance::Primary, 4_194_304);

    // Enable wave DAC
    builder.add_chip_write(
        Instance::Primary,
        GbDmgSpec {
            register: 0x1A,
            value: 0x80,
        },
    );
    // Set frequency for wave channel
    builder.add_chip_write(
        Instance::Primary,
        GbDmgSpec {
            register: 0x1D,
            value: 0x00,
        },
    );
    // Trigger wave channel
    builder.add_chip_write(
        Instance::Primary,
        GbDmgSpec {
            register: 0x1E,
            value: 0x84,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = GbDmgState::new(0.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::GbDmgWrite(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event for wave channel
    assert!(!events.is_empty());
    let has_key_on = events
        .iter()
        .any(|e| matches!(e, StateEvent::KeyOn { channel: 2, .. }));
    assert!(has_key_on);

    // Verify registers are stored correctly (Game Boy DMG)
    assert_eq!(state.read_register(0x1A), Some(0x80));
    assert_eq!(state.read_register(0x1D), Some(0x00));
    assert_eq!(state.read_register(0x1E), Some(0x84));
    assert_eq!(state.channel_count(), 4);
}

#[test]
fn test_ym2608_state_tracking() {
    // Create a VGM document with YM2608 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ym2608, Instance::Primary, 8_000_000);

    // Set frequency registers for channel 0 (port 0)
    // Using distinct values to verify port separation
    builder.add_chip_write(
        Instance::Primary,
        Ym2608Spec {
            port: 0,
            register: 0xA4,
            value: 0x22, // Block 4, F-num high 2
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2608Spec {
            port: 0,
            register: 0xA0,
            value: 0x11, // F-num low 0x11 (distinct value for port 0)
        },
    );
    // Set frequency registers for channel 3 (port 1)
    // Using clearly different values for same register addresses
    builder.add_chip_write(
        Instance::Primary,
        Ym2608Spec {
            port: 1,
            register: 0xA4,
            value: 0x33, // Block 6, F-num high 3 (different from port 0's 0x22)
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2608Spec {
            port: 1,
            register: 0xA0,
            value: 0x99, // F-num low 0x99 (clearly different from port 0's 0x11)
        },
    );
    // Key on channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2608Spec {
            port: 0,
            register: 0x28,
            value: 0xF0, // Key on channel 0, all operators
        },
    );
    // Key on channel 3 (port 1)
    builder.add_chip_write(
        Instance::Primary,
        Ym2608Spec {
            port: 0,
            register: 0x28,
            value: 0xF4, // Key on channel 3, all operators
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YM2608 state tracker
    let mut state = Ym2608State::new(8_000_000.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym2608Write(Instance::Primary, spec))) => {
                // YM2608 requires setting the port before writing to registers
                state.set_port(spec.port);
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on events for both channels
    assert_eq!(events.len(), 2);
    match &events[0] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 0);
            // F-num: low 0x11 | (high 2 << 8) = 0x211
            assert_eq!(tone.fnum, 0x211);
            // Block: 4
            assert_eq!(tone.block, 4);
            assert!(tone.freq_hz.is_some());
        }
        _ => panic!("Expected KeyOn event for channel 0"),
    }
    match &events[1] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 3);
            // F-num: low 0x99 | (high 3 << 8) = 0x399
            assert_eq!(tone.fnum, 0x399);
            // Block: 6
            assert_eq!(tone.block, 6);
            assert!(tone.freq_hz.is_some());
        }
        _ => panic!("Expected KeyOn event for channel 3"),
    }

    // Verify registers are stored correctly with port information
    // YM2608 stores port 0 and port 1 registers separately using port-encoded addresses
    // Same register addresses (0xA4, 0xA0) should hold different values per port
    state.set_port(0);
    assert_eq!(state.read_register(0xA4), Some(0x22)); // Port 0: 0x22
    assert_eq!(state.read_register(0xA0), Some(0x11)); // Port 0: 0x11
    assert_eq!(state.read_register(0x28), Some(0xF4)); // Last key on (port independent)
    state.set_port(1);
    assert_eq!(state.read_register(0xA4), Some(0x33)); // Port 1: 0x33 (different!)
    assert_eq!(state.read_register(0xA0), Some(0x99)); // Port 1's value (different!)
}

#[test]
fn test_ym2610b_state_tracking() {
    // Create a VGM document with YM2610B register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ym2610b, Instance::Primary, 8_000_000);

    // Set frequency registers for channel 0 (port 0)
    // Using distinct values to verify port separation
    builder.add_chip_write(
        Instance::Primary,
        Ym2610Spec {
            port: 0,
            register: 0xA4,
            value: 0x22, // Block 4, F-num high 2
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2610Spec {
            port: 0,
            register: 0xA0,
            value: 0x11, // F-num low 0x11 (distinct value for port 0)
        },
    );
    // Set frequency registers for channel 3 (port 1)
    // Using clearly different values for same register addresses
    builder.add_chip_write(
        Instance::Primary,
        Ym2610Spec {
            port: 1,
            register: 0xA4,
            value: 0x33, // Block 6, F-num high 3 (different from port 0's 0x22)
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2610Spec {
            port: 1,
            register: 0xA0,
            value: 0x99, // F-num low 0x99 (clearly different from port 0's 0x11)
        },
    );
    // Key on channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2610Spec {
            port: 0,
            register: 0x28,
            value: 0xF0, // Key on channel 0, all operators
        },
    );
    // Key on channel 3 (port 1)
    builder.add_chip_write(
        Instance::Primary,
        Ym2610Spec {
            port: 0,
            register: 0x28,
            value: 0xF4, // Key on channel 3, all operators
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YM2610b state tracker
    let mut state = Ym2610bState::new(8_000_000.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym2610bWrite(Instance::Primary, spec))) => {
                // YM2610B requires setting the port before writing to registers
                state.set_port(spec.port);
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on events for both channels
    assert_eq!(events.len(), 2);
    match &events[0] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 0);
            // F-num: low 0x11 | (high 2 << 8) = 0x211
            assert_eq!(tone.fnum, 0x211);
            // Block: 4
            assert_eq!(tone.block, 4);
            assert!(tone.freq_hz.is_some());
        }
        _ => panic!("Expected KeyOn event for channel 0"),
    }
    match &events[1] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 3);
            // F-num: low 0x99 | (high 3 << 8) = 0x399
            assert_eq!(tone.fnum, 0x399);
            // Block: 6
            assert_eq!(tone.block, 6);
            assert!(tone.freq_hz.is_some());
        }
        _ => panic!("Expected KeyOn event for channel 3"),
    }

    // Verify registers are stored correctly with port information
    // YM2610B stores port 0 and port 1 registers separately using port-encoded addresses
    // Same register addresses (0xA4, 0xA0) should hold different values per port
    state.set_port(0);
    assert_eq!(state.read_register(0xA4), Some(0x22)); // Port 0: 0x22
    assert_eq!(state.read_register(0xA0), Some(0x11)); // Port 0: 0x11
    assert_eq!(state.read_register(0x28), Some(0xF4)); // Last key on (port independent)
    state.set_port(1);
    assert_eq!(state.read_register(0xA4), Some(0x33)); // Port 1: 0x33 (different!)
    assert_eq!(state.read_register(0xA0), Some(0x99)); // Port 1's value (different!)
}

#[test]
fn test_ym2203_state_tracking() {
    // Create a VGM document with YM2203 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ym2203, Instance::Primary, 4_000_000);

    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2203Spec {
            register: 0xA4,
            value: 0x22, // Block 4, F-num high 2
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym2203Spec {
            register: 0xA0,
            value: 0x6D, // F-num low 0x6D
        },
    );
    // Key on channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym2203Spec {
            register: 0x28,
            value: 0xF0, // Key on channel 0, all operators
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YM2203 state tracker
    let mut state = Ym2203State::new(4_000_000.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym2203Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, tone } => {
            assert_eq!(*channel, 0);
            // F-num: low 0x6D | (high 2 << 8) = 0x26D
            assert_eq!(tone.fnum, 0x26D);
            // Block: 4
            assert_eq!(tone.block, 4);
            // Frequency should be calculated correctly
            assert!(tone.freq_hz.is_some());
            let freq = tone.freq_hz.unwrap();
            // Approximate frequency check (exact value depends on calculation)
            assert!(freq > 200.0 && freq < 400.0); // Rough check for this fnum/block
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify registers are stored correctly
    assert_eq!(state.read_register(0xA4), Some(0x22));
    assert_eq!(state.read_register(0xA0), Some(0x6D));
    assert_eq!(state.read_register(0x28), Some(0xF0));
}

#[test]
fn test_ym3526_state_tracking() {
    // Create a VGM document with YM3526 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ym3526, Instance::Primary, 3_579_545);

    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ym3526Spec {
            register: 0xA0,
            value: 0x6D,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ym3526Spec {
            register: 0xB0,
            value: 0x21,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YM3526 state tracker
    let mut state = Ym3526State::new(3_579_545.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ym3526Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify registers are stored correctly (YM3526)
    assert_eq!(state.read_register(0xB0), Some(0x21));
    assert_eq!(state.read_register(0xA0), Some(0x6D));
}

#[test]
fn test_y8950_state_tracking() {
    // Create a VGM document with Y8950 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Y8950, Instance::Primary, 3_579_545);

    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Y8950Spec {
            register: 0xA0,
            value: 0x6D,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Y8950Spec {
            register: 0xB0,
            value: 0x21,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create Y8950 state tracker
    let mut state = Y8950State::new(3_579_545.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Y8950Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify registers are stored correctly (Y8950)
    assert_eq!(state.read_register(0xB0), Some(0x21));
    assert_eq!(state.read_register(0xA0), Some(0x6D));
}

#[test]
fn test_ymf262_state_tracking() {
    // Create a VGM document with YMF262 (OPL3) register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ymf262, Instance::Primary, 14_318_180);

    // Set frequency registers for channel 0 (port 0)
    // Using distinct values to verify port separation
    builder.add_chip_write(
        Instance::Primary,
        Ymf262Spec {
            port: 0,
            register: 0xA0,
            value: 0x12, // F-num low (distinct value for port 0)
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymf262Spec {
            port: 0,
            register: 0xB0,
            value: 0x31, // Key on + block 3 + F-num high 1
        },
    );
    // Set frequency registers for channel 9 (port 1)
    // Using clearly different values for same register addresses
    builder.add_chip_write(
        Instance::Primary,
        Ymf262Spec {
            port: 1,
            register: 0xA0,
            value: 0xAB, // F-num low (clearly different from port 0's 0x12)
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymf262Spec {
            port: 1,
            register: 0xB0,
            value: 0x25, // Key on + block 2 + F-num high 1 (different from port 0's 0x31)
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YMF262 state tracker
    let mut state = Ymf262State::new(14_318_180.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ymf262Write(Instance::Primary, spec))) => {
                // YMF262 requires setting the port before writing to registers
                state.set_port(spec.port);
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on events for both channels
    assert_eq!(events.len(), 2);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event for channel 0"),
    }
    match &events[1] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 9);
        }
        _ => panic!("Expected KeyOn event for channel 9"),
    }

    // Verify registers are stored correctly with port information
    // YMF262 stores port 0 and port 1 registers separately using port-encoded addresses
    // Same register addresses (0xA0, 0xB0) should hold different values per port
    state.set_port(0);
    assert_eq!(state.read_register(0xA0), Some(0x12)); // Port 0: 0x12
    assert_eq!(state.read_register(0xB0), Some(0x31)); // Port 0: 0x31
    state.set_port(1);
    assert_eq!(state.read_register(0xA0), Some(0xAB)); // Port 1: 0xAB (different!)
    assert_eq!(state.read_register(0xB0), Some(0x25)); // Port 1: 0x25 (different!)
}

#[test]
fn test_ymf278b_state_tracking() {
    // Create a VGM document with YMF278B (OPL4) register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ymf278b, Instance::Primary, 33_868_800);

    // Set frequency registers for channel 0 (port 0)
    // Using distinct values to verify port separation
    builder.add_chip_write(
        Instance::Primary,
        Ymf278bSpec {
            port: 0,
            register: 0xA0,
            value: 0x12, // F-num low (distinct value for port 0)
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymf278bSpec {
            port: 0,
            register: 0xB0,
            value: 0x31, // Key on + block 3 + F-num high 1
        },
    );
    // Set frequency registers for channel 9 (port 1)
    // Using clearly different values for same register addresses
    builder.add_chip_write(
        Instance::Primary,
        Ymf278bSpec {
            port: 1,
            register: 0xA0,
            value: 0xAB, // F-num low (clearly different from port 0's 0x12)
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymf278bSpec {
            port: 1,
            register: 0xB0,
            value: 0x25, // Key on + block 2 + F-num high 1 (different from port 0's 0x31)
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YMF278B state tracker
    let mut state = Ymf278bState::new(33_868_800.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ymf278bWrite(Instance::Primary, spec))) => {
                // YMF278B requires setting the port before writing to registers
                state.set_port(spec.port);
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on events for both channels
    assert_eq!(events.len(), 2);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event for channel 0"),
    }
    match &events[1] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 9);
        }
        _ => panic!("Expected KeyOn event for channel 9"),
    }

    // Verify registers are stored correctly with port information
    // YMF278B stores port 0 and port 1 registers separately using port-encoded addresses
    // Same register addresses (0xA0, 0xB0) should hold different values per port
    state.set_port(0);
    assert_eq!(state.read_register(0xA0), Some(0x12)); // Port 0: 0x12
    assert_eq!(state.read_register(0xB0), Some(0x31)); // Port 0: 0x31
    state.set_port(1);
    assert_eq!(state.read_register(0xA0), Some(0xAB)); // Port 1: 0xAB (different!)
    assert_eq!(state.read_register(0xB0), Some(0x25)); // Port 1: 0x25 (different!)
    // Verify channel count (YMF278B has 18 FM channels)
    assert_eq!(state.channel_count(), 18);
}

#[test]
fn test_ymf271_state_tracking() {
    // Create a VGM document with YMF271 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ymf271, Instance::Primary, 16_934_400);

    // Select slot 0
    builder.add_chip_write(
        Instance::Primary,
        Ymf271Spec {
            port: 0,
            register: 0x80,
            value: 0x00,
        },
    );
    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ymf271Spec {
            port: 0,
            register: 12,
            value: 0x04,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymf271Spec {
            port: 0,
            register: 13,
            value: 0x02,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymf271Spec {
            port: 0,
            register: 14,
            value: 0x00,
        },
    );
    // Key on channel 0
    builder.add_chip_write(
        Instance::Primary,
        Ymf271Spec {
            port: 0,
            register: 0,
            value: 0x01,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create YMF271 state tracker
    let mut state = Ymf271State::new(16_934_400.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ymf271Write(Instance::Primary, spec))) => {
                state.set_selected_slot(spec.port);
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (YMF271 has complex addressing)
    assert_eq!(state.channel_count(), 12);
}

#[test]
fn test_scc1_state_tracking() {
    // Create a VGM document with K051649/SCC1 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::K051649, Instance::Primary, 1_500_000);

    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Scc1Spec {
            port: 0,
            register: 0x80,
            value: 0x00,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Scc1Spec {
            port: 0,
            register: 0x81,
            value: 0x08,
        },
    );
    // Enable channel 0
    builder.add_chip_write(
        Instance::Primary,
        Scc1Spec {
            port: 0,
            register: 0x8F,
            value: 0x01,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create SCC1 state tracker (same as K051649)
    let mut state = K051649State::new(1_500_000.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Scc1Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (K051649/SCC1 has 5 channels)
    assert_eq!(state.channel_count(), 5);

    // Verify that registers are stored correctly
    // K051649/SCC1 uses direct register addressing
    assert_eq!(state.read_register(0x80), Some(0x00)); // Channel 0 frequency low
    assert_eq!(state.read_register(0x81), Some(0x08)); // Channel 0 frequency high
    assert_eq!(state.read_register(0x8F), Some(0x01)); // Channel enable
}

#[test]
fn test_huc6280_state_tracking() {
    // Create a VGM document with HuC6280 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Huc6280, Instance::Primary, 3_579_545);

    // Select channel 0
    builder.add_chip_write(
        Instance::Primary,
        Huc6280Spec {
            register: 0x00,
            value: 0x00,
        },
    );
    // Set frequency registers for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Huc6280Spec {
            register: 0x02,
            value: 0x6D,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Huc6280Spec {
            register: 0x03,
            value: 0x02,
        },
    );
    // Enable channel 0
    builder.add_chip_write(
        Instance::Primary,
        Huc6280Spec {
            register: 0x04,
            value: 0x80,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create HuC6280 state tracker
    let mut state = Huc6280State::new(3_579_545.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Huc6280Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (HuC6280 has 6 channels)
    assert_eq!(state.channel_count(), 6);
}

#[test]
fn test_pokey_state_tracking() {
    // Create a VGM document with POKEY register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Pokey, Instance::Primary, 1_789_790);

    // Set frequency for channel 0
    builder.add_chip_write(
        Instance::Primary,
        PokeySpec {
            register: 0x00,
            value: 0x10,
        },
    );
    // Enable channel 0
    builder.add_chip_write(
        Instance::Primary,
        PokeySpec {
            register: 0x01,
            value: 0x08,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create Pokey state tracker
    let mut state = PokeyState::new(1_789_790.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::PokeyWrite(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (POKEY has 4 channels)
    assert_eq!(state.channel_count(), 4);
}

#[test]
fn test_vsu_state_tracking() {
    // Create a VGM document with VSU register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Vsu, Instance::Primary, 5_000_000);

    // Set frequency for channel 0
    builder.add_chip_write(
        Instance::Primary,
        VsuSpec {
            offset: 0x08,
            value: 0x00,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        VsuSpec {
            offset: 0x0C,
            value: 0x01,
        },
    );
    // Enable channel 0
    builder.add_chip_write(
        Instance::Primary,
        VsuSpec {
            offset: 0x00,
            value: 0x80,
        },
    );
    // Set volume
    builder.add_chip_write(
        Instance::Primary,
        VsuSpec {
            offset: 0x04,
            value: 0x11,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create VSU state tracker
    let mut state = VsuState::new(5_000_000.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::VsuWrite(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.offset, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (VSU has 6 channels)
    assert_eq!(state.channel_count(), 6);
}

#[test]
fn test_saa1099_state_tracking() {
    // Create a VGM document with SAA1099 register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Saa1099, Instance::Primary, 8_000_000);

    // Set frequency for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x80,
            value: 0x08,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x00,
            value: 0x6D,
        },
    );
    // Set octave
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x80,
            value: 0x10,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x00,
            value: 0x03,
        },
    );
    // Enable all channels
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x80,
            value: 0x1C,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x00,
            value: 0x01,
        },
    );
    // Enable frequency for channel 0
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x80,
            value: 0x14,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x00,
            value: 0x01,
        },
    );
    // Set amplitude
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x80,
            value: 0x00,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Saa1099Spec {
            register: 0x00,
            value: 0x88,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create SAA1099 state tracker
    let mut state = Saa1099State::new(8_000_000.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Saa1099Write(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (SAA1099 has 6 channels)
    assert_eq!(state.channel_count(), 6);

    // Verify that registers are stored correctly
    // SAA1099 uses two-stage addressing:
    // - Control writes (register >= 0x80) select the target register
    // - Data writes (register < 0x80) write to the selected register
    // Both types of writes are stored in the register storage (ArrayStorage<u8, 256>)
    assert_eq!(state.read_register(0x80), Some(0x00)); // Last control write (amplitude select)
    assert_eq!(state.read_register(0x00), Some(0x88)); // Last data write to register 0x00 (amplitude)
}

#[test]
fn test_mikey_state_tracking() {
    // Create a VGM document with Mikey register writes using VgmBuilder
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Mikey, Instance::Primary, 16_000_000);

    // Enable master
    builder.add_chip_write(
        Instance::Primary,
        MikeySpec {
            register: 0x50,
            value: 0x01,
        },
    );
    // Set counter for channel 0
    builder.add_chip_write(
        Instance::Primary,
        MikeySpec {
            register: 0x06,
            value: 0x40,
        },
    );
    // Set backup for channel 0
    builder.add_chip_write(
        Instance::Primary,
        MikeySpec {
            register: 0x04,
            value: 0x01,
        },
    );
    // Set volume for channel 0
    builder.add_chip_write(
        Instance::Primary,
        MikeySpec {
            register: 0x00,
            value: 0x7F,
        },
    );
    // Enable channel 0
    builder.add_chip_write(
        Instance::Primary,
        MikeySpec {
            register: 0x05,
            value: 0x08,
        },
    );

    let doc = builder.finalize();

    // Create a VgmStream from the document
    let stream = VgmStream::from_document(doc);

    // Create Mikey state tracker
    let mut state = MikeyState::new(16_000_000.0);

    // Collect state events while processing the stream
    let mut events = Vec::new();
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::MikeyWrite(Instance::Primary, spec))) => {
                if let Some(mut evs) = state.on_register_write(spec.register, spec.value) {
                    events.append(&mut evs);
                }
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify that we got the expected key-on event
    assert_eq!(events.len(), 1);
    match &events[0] {
        StateEvent::KeyOn { channel, .. } => {
            assert_eq!(*channel, 0);
        }
        _ => panic!("Expected KeyOn event"),
    }

    // Verify channel count (Mikey has 4 channels)
    assert_eq!(state.channel_count(), 4);
}

#[test]
fn test_sega_pcm_state_tracking() {
    // Create a VGM document with Sega PCM register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::SegaPcm, Instance::Primary, 4_000_000);

    // Write some registers (offset: u16, value: u8)
    builder.add_chip_write(
        Instance::Primary,
        SegaPcmSpec {
            offset: 0x0010,
            value: 0x42,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        SegaPcmSpec {
            offset: 0x0100,
            value: 0xAB,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        SegaPcmSpec {
            offset: 0x0200,
            value: 0xCD,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);

    // Create state tracker
    let mut state = SegaPcmState::new(0.0);

    // Process all commands (PCM chips don't generate events)
    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::SegaPcmWrite(Instance::Primary, spec))) => {
                let event = state.on_register_write(spec.offset, spec.value);
                assert!(event.is_none()); // PCM chips don't generate events
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify registers are stored correctly
    assert_eq!(state.read_register(0x0010), Some(0x42));
    assert_eq!(state.read_register(0x0100), Some(0xAB));
    assert_eq!(state.read_register(0x0200), Some(0xCD));
    assert_eq!(state.read_register(0x0300), None); // Not written
}

#[test]
fn test_rf5c68_state_tracking() {
    // Create a VGM document with RF5C68 register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Rf5c68, Instance::Primary, 12_500_000);

    // Write registers (offset: u16, value: u8)
    builder.add_chip_write(
        Instance::Primary,
        Rf5c68U16Spec {
            offset: 0x0000,
            value: 0x80,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Rf5c68U16Spec {
            offset: 0x0010,
            value: 0xFF,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = Rf5c68State::new(0.0);

    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Rf5c68U16Write(Instance::Primary, spec))) => {
                assert!(state.on_register_write(spec.offset, spec.value).is_none());
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    assert_eq!(state.read_register(0x0000), Some(0x80));
    assert_eq!(state.read_register(0x0010), Some(0xFF));
    assert_eq!(state.channel_count(), 8);
}

#[test]
fn test_ymz280b_state_tracking() {
    // Create a VGM document with YMZ280B register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Ymz280b, Instance::Primary, 16_934_400);

    // Write registers (register: u8, value: u8)
    builder.add_chip_write(
        Instance::Primary,
        Ymz280bSpec {
            register: 0x00,
            value: 0x80,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Ymz280bSpec {
            register: 0x10,
            value: 0x40,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = Ymz280bState::new(0.0);

    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Ymz280bWrite(Instance::Primary, spec))) => {
                assert!(state.on_register_write(spec.register, spec.value).is_none());
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    assert_eq!(state.read_register(0x00), Some(0x80));
    assert_eq!(state.read_register(0x10), Some(0x40));
    assert_eq!(state.channel_count(), 8);
}

#[test]
fn test_qsound_state_tracking() {
    // Create a VGM document with QSound register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Qsound, Instance::Primary, 4_000_000);

    // QSound has u8 register, u16 value
    builder.add_chip_write(
        Instance::Primary,
        QsoundSpec {
            register: 0x10,
            value: 0xBEEF,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        QsoundSpec {
            register: 0x20,
            value: 0x1234,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = QsoundState::new(0.0);

    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::QsoundWrite(Instance::Primary, spec))) => {
                assert!(state.on_register_write(spec.register, spec.value).is_none());
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify u16 values are stored correctly
    assert_eq!(state.read_register(0x10), Some(0xBEEF));
    assert_eq!(state.read_register(0x20), Some(0x1234));
    assert_eq!(state.channel_count(), 16);
}

#[test]
fn test_c352_state_tracking() {
    // Create a VGM document with C352 register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::C352, Instance::Primary, 24_192_000);

    // C352 has u16 register, u16 value
    builder.add_chip_write(
        Instance::Primary,
        C352Spec {
            register: 0x0100,
            value: 0x1234,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        C352Spec {
            register: 0x0200,
            value: 0xABCD,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = C352State::new(0.0);

    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::C352Write(Instance::Primary, spec))) => {
                assert!(state.on_register_write(spec.register, spec.value).is_none());
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify u16 register and u16 value storage
    assert_eq!(state.read_register(0x0100), Some(0x1234));
    assert_eq!(state.read_register(0x0200), Some(0xABCD));
    assert_eq!(state.channel_count(), 32);
}

#[test]
fn test_k054539_state_tracking() {
    // Create a VGM document with K054539 register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::K054539, Instance::Primary, 18_432_000);

    // K054539 has u16 register, u8 value
    builder.add_chip_write(
        Instance::Primary,
        K054539Spec {
            register: 0x0200,
            value: 0x42,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        K054539Spec {
            register: 0x0400,
            value: 0x99,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = K054539State::new(0.0);

    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::K054539Write(Instance::Primary, spec))) => {
                assert!(state.on_register_write(spec.register, spec.value).is_none());
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    assert_eq!(state.read_register(0x0200), Some(0x42));
    assert_eq!(state.read_register(0x0400), Some(0x99));
    assert_eq!(state.channel_count(), 8);
}

#[test]
fn test_okim6295_state_tracking() {
    // Create a VGM document with OKIM6295 register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::Okim6295, Instance::Primary, 1_000_000);

    builder.add_chip_write(
        Instance::Primary,
        Okim6295Spec {
            register: 0x00,
            value: 0x80,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Okim6295Spec {
            register: 0x10,
            value: 0x40,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = Okim6295State::new(0.0);

    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Okim6295Write(Instance::Primary, spec))) => {
                assert!(state.on_register_write(spec.register, spec.value).is_none());
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    assert_eq!(state.read_register(0x00), Some(0x80));
    assert_eq!(state.read_register(0x10), Some(0x40));
    assert_eq!(state.channel_count(), 4);
}

#[test]
fn test_es5506_state_tracking() {
    // Create a VGM document with ES5506 register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(
        soundlog::chip::Chip::Es5506U16,
        Instance::Primary,
        16_000_000,
    );

    // ES5506 has u8 register, u16 value (using 16-bit variant)
    builder.add_chip_write(
        Instance::Primary,
        Es5506U16Spec {
            register: 0x1A,
            value: 0xBEEF,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        Es5506U16Spec {
            register: 0x2B,
            value: 0x5678,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = Es5506State::new(0.0);

    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::Es5506D6Write(Instance::Primary, spec))) => {
                assert!(state.on_register_write(spec.register, spec.value).is_none());
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    // Verify u16 values are stored
    assert_eq!(state.read_register(0x1A), Some(0xBEEF));
    assert_eq!(state.read_register(0x2B), Some(0x5678));
    assert_eq!(state.channel_count(), 32);
}

#[test]
fn test_multi_pcm_state_tracking() {
    // Create a VGM document with MultiPCM register writes
    let mut builder = VgmBuilder::new();
    builder.register_chip(soundlog::chip::Chip::MultiPcm, Instance::Primary, 8_000_000);

    builder.add_chip_write(
        Instance::Primary,
        MultiPcmSpec {
            register: 0x00,
            value: 0xAA,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        MultiPcmSpec {
            register: 0xFF,
            value: 0xBB,
        },
    );

    let doc = builder.finalize();
    let stream = VgmStream::from_document(doc);
    let mut state = MultiPcmState::new(0.0);

    for result in stream {
        match result {
            Ok(StreamResult::Command(VgmCommand::MultiPcmWrite(Instance::Primary, spec))) => {
                assert!(state.on_register_write(spec.register, spec.value).is_none());
            }
            Ok(StreamResult::EndOfStream) | Ok(StreamResult::NeedsMoreData) => break,
            _ => {}
        }
    }

    assert_eq!(state.read_register(0x00), Some(0xAA));
    assert_eq!(state.read_register(0xFF), Some(0xBB));
    assert_eq!(state.channel_count(), 28);
}

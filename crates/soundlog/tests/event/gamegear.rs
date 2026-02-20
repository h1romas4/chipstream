// Game Gear PSG (mapped to SN76489) event test.
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Sn76489State;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::{Instance, WaitSamples};
use soundlog::{VgmBuilder, VgmCallbackStream};

/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const GG_PSG_TOLERANCE_HZ: f32 = 2.0;

#[test]
fn test_gamegear_psg_keyon_and_tone_freq_matches_a4() {
    // Target pitch
    let target_hz = 440.0_f32;

    // Game Gear / Master System PSG master clock (NTSC typical)
    let master_clock = 3_579_545.0_f32;

    // Compute freq_value = round(master / (32 * target))
    let ideal = master_clock / (32.0_f32 * target_hz);
    let freq_value = ideal.round() as u16;
    assert!(
        freq_value > 0 && freq_value <= 0x03FF,
        "freq_value out of range"
    );

    // Split into low4 and high6 parts (10-bit value)
    let low4 = (freq_value & 0x0F) as u8;
    let high6 = (freq_value >> 4) as u8; // up to 6 bits

    // Build a VGM document that:
    //  - registers GameGear PSG in the header
    //  - writes latch byte for frequency low (bit7=1, channel=0, type=0, data=low4)
    //  - writes data byte with high6 (bit7=0, data=high6)
    //  - writes volume latch to set audible volume (channel=0, volume=0 = max)
    //  - waits 22100 samples, then writes volume latch with silent (15) to key off
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Sn76489, Instance::Primary, master_clock as u32);

    // Latch byte for channel 0, frequency (type=0), low 4 bits in low nibble.
    // For channel 0 and type 0 the encoded value is simply 0x80 | data.
    let latch_freq = 0x80u8 | (low4 & 0x0F);
    // Data byte: high 6 bits (bit7 = 0)
    let data_high = high6 & 0x3F;
    // Volume latch: channel 0, type=1 (volume), data=0 (max volume)
    let volume_latch: u8 = 0x90;
    // Volume latch for key-off (silent = 15)
    let volume_latch_off: u8 = 0x9F;

    builder.add_chip_write(Instance::Primary, chip::PsgSpec { value: latch_freq });
    builder.add_chip_write(Instance::Primary, chip::PsgSpec { value: data_high });
    builder.add_chip_write(
        Instance::Primary,
        chip::PsgSpec {
            value: volume_latch,
        },
    );

    // Wait 22100 samples (~0.5s @44.1kHz) so downstream logic can detect duration,
    // then key off by setting volume to silent (15).
    builder.add_vgm_command(WaitSamples(22100));
    builder.add_chip_write(
        Instance::Primary,
        chip::PsgSpec {
            value: volume_latch_off,
        },
    );

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();

    // Optionally write artifact via shim helper; no-op if OUTPUT_VGM_DIR is None.
    super::maybe_write_vgm("gamegear_a4.vgm", &vgm_bytes);

    // Create VgmCallbackStream and enable SN76489 state tracking for the Game Gear instance.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    // GameGear PSG is tracked with Sn76489State in the common tracker.
    callback_stream.track_state::<Sn76489State>(Instance::Primary, master_clock);

    // Capture the first observed KeyOn tone frequency (if any)
    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_freq_cb = captured_freq.clone();

    // Register callback for GameGear PSG writes.
    // Capture only the first KeyOn observed.
    callback_stream.on_write(move |_inst, _spec: chip::PsgSpec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { tone, .. } = ev {
                    let mut guard = captured_freq_cb.lock().unwrap();
                    if guard.is_none() {
                        *guard = tone.freq_hz;
                    }
                }
            }
        }
    });

    // Iterate the stream (bounded) to process commands and invoke callbacks.
    for _ in (&mut callback_stream).take(200) {
        // no-op; callback captures the freq
    }

    // Assert we observed a KeyOn and the computed freq_hz is close to target.
    let freq_opt = {
        let guard = captured_freq.lock().unwrap();
        *guard
    };
    assert!(
        freq_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz for Game Gear PSG, but none was captured"
    );
    let freq = freq_opt.expect("ToneInfo.freq_hz should be Some");
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= GG_PSG_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {} Hz, target {} Hz (diff {})",
        freq,
        target_hz,
        diff
    );
}

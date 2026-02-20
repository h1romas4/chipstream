// SN76489 event test.
use std::sync::{Arc, Mutex};

use soundlog::VgmBuilder;
use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Sn76489State;
use soundlog::chip::{self, Chip};
use soundlog::vgm::VgmCallbackStream;
use soundlog::vgm::command::Instance;

const SN76489_TOLERANCE_HZ: f32 = 2.0;

#[test]
fn test_sn76489_keyon_and_tone_freq_matches_a4() {
    // Target pitch
    let target_hz = 440.0_f32;

    // SN76489 master clock (NTSC typical)
    let master_clock = 3_579_545.0_f32;

    // Compute freq_value = round(master / (32 * target))
    let ideal = master_clock / (32.0f32 * target_hz);
    let freq_value = ideal.round() as u16;
    assert!(
        freq_value > 0 && freq_value <= 0x03FF,
        "freq_value out of range"
    );

    // Split into low4 and high6 parts (10-bit value)
    let low4 = (freq_value & 0x0F) as u8;
    let high6 = (freq_value >> 4) as u8; // up to 6 bits

    // Build a VGM document that:
    //  - registers SN76489 in the header
    //  - writes latch byte for frequency low (bit7=1, channel=0, type=0, data=low4)
    //  - writes data byte with high6 (bit7=0, data=high6)
    //  - writes volume latch to set audible volume (channel=0, volume=0 = max)
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Sn76489, Instance::Primary, master_clock as u32);

    // Latch byte for channel 0, frequency (type=0), low 4 bits in low nibble.
    // For channel 0 and type 0 the encoded value is simply 0x80 | data.
    let latch_freq = 0x80u8 | (low4 & 0x0F);
    // Data byte: high 6 bits (bit7 = 0)
    let data_high = high6 & 0x3F;
    // Volume latch and key-off constants.
    let volume_latch: u8 = 0x90;
    let volume_latch_off: u8 = 0x9F;

    // Add PSG writes using the PsgSpec (single-byte writes)
    builder.add_chip_write(Instance::Primary, chip::PsgSpec { value: latch_freq });
    builder.add_chip_write(Instance::Primary, chip::PsgSpec { value: data_high });
    // Key on by setting volume to audible (0). Many PSG flows set volume after freq.
    builder.add_chip_write(
        Instance::Primary,
        chip::PsgSpec {
            value: volume_latch,
        },
    );

    // Let the tone play for 22100 samples (~0.5s at 44.1kHz) so downstream
    // logic can detect duration, then key off by setting volume to silent (15).
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(22100));
    builder.add_chip_write(
        Instance::Primary,
        chip::PsgSpec {
            value: volume_latch_off,
        },
    );

    let doc = builder.finalize();

    // Serialize document to bytes for file output (manual verification).
    let vgm_bytes: Vec<u8> = (&doc).into();

    // Optionally write the VGM artifact using the shared helper from the shim.
    // The helper will no-op when `OUTPUT_VGM_DIR` is `None`.
    super::maybe_write_vgm("sn76489_a4.vgm", &vgm_bytes);

    // Create VgmCallbackStream and enable SN76489 state tracking.
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Sn76489State>(Instance::Primary, master_clock);

    // Capture the first observed KeyOn tone frequency (if any).
    let captured_freq = Arc::new(Mutex::new(None::<f32>));

    // Register callback for PSG writes. Capture only the first KeyOn observed.
    let captured_freq_cb = captured_freq.clone();
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
    for _ in (&mut callback_stream).take(100) {
        // no-op; the callback captures the freq
    }

    // Assert we observed a KeyOn and the computed freq_hz is close to target.
    let freq_opt = {
        let guard = captured_freq.lock().unwrap();
        *guard
    };
    assert!(
        freq_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz, but none was captured"
    );
    let freq = freq_opt.unwrap();
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= SN76489_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {} Hz, target {} Hz (diff {})",
        freq,
        target_hz,
        diff
    );
}

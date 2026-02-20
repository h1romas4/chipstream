// YMF271 (OPX) event test. (NOT WORKING)
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

use soundlog::chip::fnumber::{OpxSpec, find_and_tune_fnumber, generate_12edo_fnum_table};
use soundlog::chip::state::Ymf271State;

const FM_TOLERANCE_HZ: f32 = 2.0;
const WAIT_SAMPLES: u16 = 22100; // ~0.5s @44.1kHz

/// Write a single YMF271 register via the VgmBuilder.
///
/// The YMF271 VGM command is opcode 0xD1: port pp, register aa, value dd.
/// We use port 0 (register bank A/B, FM channels 0–5) for all writes in this test.
#[inline]
fn ymf271_write(builder: &mut VgmBuilder, instance: Instance, register: u8, value: u8) {
    builder.add_chip_write(
        instance,
        chip::Ymf271Spec {
            port: 0,
            register,
            value,
        },
    );
}

/// Select a YMF271 slot for subsequent register writes.
///
/// In soundlog's YMF271 state model a write to any address >= 0x80 sets the
/// currently selected slot.  Slot numbers 0–11 are the 12 FM channels.
#[inline]
fn ymf271_select_slot(builder: &mut VgmBuilder, instance: Instance, slot: u8) {
    ymf271_write(builder, instance, 0x80, slot);
}

/// Configure a YMF271 FM slot to produce an audible tone.
///
/// The YMF271 (OPX) is a 4-operator FM chip.  For a basic test tone we use
/// the simplest 2-operator single-carrier configuration so that the slot
/// actually produces output when keyed on.  Only the registers that affect
/// audibility are programmed here; the rest default to zero (silent / off).
///
/// Relevant per-slot register offsets (after slot selection via reg >= 0x80):
///
/// | Reg | Contents                                           |
/// |-----|-----------------------------------------------------|
/// |   0 | KeyOn (bit 0), external output enables              |
/// |   1 | LFO reset / wave select (bits 7–4)                 |
/// |   2 | Algorithm (bits 2–0), feedback (bits 5–3)          |
/// |   8 | Operator 1 total-level (0 = loudest)               |
/// |   9 | Operator 2 total-level                             |
/// |  10 | Operator 3 total-level                             |
/// |  11 | Operator 4 total-level                             |
/// |  16 | OP1 AR (bits 7–4) / D1R (bits 3–0)                |
/// |  24 | OP2 AR / D1R                                       |
/// |  12 | Block (octave, bits 2–0)                           |
/// |  13 | F-number high nibble (bits 3–0)                    |
/// |  14 | F-number low byte                                  |
pub fn write_ymf271_sine_voice(builder: &mut VgmBuilder, instance: Instance, slot: u8) {
    assert!(slot < 12, "YMF271 has 12 FM slots (0–11)");

    // Select the target slot before writing parameter registers.
    ymf271_select_slot(builder, instance, slot);

    // Algorithm 0 = 4-op chain; for our simple test we want a 2-op sine.
    // Setting the algorithm to 7 (all operators independent / additive) makes
    // every operator contribute to the output.  We then silence OP2–OP4 via
    // total-level so only OP1 is audible.
    //
    // Reg 2: [--FB2-FB1-FB0-ALG2-ALG1-ALG0]  → ALG=7, FB=0 → 0x07
    ymf271_write(builder, instance, 2, 0x07);

    // Total level for OP1 = 0 (maximum volume).
    // Total level for OP2–OP4 = 63 (silent).
    ymf271_write(builder, instance, 8, 0x00); // OP1 TL = 0 (loudest)
    ymf271_write(builder, instance, 9, 0x3F); // OP2 TL = 63 (silent)
    ymf271_write(builder, instance, 10, 0x3F); // OP3 TL = 63 (silent)
    ymf271_write(builder, instance, 11, 0x3F); // OP4 TL = 63 (silent)

    // OP1 envelope: AR=15 (instant attack), D1R=0 (no decay).
    // Reg 16: [AR3-AR2-AR1-AR0-D1R3-D1R2-D1R1-D1R0] → 0xF0
    ymf271_write(builder, instance, 16, 0xF0);
}

/// Write the 3-bit Block and 12-bit F-number to a YMF271 FM slot.
///
/// Register layout (after slot selection):
///
/// | Reg | Bits  | Meaning                              |
/// |-----|-------|--------------------------------------|
/// |  12 | [2:0] | Block (octave 0–7)                   |
/// |  13 | [3:0] | F-number bits 11–8 (high nibble)     |
/// |  14 | [7:0] | F-number bits  7–0 (low byte)        |
pub fn write_ymf271_frequency(
    builder: &mut VgmBuilder,
    instance: Instance,
    slot: u8,
    fnum: u16,
    block: u8,
) {
    assert!(slot < 12, "YMF271 has 12 FM slots (0–11)");
    assert!(fnum <= 0xFFF, "F-number must be 12-bit (0–4095)");
    assert!(block <= 7, "Block must be 3-bit (0–7)");

    ymf271_select_slot(builder, instance, slot);

    // Register 12: block (lower 3 bits).
    ymf271_write(builder, instance, 12, block & 0x07);

    // Register 13: F-number high nibble (bits 11–8).
    ymf271_write(builder, instance, 13, ((fnum >> 8) & 0x0F) as u8);

    // Register 14: F-number low byte (bits 7–0).
    ymf271_write(builder, instance, 14, (fnum & 0xFF) as u8);
}

/// Assert key-on for a YMF271 FM slot.
///
/// Register 0, bit 0 = KeyOn.  Writing 0x01 starts the envelope.
pub fn write_ymf271_keyon(builder: &mut VgmBuilder, instance: Instance, slot: u8) {
    assert!(slot < 12, "YMF271 has 12 FM slots (0–11)");
    ymf271_select_slot(builder, instance, slot);
    ymf271_write(builder, instance, 0, 0x01);
}

/// Assert key-off for a YMF271 FM slot.
///
/// Register 0, bit 0 = KeyOn.  Writing 0x00 releases the envelope.
pub fn write_ymf271_keyoff(builder: &mut VgmBuilder, instance: Instance, slot: u8) {
    assert!(slot < 12, "YMF271 has 12 FM slots (0–11)");
    ymf271_select_slot(builder, instance, slot);
    ymf271_write(builder, instance, 0, 0x00);
}

#[test]
fn test_ymf271_fm_keyon_and_tone_freq_matches_a4() {
    // ---------------------------------------------------------------
    // Target pitch and chip configuration
    // ---------------------------------------------------------------
    let target_hz = 440.0_f32;

    // YMF271 master clock: 16.9344 MHz (÷384 → 44,100 Hz sample rate).
    let master_clock = 16_934_400.0_f32;

    // We'll use FM slot 0.
    let slot: u8 = 0;

    // ---------------------------------------------------------------
    // F-number calculation using OpxSpec (12-bit F-number, 3-bit block)
    // ---------------------------------------------------------------
    let table =
        generate_12edo_fnum_table::<OpxSpec>(master_clock).expect("generate OpxSpec fnum table");
    let tuned = find_and_tune_fnumber::<OpxSpec>(&table, target_hz, master_clock)
        .expect("find and tune fnumber for A4");

    let fnum = tuned.f_num as u16;
    let block = tuned.block;

    // ---------------------------------------------------------------
    // Build VGM
    // ---------------------------------------------------------------
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ymf271, Instance::Primary, master_clock as u32);

    // 1. Configure slot with a simple sine-wave voice.
    write_ymf271_sine_voice(&mut builder, Instance::Primary, slot);

    // 2. Write the frequency registers (block + F-number) for the slot.
    write_ymf271_frequency(&mut builder, Instance::Primary, slot, fnum, block);

    // 3. Key on.
    write_ymf271_keyon(&mut builder, Instance::Primary, slot);

    // 4. Hold the note for ~0.5 s at 44 100 Hz sample rate.
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));

    // 5. Key off.
    write_ymf271_keyoff(&mut builder, Instance::Primary, slot);

    let doc = builder.finalize();

    // ---------------------------------------------------------------
    // Optionally write the VGM artifact for manual verification.
    // ---------------------------------------------------------------
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ymf271_fm_a4.vgm", &vgm_bytes);

    // ---------------------------------------------------------------
    // State-tracking assertion: KeyOn must fire with freq ≈ 440 Hz.
    // ---------------------------------------------------------------
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ymf271State>(Instance::Primary, master_clock);

    let captured_freq_hz = Arc::new(Mutex::new(None::<f32>));
    let captured_freq_hz_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ymf271Spec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { channel: _ch, tone } = ev {
                    let mut guard = captured_freq_hz_cb.lock().unwrap();
                    if guard.is_none() {
                        // Capture only the first KeyOn so the test is deterministic.
                        *guard = tone.freq_hz;
                    }
                }
            }
        }
    });

    // Iterate a bounded number of steps to process all commands.
    for _res in (&mut callback_stream).take(300) {}

    let got_guard = captured_freq_hz.lock().unwrap();
    let got_opt = *got_guard;
    assert!(
        got_opt.is_some(),
        "Expected a KeyOn StateEvent with ToneInfo.freq_hz, but none was captured.\n\
         Check that the slot-select → frequency → key-on write sequence is correct."
    );
    let freq = got_opt.unwrap();
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= FM_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target A4:\n  got  = {freq:.3} Hz\n  want = {target_hz:.3} Hz\n  diff = {diff:.3} Hz (tolerance ±{FM_TOLERANCE_HZ} Hz)"
    );
}

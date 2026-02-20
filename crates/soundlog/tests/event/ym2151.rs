// YM2151 (OPM) event test.
//
// # Voice design (pure sine from M1 only)
//
//   Algorithm 7  – all four operators are output "slots" (summed to output).
//   Feedback  0  – no M1 self-feedback → M1 generates a pure sine wave.
//   M1 TL = 0    – full volume.
//   C1/M2/C2 TL = 127 – fully attenuated → they contribute nothing audible.
//   AR = 31 for all ops – instant attack.
//   D1R = D2R = 0 – no decay while key is held.
//   D1L = 0, RR = 15 – sustain at full level, fast release after key-off.
//   Stereo: both L and R enabled (RL = 0xC0 in register 0x20+ch).
//
// # Common helpers
//
//   write_ym2151_global_init()  – one-time chip reset (LFO off, key-off all).
//   write_ym2151_sine_voice()   – operator + channel registers for a pure sine voice.
//   write_ym2151_frequency()    – write KC and KF registers.
//   write_ym2151_keyon()        – 0x08 with all-operator-on mask.
//   write_ym2151_keyoff()       – 0x08 with zero operator mask.
//
//   All helpers are parameterised on (channel: u8) for any of the eight
//   YM2151 FM channels (0-7).

use soundlog::chip::event::StateEvent;
use soundlog::chip::state::Ym2151State;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

const TARGET_A4_HZ: f32 = 440.0_f32;
/// Allowed absolute Hz tolerance when comparing produced frequency to target.
const YM2151_TOLERANCE_HZ: f32 = 2.0;

#[inline]
fn ym2151_write(builder: &mut VgmBuilder, instance: Instance, register: u8, value: u8) {
    builder.add_chip_write(instance, chip::Ym2151Spec { register, value });
}

/// Emit the one-time global initialisation sequence for a YM2151.
///
/// Writes:
/// - `0x01 = 0x00`  LFO reset / test register
/// - `0x08 = 0x00..0x07`  Key-off all eight channels
/// - `0x19 = 0x00`  AMD (AM depth) = 0
/// - `0x19 = 0x00`  PMD (PM depth) = 0
///
/// Call once before any per-channel configuration.
pub fn write_ym2151_global_init(builder: &mut VgmBuilder, instance: Instance) {
    ym2151_write(builder, instance, 0x01, 0x00); // Test register / LFO reset

    // Key-off all channels (0-7)
    // Register 0x08: bits 6-3 = operator mask (0 = all off), bits 2-0 = channel
    for ch in 0..8u8 {
        ym2151_write(builder, instance, 0x08, ch); // operator mask = 0, channel = ch
    }

    ym2151_write(builder, instance, 0x19, 0x00); // AMD = 0 (AM depth)
    ym2151_write(builder, instance, 0x19, 0x00); // PMD = 0 (PM depth)
}

/// Configure one YM2151 FM channel to produce a **pure sine wave** using only
/// operator M1.
///
/// Voice parameters
/// ----------------
/// | Param      | M1  | C1  | M2  | C2  |
/// |------------|-----|-----|-----|-----|
/// | DT1 / MUL  | 0/1 | 0/1 | 0/1 | 0/1 |
/// | TL         |   0 | 127 | 127 | 127 |
/// | KS / AR    | 0/31| 0/31| 0/31| 0/31|
/// | D1R        |   0 |   0 |   0 |   0 |
/// | DT2 / D2R  | 0/0 | 0/0 | 0/0 | 0/0 |
/// | D1L / RR   | 0/15| 0/15| 0/15| 0/15|
///
/// Channel parameters: Algorithm = 7, Feedback = 0, L = R = 1.
///
/// # Arguments
/// * `channel` – Channel index: 0-7.
pub fn write_ym2151_sine_voice(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 8, "channel must be 0-7");

    // ------------------------------------------------------------------
    // YM2151 operator register layout:
    //
    // YM2151 has 32 operator slots arranged as:
    //   M1: slot 0  (registers base + 0)
    //   C1: slot 8  (registers base + 8)
    //   M2: slot 16 (registers base + 16)
    //   C2: slot 24 (registers base + 24)
    //
    // For each operator register base:
    //   actual_register = base + channel + slot_offset
    //
    // Register bases:
    //   DT1/MUL:  0x40-0x5F
    //   TL:       0x60-0x7F
    //   KS/AR:    0x80-0x9F
    //   AMS/D1R:  0xA0-0xBF
    //   DT2/D2R:  0xC0-0xDF
    //   D1L/RR:   0xE0-0xFF
    // ------------------------------------------------------------------

    // Operator offsets for M1, C1, M2, C2
    let op_m1 = channel; // slot 0
    let op_c1 = channel + 8; // slot 8
    let op_m2 = channel + 16; // slot 16
    let op_c2 = channel + 24; // slot 24

    // --- DT1/MUL  [DT1[2:0]-0-MUL[3:0]] ---------------------------------
    // DT1=0 (no detune), MUL=1 (×1 frequency).  Encoded value = 0x01.
    ym2151_write(builder, instance, 0x40 + op_m1, 0x01); // M1
    ym2151_write(builder, instance, 0x40 + op_c1, 0x01); // C1
    ym2151_write(builder, instance, 0x40 + op_m2, 0x01); // M2
    ym2151_write(builder, instance, 0x40 + op_c2, 0x01); // C2

    // --- TL  [0-TL[6:0]] ------------------------------------------------
    // M1 at full volume (TL=0); C1/M2/C2 fully attenuated (TL=127 = 0x7F).
    ym2151_write(builder, instance, 0x60 + op_m1, 0x00); // M1 – loudest
    ym2151_write(builder, instance, 0x60 + op_c1, 0x7F); // C1 – muted
    ym2151_write(builder, instance, 0x60 + op_m2, 0x7F); // M2 – muted
    ym2151_write(builder, instance, 0x60 + op_c2, 0x7F); // C2 – muted

    // --- KS/AR  [KS[1:0]-0-AR[4:0]] -------------------------------------
    // KS=0, AR=31 (0x1F) → instant attack for all operators.
    ym2151_write(builder, instance, 0x80 + op_m1, 0x1F); // M1
    ym2151_write(builder, instance, 0x80 + op_c1, 0x1F); // C1
    ym2151_write(builder, instance, 0x80 + op_m2, 0x1F); // M2
    ym2151_write(builder, instance, 0x80 + op_c2, 0x1F); // C2

    // --- AMS-EN/D1R  [AMS-EN-0-D1R[4:0]] --------------------------------
    // AMS-EN=0, D1R=0 → no first-decay while key is held.
    ym2151_write(builder, instance, 0xA0 + op_m1, 0x00); // M1
    ym2151_write(builder, instance, 0xA0 + op_c1, 0x00); // C1
    ym2151_write(builder, instance, 0xA0 + op_m2, 0x00); // M2
    ym2151_write(builder, instance, 0xA0 + op_c2, 0x00); // C2

    // --- DT2/D2R  [DT2[1:0]-0-D2R[4:0]] ---------------------------------
    // DT2=0, D2R=0 → no secondary decay.
    ym2151_write(builder, instance, 0xC0 + op_m1, 0x00); // M1
    ym2151_write(builder, instance, 0xC0 + op_c1, 0x00); // C1
    ym2151_write(builder, instance, 0xC0 + op_m2, 0x00); // M2
    ym2151_write(builder, instance, 0xC0 + op_c2, 0x00); // C2

    // --- D1L/RR  [D1L[3:0]-RR[3:0]] -------------------------------------
    // D1L=0 (sustain at full level), RR=15 (fast release after key-off).
    // Encoded: (0 << 4) | 15 = 0x0F.
    ym2151_write(builder, instance, 0xE0 + op_m1, 0x0F); // M1
    ym2151_write(builder, instance, 0xE0 + op_c1, 0x0F); // C1
    ym2151_write(builder, instance, 0xE0 + op_m2, 0x0F); // M2
    ym2151_write(builder, instance, 0xE0 + op_c2, 0x0F); // C2

    // 0x20+ch  [RL[1:0]-FL[2:0]-CON[2:0]]
    // RL = 0b11 (both L and R), FL (Feedback) = 0, CON (Connection/Algorithm) = 7
    // Encoded: 0b11_000_111 = 0xC7
    ym2151_write(builder, instance, 0x20 + channel, 0xC7);
}

/// Write the KC (Key Code) and KF (Key Fraction) to a YM2151 FM channel.
///
/// YM2151 frequency is specified by:
/// - KC (0x28+ch): bits 6-4 = octave (block), bits 3-0 = note code
/// - KF (0x30+ch): bits 7-2 = key fraction (6 bits)
///
/// The effective F-number is: fnum = (note_code * 64) + kf_fraction
///
/// # Arguments
/// * `channel`  – 0-7.
/// * `kc_value` – Value to write to 0x28+channel (octave and note code).
/// * `kf_value` – Value to write to 0x30+channel (key fraction, upper 6 bits).
pub fn write_ym2151_frequency(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    kc_value: u8,
    kf_value: u8,
) {
    assert!(channel < 8, "channel must be 0-7");
    ym2151_write(builder, instance, 0x28 + channel, kc_value);
    ym2151_write(builder, instance, 0x30 + channel, kf_value);
}

/// Emit a key-on command that activates all four operators on one channel.
///
/// Register 0x08 encoding:
/// ```
/// bits 6-3: operator slots to key-on
///   bit 6: M1
///   bit 5: C1
///   bit 4: M2
///   bit 3: C2
///   0x78 = 0b0111_1000 = all four operators on
/// bits 2-0: channel selector (0-7)
/// ```
pub fn write_ym2151_keyon(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 8, "channel must be 0-7");
    // All four operator slots on (bits 6-3 = 0b1111 = 0x78).
    ym2151_write(builder, instance, 0x08, 0x78 | channel);
}

/// Emit a key-off command that silences all four operators on one channel.
///
/// Same encoding as [`write_ym2151_keyon`] but with operator mask = 0.
pub fn write_ym2151_keyoff(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 8, "channel must be 0-7");
    // Operator mask = 0 → key off (bits 6-3 = 0).
    ym2151_write(builder, instance, 0x08, channel);
}

#[test]
fn test_ym2151_keyon_and_tone_freq_matches_a4() {
    // ---------------------------------------------------------------
    // Target pitch and chip configuration
    // ---------------------------------------------------------------
    let target_hz = TARGET_A4_HZ;

    // YM2151 master clock for arcade systems (NTSC colorburst frequency)
    let master_clock = 3_579_545.0_f32;

    // We'll use channel 0 (the first FM channel).
    let channel: u8 = 0;

    // YM2151 frequency encoding:
    // KC register: bits 6-4 = oct, bits 3-0 = note_code
    // KF register: bits 7-2 = kf_fraction (6 bits)
    let kc_value = 0x4a;
    let kf_value = 0x00;

    // ---------------------------------------------------------------
    // Build VGM
    // ---------------------------------------------------------------
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2151, Instance::Primary, master_clock as u32);

    // 1. One-time global initialisation (LFO off, all channels key-off)
    write_ym2151_global_init(&mut builder, Instance::Primary);

    // 2. Initialise channel 0 with a pure-sine voice (algorithm 7, feedback 0,
    //    M1 full volume, C1/M2/C2 muted, instant attack, no decay, fast release).
    write_ym2151_sine_voice(&mut builder, Instance::Primary, channel);

    // 3. Write the frequency (KC and KF registers).
    write_ym2151_frequency(&mut builder, Instance::Primary, channel, kc_value, kf_value);

    // 4. Key on – all four operators.
    write_ym2151_keyon(&mut builder, Instance::Primary, channel);

    // 5. Hold note for ~0.5 s at 44 100 Hz sample rate.
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(22100));

    // 6. Key off.
    write_ym2151_keyoff(&mut builder, Instance::Primary, channel);

    let doc = builder.finalize();

    // ---------------------------------------------------------------
    // Optionally write the VGM artefact for manual verification
    // ---------------------------------------------------------------
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ym2151_a4.vgm", &vgm_bytes);

    // ---------------------------------------------------------------
    // State-tracking assertion: KeyOn must fire with freq ≈ 440 Hz
    // ---------------------------------------------------------------
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ym2151State>(Instance::Primary, master_clock);

    let captured_freq_hz = std::sync::Arc::new(std::sync::Mutex::new(None::<f32>));
    let captured_freq_hz_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ym2151Spec, _sample, event_opt| {
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

    // Iterate bounded to process all commands and trigger callbacks.
    for _res in (&mut callback_stream).take(200) {}

    let got_guard = captured_freq_hz.lock().unwrap();
    let got_opt = *got_guard;
    assert!(
        got_opt.is_some(),
        "Expected KeyOn StateEvent with ToneInfo.freq_hz, but none was captured"
    );
    let freq = got_opt.unwrap();
    // Use the tuned.actual_freq_hz as expected frequency (closest representable)
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= YM2151_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from expected: got {freq} Hz, expected {target_hz} Hz (diff {diff})"
    );
}

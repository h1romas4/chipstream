// YM2203 event test.
use std::sync::{Arc, Mutex};

use soundlog::chip::event::StateEvent;
use soundlog::chip::{self, Chip};
use soundlog::vgm::command::Instance;
use soundlog::{VgmBuilder, VgmCallbackStream};

use soundlog::chip::fnumber::{OpnSpec, find_and_tune_fnumber, generate_12edo_fnum_table};
use soundlog::chip::state::Ym2203State;

const TARGET_A4_HZ: f32 = 440.0_f32;
const PSG_TOLERANCE_HZ: f32 = 2.0;
const FM_TOLERANCE_HZ: f32 = 2.0;
const WAIT_SAMPLES: u16 = 22100; // ~0.5s @44.1kHz

/// Helper: emit a single YM2203 register write
#[inline]
fn ym2203_write(builder: &mut VgmBuilder, instance: Instance, register: u8, value: u8) {
    builder.add_chip_write(instance, chip::Ym2203Spec { register, value });
}

/// Configure a YM2203 FM channel to produce a pure sine wave using only Operator 1.
///
/// Voice parameters:
/// | Param      | OP1 | OP2 | OP3 | OP4 |
/// |------------|-----|-----|-----|-----|
/// | DT1 / MUL  | 0/1 | 0/1 | 0/1 | 0/1 |
/// | TL         |   0 | 127 | 127 | 127 |
/// | AR         |  31 |  31 |  31 |  31 |
/// | D1R        |   0 |   0 |   0 |   0 |
/// | D2R        |   0 |   0 |   0 |   0 |
/// | D1L / RR   | 0/15| 0/15| 0/15| 0/15|
/// | SSG-EG     |   0 |   0 |   0 |   0 |
///
/// Channel parameters: Algorithm = 7, Feedback = 0, L = R = 1.
///
/// # Arguments
/// * `channel` - FM channel index (0-2)
pub fn write_ym2203_sine_voice(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 3, "YM2203 has 3 FM channels (0-2)");

    // Operator register layout (all four operators, per channel):
    //   base + op_offset + channel
    //     op_offset : op1=0x00, op2=0x04, op3=0x08, op4=0x0C

    // --- DT1/MUL  [--DT1[2:0]-MUL[3:0]] ---------------------------------
    // DT1=0 (no detune), MUL=1 (×1 frequency).  Encoded value = 0x01.
    ym2203_write(builder, instance, 0x30 + channel, 0x01); // op1
    ym2203_write(builder, instance, 0x34 + channel, 0x01); // op2
    ym2203_write(builder, instance, 0x38 + channel, 0x01); // op3
    ym2203_write(builder, instance, 0x3C + channel, 0x01); // op4

    // --- TL  [---TL[6:0]] ------------------------------------------------
    // OP1 at full volume (TL=0); OP2/3/4 fully attenuated (TL=127 = 0x7F).
    ym2203_write(builder, instance, 0x40 + channel, 0x00); // op1 – loudest
    ym2203_write(builder, instance, 0x44 + channel, 0x7F); // op2 – muted
    ym2203_write(builder, instance, 0x48 + channel, 0x7F); // op3 – muted
    ym2203_write(builder, instance, 0x4C + channel, 0x7F); // op4 – muted

    // --- RS/AR  [RS[1:0]-0-AR[4:0]] --------------------------------------
    // RS=0, AR=31 (0x1F) → instant attack for all operators.
    ym2203_write(builder, instance, 0x50 + channel, 0x1F); // op1
    ym2203_write(builder, instance, 0x54 + channel, 0x1F); // op2
    ym2203_write(builder, instance, 0x58 + channel, 0x1F); // op3
    ym2203_write(builder, instance, 0x5C + channel, 0x1F); // op4

    // --- AM/D1R  [AM-0-D1R[4:0]] -----------------------------------------
    // AM=0, D1R=0 → no first-decay while key is held.
    ym2203_write(builder, instance, 0x60 + channel, 0x00); // op1
    ym2203_write(builder, instance, 0x64 + channel, 0x00); // op2
    ym2203_write(builder, instance, 0x68 + channel, 0x00); // op3
    ym2203_write(builder, instance, 0x6C + channel, 0x00); // op4

    // --- D2R  [---D2R[4:0]] -----------------------------------------------
    // D2R=0 → no secondary decay.
    ym2203_write(builder, instance, 0x70 + channel, 0x00); // op1
    ym2203_write(builder, instance, 0x74 + channel, 0x00); // op2
    ym2203_write(builder, instance, 0x78 + channel, 0x00); // op3
    ym2203_write(builder, instance, 0x7C + channel, 0x00); // op4

    // --- D1L/RR  [D1L[3:0]-RR[3:0]] --------------------------------------
    // D1L=0 (sustain at full level), RR=15 (fast release after key-off).
    // Encoded: (0 << 4) | 15 = 0x0F.
    ym2203_write(builder, instance, 0x80 + channel, 0x0F); // op1
    ym2203_write(builder, instance, 0x84 + channel, 0x0F); // op2
    ym2203_write(builder, instance, 0x88 + channel, 0x0F); // op3
    ym2203_write(builder, instance, 0x8C + channel, 0x0F); // op4

    // --- SSG-EG  [---SSG-EG[3:0]] ----------------------------------------
    // Proprietary; always 0.
    ym2203_write(builder, instance, 0x90 + channel, 0x00); // op1
    ym2203_write(builder, instance, 0x94 + channel, 0x00); // op2
    ym2203_write(builder, instance, 0x98 + channel, 0x00); // op3
    ym2203_write(builder, instance, 0x9C + channel, 0x00); // op4

    // ------------------------------------------------------------------
    // Per-channel registers (B0H+, B4H+)
    // ------------------------------------------------------------------

    // B0H+  [--FB[2:0]-ALG[2:0]]
    // Feedback=0 (no OP1 self-feedback → pure sine), Algorithm=7 (all ops are
    // output slots). Encoded: 0b000_000_111 = 0x07.
    ym2203_write(builder, instance, 0xB0 + channel, 0x07);

    // B4H+  [L-R-AMS[1:0]-0-FMS[2:0]]
    // L=1, R=1 (both speakers), AMS=0, FMS=0.  Encoded: 0b11_00_0_000 = 0xC0.
    ym2203_write(builder, instance, 0xB4 + channel, 0xC0);
}

/// Write the 11-bit F-number and Block to a YM2203 FM channel.
///
/// Per the YM2203 manual, the high byte (A4H+) **must** be written before
/// the low byte (A0H+).
///
/// # Arguments
/// * `channel`   – 0, 1, or 2.
/// * `a4_value`  – `Block[2:0]` in bits 5-3 and `F-num[10:8]` in bits 2-0.
/// * `fnum_low`  – `F-num[7:0]` (low byte).
pub fn write_ym2203_frequency(
    builder: &mut VgmBuilder,
    instance: Instance,
    channel: u8,
    a4_value: u8,
    fnum_low: u8,
) {
    assert!(channel < 3, "YM2203 has 3 FM channels (0-2)");
    // High byte first, then low byte (as required by the manual).
    ym2203_write(builder, instance, 0xA4 + channel, a4_value);
    ym2203_write(builder, instance, 0xA0 + channel, fnum_low);
}

/// Emit a key-on command that activates all four operators on one channel.
///
/// Register 0x28 encoding:
/// ```
/// bits 7-4: operator slots to key-on (0xF = all four)
/// bits 1-0: channel selector (0-2)
/// ```
pub fn write_ym2203_keyon(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 3, "YM2203 has 3 FM channels (0-2)");
    // All four operator slots on (bits 7-4 = 0xF).
    ym2203_write(builder, instance, 0x28, 0xF0 | channel);
}

/// Emit a key-off command that silences all four operators on one channel.
///
/// Same encoding as [`write_ym2203_keyon`] but with operator mask = 0.
pub fn write_ym2203_keyoff(builder: &mut VgmBuilder, instance: Instance, channel: u8) {
    assert!(channel < 3, "YM2203 has 3 FM channels (0-2)");
    // Operator mask = 0 → key off.
    ym2203_write(builder, instance, 0x28, channel);
}

#[test]
fn test_ym2203_fm_keyon_and_tone_freq_matches_a4() {
    // ---------------------------------------------------------------
    // Target pitch and chip configuration
    // ---------------------------------------------------------------
    let target_hz = 440.0_f32;

    // YM2203 typical master clock
    let master_clock = 4_000_000.0_f32;

    // We'll use FM channel 0
    let channel: u8 = 0;

    // ---------------------------------------------------------------
    // F-number calculation using OpnSpec (prescaler = 2.0 for YM2203)
    // ---------------------------------------------------------------
    let table = generate_12edo_fnum_table::<OpnSpec>(master_clock).expect("generate fnum table");
    let tuned = find_and_tune_fnumber::<OpnSpec>(&table, target_hz, master_clock)
        .expect("find and tune fnumber");

    let fnum_u32 = tuned.f_num;
    let block_u8 = tuned.block;

    // YM2203 F-number is 11 bits: low 8 → A0H, high 3 → A4H bits 2-0.
    // Block occupies A4H bits 5-3.
    let fnum_low = (fnum_u32 & 0xFF) as u8;
    let fnum_high = ((fnum_u32 >> 8) & 0x07) as u8;
    let a4_value = ((block_u8 & 0x07) << 3) | (fnum_high & 0x07);

    // ---------------------------------------------------------------
    // Build VGM
    // ---------------------------------------------------------------
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2203, Instance::Primary, master_clock as u32);

    // 1. Initialize channel with a pure-sine voice
    write_ym2203_sine_voice(&mut builder, Instance::Primary, channel);

    // 2. Write the frequency (A4H high byte first, then A0H low byte)
    write_ym2203_frequency(&mut builder, Instance::Primary, channel, a4_value, fnum_low);

    // 3. Key on – all four operators
    write_ym2203_keyon(&mut builder, Instance::Primary, channel);

    // 4. Hold note for ~0.5 s at 44 100 Hz sample rate
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));

    // 5. Key off
    write_ym2203_keyoff(&mut builder, Instance::Primary, channel);

    let doc = builder.finalize();

    // ---------------------------------------------------------------
    // Optionally write the VGM artifact for manual verification
    // ---------------------------------------------------------------
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ym2203_fm_a4.vgm", &vgm_bytes);

    // ---------------------------------------------------------------
    // State-tracking assertion: KeyOn must fire with freq ≈ 440 Hz
    // ---------------------------------------------------------------
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ym2203State>(Instance::Primary, master_clock);

    let captured_freq_hz = Arc::new(Mutex::new(None::<f32>));
    let captured_freq_hz_cb = captured_freq_hz.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ym2203Spec, _sample, event_opt| {
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
    let diff = (freq - target_hz).abs();
    assert!(
        diff <= FM_TOLERANCE_HZ,
        "ToneInfo.freq_hz differs from target: got {freq} Hz, target {target_hz} Hz (diff {diff})"
    );
}

#[test]
fn test_ym2203_psg_channel_a_matches_a4() {
    // YM2203 typical master clock (use 4_000_000 as documented)
    let master_clock = 4_000_000.0_f32;

    // PSG period formula: the YM2203 SSG section uses master_clock / 2 as its effective
    // clock before the AY-compatible tone counters.
    // period = round(master_clock / 2 / (16 * freq)) = round(master_clock / (32 * freq))
    let ideal_period = (master_clock / (2.0_f32 * 16.0_f32 * TARGET_A4_HZ)).round() as u16;
    assert!(
        ideal_period > 0 && ideal_period <= 0x0FFF,
        "period out of range"
    );

    let fine = (ideal_period & 0xFF) as u8;
    let coarse = ((ideal_period >> 8) & 0x0F) as u8;

    // Build VGM: register YM2203, write PSG period regs (0x00/0x01 channel 0),
    // enable mixer for channel A (clear bit 0), wait then disable mixer bit.
    let mut builder = VgmBuilder::new();
    builder.register_chip(Chip::Ym2203, Instance::Primary, master_clock as u32);

    // PSG tone period registers for channel 0: 0x00 (fine), 0x01 (coarse low nibble)
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2203Spec {
            register: 0x00,
            value: fine,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2203Spec {
            register: 0x01,
            value: coarse & 0x0F,
        },
    );

    // Channel A volume: bits 3-0 = level (0=silent, 0xF=max), bit4=0 (fixed-level mode).
    // Must be written before enabling the mixer, otherwise the PSG channel is silent.
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2203Spec {
            register: 0x08,
            value: 0x0F, // max volume, fixed-level mode
        },
    );

    // Mixer: bits 0-2 tone enable (0=enabled, 1=disabled). Enable channel A only.
    // 0b1111_1110 disables all tone channels except A (enables A).
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2203Spec {
            register: 0x07,
            value: 0b1111_1110u8,
        },
    );

    // Wait then key off (disable channel A and silence volume)
    builder.add_vgm_command(soundlog::vgm::command::WaitSamples(WAIT_SAMPLES));
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2203Spec {
            register: 0x07,
            value: 0b1111_1111u8,
        },
    );
    builder.add_chip_write(
        Instance::Primary,
        chip::Ym2203Spec {
            register: 0x08,
            value: 0x00, // silence channel A volume
        },
    );

    let doc = builder.finalize();
    let vgm_bytes: Vec<u8> = (&doc).into();
    super::maybe_write_vgm("ym2203_psg_a4.vgm", &vgm_bytes);

    // Create callback stream and enable YM2203 PSG state tracking
    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_state::<Ym2203State>(Instance::Primary, master_clock);

    let captured_freq = Arc::new(Mutex::new(None::<f32>));
    let captured_cb = captured_freq.clone();

    callback_stream.on_write(move |_inst, _spec: chip::Ym2203Spec, _sample, event_opt| {
        if let Some(events) = event_opt {
            for ev in events {
                if let StateEvent::KeyOn { tone, .. } = ev {
                    let mut g = captured_cb.lock().unwrap();
                    *g = tone.freq_hz;
                }
            }
        }
    });

    for _ in (&mut callback_stream).take(200) {}

    let got = captured_freq.lock().unwrap();
    assert!(
        got.is_some(),
        "Expected KeyOn StateEvent for YM2203 PSG but none captured"
    );
    let freq = got.unwrap();
    // Expected frequency: master_clock / 2 / (16 * ideal_period)
    let expected_freq = master_clock / (2.0_f32 * 16.0_f32 * ideal_period as f32);
    let diff = (freq - expected_freq).abs();
    assert!(
        diff <= PSG_TOLERANCE_HZ,
        "YM2203 PSG freq differs: got {} Hz, expected {} Hz (diff {})",
        freq,
        expected_freq,
        diff
    );
}

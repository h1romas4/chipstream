use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use soundlog::VgmBuilder;
use soundlog::VgmCallbackStream;
use soundlog::vgm::command::{Instance, VgmCommand};

/// Construct a VGM command list that covers the wide set of chip-write
/// variants exercised by the parser / callback stream. This mirrors the
/// exhaustive cases used elsewhere in the test-suite.
#[allow(clippy::vec_init_then_push)]
fn build_all_chip_write_cases() -> Vec<VgmCommand> {
    use soundlog::chip;

    let mut cases: Vec<VgmCommand> = Vec::new();

    // Common register/value style chips (primary instance)
    cases.push(VgmCommand::Sn76489Write(
        Instance::Primary,
        chip::PsgSpec { value: 0x11 },
    ));
    cases.push(VgmCommand::Ym2413Write(
        Instance::Primary,
        chip::Ym2413Spec {
            register: 0x10,
            value: 0x22,
        },
    ));
    cases.push(VgmCommand::Ym2612Write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x2A,
            value: 0x33,
        },
    ));
    cases.push(VgmCommand::Ym2151Write(
        Instance::Primary,
        chip::Ym2151Spec {
            register: 0x01,
            value: 0x55,
        },
    ));
    cases.push(VgmCommand::Ym2203Write(
        Instance::Primary,
        chip::Ym2203Spec {
            register: 0x02,
            value: 0x66,
        },
    ));
    cases.push(VgmCommand::Ym2608Write(
        Instance::Primary,
        chip::Ym2608Spec {
            port: 0,
            register: 0x03,
            value: 0x77,
        },
    ));
    cases.push(VgmCommand::Ym2610bWrite(
        Instance::Primary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x04,
            value: 0x88,
        },
    ));
    cases.push(VgmCommand::Ym3812Write(
        Instance::Primary,
        chip::Ym3812Spec {
            register: 0x05,
            value: 0x99,
        },
    ));
    cases.push(VgmCommand::Ym3526Write(
        Instance::Primary,
        chip::Ym3526Spec {
            register: 0x06,
            value: 0xA0,
        },
    ));
    cases.push(VgmCommand::Y8950Write(
        Instance::Primary,
        chip::Y8950Spec {
            register: 0x07,
            value: 0xB1,
        },
    ));
    cases.push(VgmCommand::Ymz280bWrite(
        Instance::Primary,
        chip::Ymz280bSpec {
            register: 0x08,
            value: 0xC2,
        },
    ));
    cases.push(VgmCommand::Ymf262Write(
        Instance::Primary,
        chip::Ymf262Spec {
            port: 0,
            register: 0x09,
            value: 0xD3,
        },
    ));
    // AY8910 primary/secondary
    cases.push(VgmCommand::Ay8910Write(
        Instance::Primary,
        chip::Ay8910Spec {
            register: 0x05,
            value: 0x12,
        },
    ));
    // AY secondary
    cases.push(VgmCommand::Ay8910Write(
        Instance::Secondary,
        chip::Ay8910Spec {
            register: 0x07,
            value: 0x34,
        },
    ));

    // Mikey
    cases.push(VgmCommand::MikeyWrite(
        Instance::Primary,
        chip::MikeySpec {
            register: 0x10,
            value: 0x77,
        },
    ));

    // Game Gear PSG (treated as SN76489)
    cases.push(VgmCommand::GameGearPsgWrite(
        Instance::Primary,
        chip::GameGearPsgSpec { value: 0x99 },
    ));

    // RF5C68 / RF5C164 (u8/u16 variants)
    cases.push(VgmCommand::Rf5c68U8Write(
        Instance::Primary,
        chip::Rf5c68U8Spec {
            offset: 0x10,
            value: 0x21,
        },
    ));
    cases.push(VgmCommand::Rf5c68U16Write(
        Instance::Primary,
        chip::Rf5c68U16Spec {
            offset: 0x1234,
            value: 0x22,
        },
    ));
    cases.push(VgmCommand::Rf5c164U8Write(
        Instance::Primary,
        chip::Rf5c164U8Spec {
            offset: 0x05,
            value: 0x23,
        },
    ));
    cases.push(VgmCommand::Rf5c164U16Write(
        Instance::Primary,
        chip::Rf5c164U16Spec {
            offset: 0x4321,
            value: 0x24,
        },
    ));

    // PWM
    cases.push(VgmCommand::PwmWrite(
        Instance::Primary,
        chip::PwmSpec {
            register: 0x01,
            value: 0x0FFFu32,
        },
    ));

    // GameBoy DMG, NES APU, MultiPCM, UPD7759
    cases.push(VgmCommand::GbDmgWrite(
        Instance::Primary,
        chip::GbDmgSpec {
            register: 0x0A,
            value: 0x42,
        },
    ));
    cases.push(VgmCommand::NesApuWrite(
        Instance::Primary,
        chip::NesApuSpec {
            register: 0x0B,
            value: 0x43,
        },
    ));
    cases.push(VgmCommand::MultiPcmWrite(
        Instance::Primary,
        chip::MultiPcmSpec {
            register: 0x0C,
            value: 0x44,
        },
    ));
    cases.push(VgmCommand::Upd7759Write(
        Instance::Primary,
        chip::Upd7759Spec {
            register: 0x0D,
            value: 0x45,
        },
    ));

    // OKIM / HUC / K053260 / Pokey / WonderSwanReg / SAA1099 / ES5506 / GA20
    cases.push(VgmCommand::Okim6258Write(
        Instance::Primary,
        chip::Okim6258Spec {
            register: 0x0E,
            value: 0x46,
        },
    ));
    cases.push(VgmCommand::Okim6295Write(
        Instance::Primary,
        chip::Okim6295Spec {
            register: 0x0F,
            value: 0x47,
        },
    ));
    cases.push(VgmCommand::Huc6280Write(
        Instance::Primary,
        chip::Huc6280Spec {
            register: 0x11,
            value: 0x48,
        },
    ));
    cases.push(VgmCommand::K053260Write(
        Instance::Primary,
        chip::K053260Spec {
            register: 0x12,
            value: 0x49,
        },
    ));
    cases.push(VgmCommand::PokeyWrite(
        Instance::Primary,
        chip::PokeySpec {
            register: 0x13,
            value: 0x4A,
        },
    ));
    cases.push(VgmCommand::WonderSwanRegWrite(
        Instance::Primary,
        chip::WonderSwanRegSpec {
            register: 0x14,
            value: 0x4B,
        },
    ));
    cases.push(VgmCommand::Saa1099Write(
        Instance::Primary,
        chip::Saa1099Spec {
            register: 0x15,
            value: 0x4C,
        },
    ));
    cases.push(VgmCommand::Es5506BEWrite(
        Instance::Primary,
        chip::Es5506U8Spec {
            register: 0x16,
            value: 0x4D,
        },
    ));
    cases.push(VgmCommand::Ga20Write(
        Instance::Primary,
        chip::Ga20Spec {
            register: 0x17,
            value: 0x4E,
        },
    ));

    // MultiPcmBank, QSound, SCSP, WonderSwan, VSU, X1010
    cases.push(VgmCommand::MultiPcmBankWrite(
        Instance::Primary,
        chip::MultiPcmBankSpec {
            channel: 0x01,
            bank_offset: 0x0200,
        },
    ));
    cases.push(VgmCommand::QsoundWrite(
        Instance::Primary,
        chip::QsoundSpec {
            register: 0x01,
            value: 0x50,
        },
    ));
    cases.push(VgmCommand::ScspWrite(
        Instance::Primary,
        chip::ScspSpec {
            offset: 0x20,
            value: 0x51,
        },
    ));
    cases.push(VgmCommand::WonderSwanWrite(
        Instance::Primary,
        chip::WonderSwanSpec {
            offset: 0x1234,
            value: 0x52,
        },
    ));
    cases.push(VgmCommand::VsuWrite(
        Instance::Primary,
        chip::VsuSpec {
            offset: 0x21,
            value: 0x53,
        },
    ));
    cases.push(VgmCommand::X1010Write(
        Instance::Primary,
        chip::X1010Spec {
            offset: 0x22,
            value: 0x54,
        },
    ));

    // YMF family & related
    cases.push(VgmCommand::Ymf278bWrite(
        Instance::Primary,
        chip::Ymf278bSpec {
            port: 0,
            register: 0x30,
            value: 0x60,
        },
    ));
    cases.push(VgmCommand::Ymf271Write(
        Instance::Primary,
        chip::Ymf271Spec {
            port: 0,
            register: 0x31,
            value: 0x61,
        },
    ));
    cases.push(VgmCommand::Scc1Write(
        Instance::Primary,
        chip::Scc1Spec {
            port: 0,
            register: 0x32,
            value: 0x62,
        },
    ));
    cases.push(VgmCommand::K054539Write(
        Instance::Primary,
        chip::K054539Spec {
            register: 0x33,
            value: 0x63,
        },
    ));
    cases.push(VgmCommand::C140Write(
        Instance::Primary,
        chip::C140Spec {
            register: 0x34,
            value: 0x64,
        },
    ));
    cases.push(VgmCommand::Es5503Write(
        Instance::Primary,
        chip::Es5503Spec {
            register: 0x35,
            value: 0x65,
        },
    ));
    cases.push(VgmCommand::Es5506D6Write(
        Instance::Primary,
        chip::Es5506U16Spec {
            register: 0x36,
            value: 0x1234,
        },
    ));
    cases.push(VgmCommand::C352Write(
        Instance::Primary,
        chip::C352Spec {
            register: 0x40,
            value: 0x66,
        },
    ));

    // Secondary-instance variants for dual-instance-capable chips
    cases.push(VgmCommand::Sn76489Write(
        Instance::Secondary,
        chip::PsgSpec { value: 0x12 },
    ));
    cases.push(VgmCommand::Ym2413Write(
        Instance::Secondary,
        chip::Ym2413Spec {
            register: 0x10,
            value: 0x23,
        },
    ));
    cases.push(VgmCommand::Ym2151Write(
        Instance::Secondary,
        chip::Ym2151Spec {
            register: 0x01,
            value: 0x34,
        },
    ));
    cases.push(VgmCommand::Ym2203Write(
        Instance::Secondary,
        chip::Ym2203Spec {
            register: 0x02,
            value: 0x45,
        },
    ));
    cases.push(VgmCommand::Ym2608Write(
        Instance::Secondary,
        chip::Ym2608Spec {
            port: 0,
            register: 0x03,
            value: 0x56,
        },
    ));
    cases.push(VgmCommand::Ym2610bWrite(
        Instance::Secondary,
        chip::Ym2610Spec {
            port: 0,
            register: 0x04,
            value: 0x67,
        },
    ));
    cases.push(VgmCommand::Ym3812Write(
        Instance::Secondary,
        chip::Ym3812Spec {
            register: 0x05,
            value: 0x78,
        },
    ));
    cases.push(VgmCommand::Ym3526Write(
        Instance::Secondary,
        chip::Ym3526Spec {
            register: 0x06,
            value: 0x79,
        },
    ));
    cases.push(VgmCommand::Y8950Write(
        Instance::Secondary,
        chip::Y8950Spec {
            register: 0x07,
            value: 0x7A,
        },
    ));
    cases.push(VgmCommand::Ymz280bWrite(
        Instance::Secondary,
        chip::Ymz280bSpec {
            register: 0x08,
            value: 0x7B,
        },
    ));
    cases.push(VgmCommand::Ymf262Write(
        Instance::Secondary,
        chip::Ymf262Spec {
            port: 0,
            register: 0x09,
            value: 0x7C,
        },
    ));
    cases.push(VgmCommand::GbDmgWrite(
        Instance::Secondary,
        chip::GbDmgSpec {
            register: 0x0A,
            value: 0x11,
        },
    ));
    cases.push(VgmCommand::NesApuWrite(
        Instance::Secondary,
        chip::NesApuSpec {
            register: 0x0B,
            value: 0x22,
        },
    ));
    cases.push(VgmCommand::MultiPcmWrite(
        Instance::Secondary,
        chip::MultiPcmSpec {
            register: 0x0C,
            value: 0x33,
        },
    ));
    cases.push(VgmCommand::Upd7759Write(
        Instance::Secondary,
        chip::Upd7759Spec {
            register: 0x0D,
            value: 0x44,
        },
    ));
    cases.push(VgmCommand::Okim6258Write(
        Instance::Secondary,
        chip::Okim6258Spec {
            register: 0x0E,
            value: 0x55,
        },
    ));
    cases.push(VgmCommand::Okim6295Write(
        Instance::Secondary,
        chip::Okim6295Spec {
            register: 0x0F,
            value: 0x66,
        },
    ));
    cases.push(VgmCommand::Huc6280Write(
        Instance::Secondary,
        chip::Huc6280Spec {
            register: 0x11,
            value: 0x77,
        },
    ));
    cases.push(VgmCommand::K053260Write(
        Instance::Secondary,
        chip::K053260Spec {
            register: 0x12,
            value: 0x11,
        },
    ));
    cases.push(VgmCommand::PokeyWrite(
        Instance::Secondary,
        chip::PokeySpec {
            register: 0x13,
            value: 0x22,
        },
    ));
    cases.push(VgmCommand::WonderSwanRegWrite(
        Instance::Secondary,
        chip::WonderSwanRegSpec {
            register: 0x14,
            value: 0x33,
        },
    ));
    cases.push(VgmCommand::Saa1099Write(
        Instance::Secondary,
        chip::Saa1099Spec {
            register: 0x15,
            value: 0x44,
        },
    ));
    cases.push(VgmCommand::Es5506BEWrite(
        Instance::Secondary,
        chip::Es5506U8Spec {
            register: 0x16,
            value: 0x55,
        },
    ));
    cases.push(VgmCommand::Ga20Write(
        Instance::Secondary,
        chip::Ga20Spec {
            register: 0x17,
            value: 0x66,
        },
    ));
    cases.push(VgmCommand::SegaPcmWrite(
        Instance::Secondary,
        chip::SegaPcmSpec {
            offset: 0x1234,
            value: 0x7F,
        },
    ));
    cases.push(VgmCommand::MultiPcmBankWrite(
        Instance::Secondary,
        chip::MultiPcmBankSpec {
            channel: 0x01,
            bank_offset: 0x0100,
        },
    ));
    cases.push(VgmCommand::ScspWrite(
        Instance::Secondary,
        chip::ScspSpec {
            offset: 0x20,
            value: 0x11,
        },
    ));
    cases.push(VgmCommand::WonderSwanWrite(
        Instance::Secondary,
        chip::WonderSwanSpec {
            offset: 0x1234,
            value: 0x22,
        },
    ));
    cases.push(VgmCommand::VsuWrite(
        Instance::Secondary,
        chip::VsuSpec {
            offset: 0x21,
            value: 0x33,
        },
    ));
    cases.push(VgmCommand::X1010Write(
        Instance::Secondary,
        chip::X1010Spec {
            offset: 0x22,
            value: 0x44,
        },
    ));
    cases.push(VgmCommand::Ymf278bWrite(
        Instance::Secondary,
        chip::Ymf278bSpec {
            port: 0x00,
            register: 0x30,
            value: 0x55,
        },
    ));
    cases.push(VgmCommand::Ymf271Write(
        Instance::Secondary,
        chip::Ymf271Spec {
            port: 0x00,
            register: 0x31,
            value: 0x66,
        },
    ));
    cases.push(VgmCommand::Scc1Write(
        Instance::Secondary,
        chip::Scc1Spec {
            port: 0x00,
            register: 0x32,
            value: 0x77,
        },
    ));
    cases.push(VgmCommand::K054539Write(
        Instance::Secondary,
        chip::K054539Spec {
            register: 0x33,
            value: 0x11,
        },
    ));
    cases.push(VgmCommand::C140Write(
        Instance::Secondary,
        chip::C140Spec {
            register: 0x34,
            value: 0x22,
        },
    ));
    cases.push(VgmCommand::Es5503Write(
        Instance::Secondary,
        chip::Es5503Spec {
            register: 0x35,
            value: 0x33,
        },
    ));
    cases.push(VgmCommand::Es5506D6Write(
        Instance::Secondary,
        chip::Es5506U16Spec {
            register: 0x36,
            value: 0x1234,
        },
    ));
    cases.push(VgmCommand::C352Write(
        Instance::Secondary,
        chip::C352Spec {
            register: 0x40,
            value: 0x44,
        },
    ));

    cases
}

/// Verify simple typed callbacks can be registered and invoked.
#[test]
fn test_callback_stream_typed_callbacks_invoked() {
    use soundlog::chip;

    let mut builder = VgmBuilder::new();

    // Add a YM2612 primary write and a PSG (SN76489) write
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x2A,
            value: 0x10,
        },
    ));
    builder.add_vgm_command(VgmCommand::Sn76489Write(
        Instance::Primary,
        chip::PsgSpec { value: 0xAA },
    ));

    let doc = builder.finalize();
    let mut callback_stream = VgmCallbackStream::from_document(doc);

    let ym_invoked = Rc::new(RefCell::new(0usize));
    let sn_invoked = Rc::new(RefCell::new(0usize));

    // Register typed callbacks
    {
        let ym_clone = ym_invoked.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Ym2612Spec, _sample, _event| {
            *ym_clone.borrow_mut() += 1;
        });
    }
    {
        let sn_clone = sn_invoked.clone();
        callback_stream.on_write(move |_inst, _spec: chip::PsgSpec, _sample, _event| {
            *sn_clone.borrow_mut() += 1;
        });
    }

    // Drain the stream so callbacks run
    for _ in &mut callback_stream {}

    assert_eq!(
        *ym_invoked.borrow(),
        1,
        "YM2612 callback should be invoked once"
    );
    assert_eq!(
        *sn_invoked.borrow(),
        1,
        "SN76489 callback should be invoked once"
    );
}

/// Verify that all the constructed commands are emitted once; this test uses a
/// single generic `on_any_command` to count emitted commands (sanity check).
#[test]
fn test_all_chip_writes_invoke_callback_once_per_command() {
    let cases = build_all_chip_write_cases();
    let mut builder = VgmBuilder::new();
    for c in &cases {
        builder.add_vgm_command(c.clone());
    }
    let doc = builder.finalize();

    // Expected counts
    let mut expected: HashMap<String, usize> = HashMap::new();
    for c in &cases {
        *expected.entry(format!("{:?}", c)).or_insert(0) += 1;
    }

    let actual_counts: Rc<RefCell<HashMap<String, usize>>> = Rc::new(RefCell::new(HashMap::new()));
    let mut callback_stream = VgmCallbackStream::from_document(doc);

    {
        let counts = actual_counts.clone();
        callback_stream.on_any_command(move |cmd, _sample| {
            *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
        });
    }

    // Drain
    for _ in &mut callback_stream {}

    // Compare expected vs actual
    let actual = actual_counts.borrow();
    for (k, v) in expected.into_iter() {
        let got = actual.get(&k).cloned().unwrap_or(0);
        assert_eq!(
            got, v,
            "Command {:?} should be emitted {} time(s), got {}",
            k, v, got
        );
    }
}

/// Verify that registering individual typed callbacks (one per chip spec type)
/// receives the corresponding writes. This test does not use `on_any_command`.
#[test]
fn test_individual_typed_callbacks_for_all_chips() {
    use soundlog::chip;

    let cases = build_all_chip_write_cases();
    let mut builder = VgmBuilder::new();
    for c in &cases {
        builder.add_vgm_command(c.clone());
    }
    let doc = builder.finalize();

    // Expected counts by Debug string
    let mut expected: HashMap<String, usize> = HashMap::new();
    for c in &cases {
        *expected.entry(format!("{:?}", c)).or_insert(0) += 1;
    }

    let actual_counts: Rc<RefCell<HashMap<String, usize>>> = Rc::new(RefCell::new(HashMap::new()));
    let mut callback_stream = VgmCallbackStream::from_document(doc);

    // Register typed callbacks for a representative set of spec types. Each
    // callback reconstructs the corresponding `VgmCommand` and increments the
    // emitted count keyed by its Debug representation. We prefer taking `spec`
    // by value (the callback parameter) and avoid extra clones when possible.

    let reg = |stream: &mut VgmCallbackStream, counts: Rc<RefCell<HashMap<String, usize>>>| {
        // Macro-like helper (closure) to reduce repetition
        let s = stream;
        // YM2612
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ym2612Spec, _sample, _event| {
                let cmd = VgmCommand::Ym2612Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // SN76489 / PSG
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::PsgSpec, _sample, _event| {
                let cmd = VgmCommand::Sn76489Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YM2413
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ym2413Spec, _sample, _event| {
                let cmd = VgmCommand::Ym2413Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YM2151
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ym2151Spec, _sample, _event| {
                let cmd = VgmCommand::Ym2151Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YM2203
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ym2203Spec, _sample, _event| {
                let cmd = VgmCommand::Ym2203Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YM2608
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ym2608Spec, _sample, _event| {
                let cmd = VgmCommand::Ym2608Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YM2610b
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ym2610Spec, _sample, _event| {
                let cmd = VgmCommand::Ym2610bWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YM3812
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ym3812Spec, _sample, _event| {
                let cmd = VgmCommand::Ym3812Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YM3526
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ym3526Spec, _sample, _event| {
                let cmd = VgmCommand::Ym3526Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // Y8950
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Y8950Spec, _sample, _event| {
                let cmd = VgmCommand::Y8950Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YMZ280B
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ymz280bSpec, _sample, _event| {
                let cmd = VgmCommand::Ymz280bWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YMF262
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ymf262Spec, _sample, _event| {
                let cmd = VgmCommand::Ymf262Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // AY8910
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ay8910Spec, _sample, _event| {
                let cmd = VgmCommand::Ay8910Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // Mikey
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::MikeySpec, _sample, _event| {
                let cmd = VgmCommand::MikeyWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // Game Gear PSG
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::GameGearPsgSpec, _sample, _event| {
                let cmd = VgmCommand::GameGearPsgWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // RF5C68 / RF5C164
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Rf5c68U8Spec, _sample, _event| {
                let cmd = VgmCommand::Rf5c68U8Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Rf5c68U16Spec, _sample, _event| {
                let cmd = VgmCommand::Rf5c68U16Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Rf5c164U8Spec, _sample, _event| {
                let cmd = VgmCommand::Rf5c164U8Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Rf5c164U16Spec, _sample, _event| {
                let cmd = VgmCommand::Rf5c164U16Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // PWM
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::PwmSpec, _sample, _event| {
                let cmd = VgmCommand::PwmWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // MultiPcm / Upd7759 / Okim / Huc / K053260 / Pokey / WonderSwanReg / SAA1099
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::MultiPcmSpec, _sample, _event| {
                let cmd = VgmCommand::MultiPcmWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Upd7759Spec, _sample, _event| {
                let cmd = VgmCommand::Upd7759Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Okim6258Spec, _sample, _event| {
                let cmd = VgmCommand::Okim6258Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Okim6295Spec, _sample, _event| {
                let cmd = VgmCommand::Okim6295Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Huc6280Spec, _sample, _event| {
                let cmd = VgmCommand::Huc6280Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::K053260Spec, _sample, _event| {
                let cmd = VgmCommand::K053260Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::PokeySpec, _sample, _event| {
                let cmd = VgmCommand::PokeyWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(
                move |inst, spec: chip::WonderSwanRegSpec, _sample, _event| {
                    let cmd = VgmCommand::WonderSwanRegWrite(inst, spec);
                    *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
                },
            );
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Saa1099Spec, _sample, _event| {
                let cmd = VgmCommand::Saa1099Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // ES5506 U8/U16, GA20, MultiPcmBank, QSound, SCSP, WonderSwan, VSU, X1010, C140, ES5503, C352
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Es5506U8Spec, _sample, _event| {
                let cmd = VgmCommand::Es5506BEWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Es5506U16Spec, _sample, _event| {
                let cmd = VgmCommand::Es5506D6Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ga20Spec, _sample, _event| {
                let cmd = VgmCommand::Ga20Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::MultiPcmBankSpec, _sample, _event| {
                let cmd = VgmCommand::MultiPcmBankWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::QsoundSpec, _sample, _event| {
                let cmd = VgmCommand::QsoundWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::ScspSpec, _sample, _event| {
                let cmd = VgmCommand::ScspWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::WonderSwanSpec, _sample, _event| {
                let cmd = VgmCommand::WonderSwanWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::VsuSpec, _sample, _event| {
                let cmd = VgmCommand::VsuWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::X1010Spec, _sample, _event| {
                let cmd = VgmCommand::X1010Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::C140Spec, _sample, _event| {
                let cmd = VgmCommand::C140Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Es5503Spec, _sample, _event| {
                let cmd = VgmCommand::Es5503Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::C352Spec, _sample, _event| {
                let cmd = VgmCommand::C352Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // GbDmg / NesApu
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::GbDmgSpec, _sample, _event| {
                let cmd = VgmCommand::GbDmgWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::NesApuSpec, _sample, _event| {
                let cmd = VgmCommand::NesApuWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // YMF278b / YMF271
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ymf278bSpec, _sample, _event| {
                let cmd = VgmCommand::Ymf278bWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Ymf271Spec, _sample, _event| {
                let cmd = VgmCommand::Ymf271Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        // SCC1 / K054539 / SegaPcm
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::Scc1Spec, _sample, _event| {
                let cmd = VgmCommand::Scc1Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::K054539Spec, _sample, _event| {
                let cmd = VgmCommand::K054539Write(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
        {
            let counts = counts.clone();
            s.on_write(move |inst, spec: chip::SegaPcmSpec, _sample, _event| {
                let cmd = VgmCommand::SegaPcmWrite(inst, spec);
                *counts.borrow_mut().entry(format!("{:?}", cmd)).or_insert(0) += 1;
            });
        }
    };

    // Register a broad set of typed callbacks via the helper above.
    reg(&mut callback_stream, actual_counts.clone());

    // Drain the stream so callbacks execute
    for _ in &mut callback_stream {}

    // Validate
    let actual = actual_counts.borrow();
    for (k, v) in expected.into_iter() {
        let got = actual.get(&k).cloned().unwrap_or(0);
        assert_eq!(
            got, v,
            "Command {:?} should be emitted {} time(s), got {}",
            k, v, got
        );
    }
}

/// Build a `VgmHeader` with every sound chip registered at a representative
/// clock, then verify that:
///
/// 1. `chip_instances()` returns an entry for every chip we registered.
/// 2. `VgmCallbackStream::track_chips` can be called without panicking.
/// 3. After `track_chips`, a representative chip (YM2612) actually has its
///    state tracker active: a write that triggers a state event causes the
///    callback to receive `Some(events)` rather than `None`.
#[test]
fn test_track_chips_enables_state_tracking_for_all_chips() {
    use soundlog::chip::Chip;
    use soundlog::vgm::command::{EndOfData, Instance};
    use soundlog::vgm::header::ChipInstances;
    use soundlog::{VgmBuilder, VgmCallbackStream};
    use std::cell::RefCell;
    use std::rc::Rc;

    // -----------------------------------------------------------------------
    // 1. Build a VgmHeader with every supported chip registered.
    //    Each chip gets a non-zero representative clock so chip_instances()
    //    includes it.  The secondary-instance bit (0x8000_0000) is NOT set
    //    here; we want Primary instances only to keep the list predictable.
    // -----------------------------------------------------------------------
    let mut builder = VgmBuilder::new();

    // FM chips
    builder.register_chip(Chip::Ym2612, Instance::Primary, 7_670_454);
    builder.register_chip(Chip::Ym2151, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Ym2203, Instance::Primary, 3_993_600);
    builder.register_chip(Chip::Ym2608, Instance::Primary, 8_000_000);
    builder.register_chip(Chip::Ym2610b, Instance::Primary, 8_000_000);
    builder.register_chip(Chip::Ym2413, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Ym3812, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Ym3526, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Y8950, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Ymf262, Instance::Primary, 14_318_180);
    builder.register_chip(Chip::Ymf271, Instance::Primary, 16_000_000);
    builder.register_chip(Chip::Ymf278b, Instance::Primary, 33_868_800);
    builder.register_chip(Chip::Ymz280b, Instance::Primary, 16_934_400);

    // PSG / tone generators
    builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Ay8910, Instance::Primary, 1_789_773);
    builder.register_chip(Chip::GbDmg, Instance::Primary, 4_194_304);
    builder.register_chip(Chip::NesApu, Instance::Primary, 1_789_773);
    builder.register_chip(Chip::Huc6280, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Pokey, Instance::Primary, 1_789_773);
    builder.register_chip(Chip::Saa1099, Instance::Primary, 8_000_000);
    builder.register_chip(Chip::WonderSwan, Instance::Primary, 3_072_000);
    builder.register_chip(Chip::Vsu, Instance::Primary, 5_000_000);
    builder.register_chip(Chip::Mikey, Instance::Primary, 16_000_000);
    builder.register_chip(Chip::K051649, Instance::Primary, 1_500_000);

    // PCM / sample chips
    builder.register_chip(Chip::SegaPcm, Instance::Primary, 7_670_454);
    builder.register_chip(Chip::Rf5c68, Instance::Primary, 12_500_000);
    builder.register_chip(Chip::Rf5c164, Instance::Primary, 12_500_000);
    builder.register_chip(Chip::Pwm, Instance::Primary, 23_011_361);
    builder.register_chip(Chip::MultiPcm, Instance::Primary, 8_053_975);
    builder.register_chip(Chip::Upd7759, Instance::Primary, 640_000);
    builder.register_chip(Chip::Okim6258, Instance::Primary, 4_000_000);
    builder.register_chip(Chip::Okim6295, Instance::Primary, 1_000_000);
    builder.register_chip(Chip::K054539, Instance::Primary, 18_432_000);
    builder.register_chip(Chip::C140, Instance::Primary, 21_390);
    builder.register_chip(Chip::C352, Instance::Primary, 24_192_000);
    builder.register_chip(Chip::K053260, Instance::Primary, 3_579_545);
    builder.register_chip(Chip::Qsound, Instance::Primary, 4_000_000);
    builder.register_chip(Chip::Scsp, Instance::Primary, 22_579_200);
    builder.register_chip(Chip::Es5503, Instance::Primary, 7_159_090);
    builder.register_chip(Chip::Es5506U8, Instance::Primary, 16_000_000);
    builder.register_chip(Chip::X1010, Instance::Primary, 16_000_000);
    builder.register_chip(Chip::Ga20, Instance::Primary, 3_579_545);

    // Add a YM2612 key-on sequence so we can verify the state tracker fires an
    // event later.  These writes mirror the minimal key-on pattern used
    // elsewhere in the test-suite.
    use soundlog::chip;
    use soundlog::vgm::command::VgmCommand;

    // Set algorithm/FB for channel 0 (reg 0xB0, port 0)
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0xB0,
            value: 0x00,
        },
    ));
    // Operator 1: total level = 0 (max volume, reg 0x40)
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x40,
            value: 0x00,
        },
    ));
    // F-number low  (reg 0xA4, port 0) – arbitrary non-zero frequency
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0xA4,
            value: 0x22,
        },
    ));
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0xA0,
            value: 0x69,
        },
    ));
    // Key-on: all operators for channel 0 (reg 0x28, value 0xF0)
    builder.add_vgm_command(VgmCommand::Ym2612Write(
        Instance::Primary,
        chip::Ym2612Spec {
            port: 0,
            register: 0x28,
            value: 0xF0,
        },
    ));

    builder.add_vgm_command(VgmCommand::EndOfData(EndOfData));

    let doc = builder.finalize();
    let header = doc.header.clone();

    // -----------------------------------------------------------------------
    // 2. Obtain ChipInstances and verify it is non-empty and contains every
    //    chip we registered.
    // -----------------------------------------------------------------------
    let instances: ChipInstances = header.chip_instances();
    assert!(
        !instances.is_empty(),
        "chip_instances() must not be empty when chips are registered"
    );

    // Collect the Chip variants present in the instance list.
    let present_chips: Vec<chip::Chip> = instances
        .iter()
        .map(|(_inst, ch, _clk)| ch.clone())
        .collect();

    // Every chip we registered should appear in the list.
    let expected_chips: &[chip::Chip] = &[
        Chip::Sn76489,
        Chip::Ym2413,
        Chip::Ym2612,
        Chip::Ym2151,
        Chip::SegaPcm,
        Chip::Rf5c68,
        Chip::Ym2203,
        Chip::Ym2608,
        Chip::Ym2610b,
        Chip::Ym3812,
        Chip::Ym3526,
        Chip::Y8950,
        Chip::Ymf262,
        Chip::Ymf278b,
        Chip::Ymf271,
        Chip::Ymz280b,
        Chip::Rf5c164,
        Chip::Pwm,
        Chip::Ay8910,
        Chip::GbDmg,
        Chip::NesApu,
        Chip::MultiPcm,
        Chip::Upd7759,
        Chip::Okim6258,
        Chip::Okim6295,
        Chip::K051649,
        Chip::K054539,
        Chip::Huc6280,
        Chip::C140,
        Chip::K053260,
        Chip::Pokey,
        Chip::Qsound,
        Chip::Scsp,
        Chip::WonderSwan,
        Chip::Vsu,
        Chip::Saa1099,
        Chip::Es5503,
        Chip::Es5506U8,
        Chip::X1010,
        Chip::C352,
        Chip::Ga20,
        Chip::Mikey,
    ];
    for expected in expected_chips {
        assert!(
            present_chips.contains(expected),
            "chip_instances() must contain {:?}",
            expected
        );
    }

    // -----------------------------------------------------------------------
    // 3. Call track_chips and verify it completes without panic.
    //    Then confirm that the YM2612 state tracker was activated by checking
    //    that the key-on write triggers a KeyOn StateEvent in the callback.
    // -----------------------------------------------------------------------
    let mut callback_stream = VgmCallbackStream::from_document(doc);

    // This must not panic for any chip in the list.
    callback_stream.track_chips(&instances);

    // Register a YM2612 write callback to detect whether the state tracker
    // produces a KeyOn event (which only happens when the tracker is active).
    let key_on_detected = Rc::new(RefCell::new(false));
    {
        let flag = key_on_detected.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Ym2612Spec, _sample, event| {
            if event.is_some_and(|events| {
                events
                    .iter()
                    .any(|e| matches!(e, soundlog::chip::event::StateEvent::KeyOn { .. }))
            }) {
                *flag.borrow_mut() = true;
            }
        });
    }

    // Drain the stream so all commands are processed.
    for _ in &mut callback_stream {}

    assert!(
        *key_on_detected.borrow(),
        "YM2612 state tracker must be active after track_chips: expected a KeyOn event"
    );
}

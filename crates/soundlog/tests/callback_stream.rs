use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use soundlog::VgmBuilder;
use soundlog::VgmCallbackStream;
use soundlog::chip::Chip;
use soundlog::vgm::command::{Instance, VgmCommand};

#[test]
fn test_typed_callbacks_for_state_tracker_chips_invoked() {
    use soundlog::chip;

    // Build a simple VGM document that contains one representative write
    // for each of the state-tracker chips we want to validate.
    let mut builder = VgmBuilder::new();

    builder.register_chip(Chip::SegaPcm, Instance::Primary, 0x1);
    builder.register_chip(Chip::Rf5c68, Instance::Primary, 0x2);
    builder.register_chip(Chip::Rf5c164, Instance::Primary, 0x3);
    builder.register_chip(Chip::Pwm, Instance::Primary, 0x4);
    builder.register_chip(Chip::MultiPcm, Instance::Primary, 0x5);
    builder.register_chip(Chip::Upd7759, Instance::Primary, 0x6);
    builder.register_chip(Chip::Okim6258, Instance::Primary, 0x7);
    builder.register_chip(Chip::Okim6295, Instance::Primary, 0x8);
    builder.register_chip(Chip::K054539, Instance::Primary, 0x9);
    builder.register_chip(Chip::C140, Instance::Primary, 0xa);
    builder.register_chip(Chip::C352, Instance::Primary, 0xb);
    builder.register_chip(Chip::K053260, Instance::Primary, 0xc);
    builder.register_chip(Chip::Qsound, Instance::Primary, 0xd);
    builder.register_chip(Chip::Scsp, Instance::Primary, 0xe);
    builder.register_chip(Chip::Es5503, Instance::Primary, 0xf);
    builder.register_chip(Chip::Es5506U16, Instance::Primary, 0x10);
    builder.register_chip(Chip::Es5506U8, Instance::Primary, 0x11);
    builder.register_chip(Chip::X1010, Instance::Primary, 0x12);
    builder.register_chip(Chip::Ga20, Instance::Primary, 0x13);
    builder.register_chip(Chip::Ymz280b, Instance::Primary, 0x14);

    // Add one write per chip (Primary instance) with minimal/sample data.
    builder.add_vgm_command(VgmCommand::SegaPcmWrite(
        Instance::Primary,
        chip::SegaPcmSpec {
            offset: 0x1234,
            value: 0x7F,
        },
    ));
    builder.add_vgm_command(VgmCommand::Rf5c68U8Write(
        Instance::Primary,
        chip::Rf5c68U8Spec {
            offset: 0x10,
            value: 0x21,
        },
    ));
    builder.add_vgm_command(VgmCommand::Rf5c164U8Write(
        Instance::Primary,
        chip::Rf5c164U8Spec {
            offset: 0x05,
            value: 0x23,
        },
    ));
    builder.add_vgm_command(VgmCommand::PwmWrite(
        Instance::Primary,
        chip::PwmSpec {
            register: 0x01,
            value: 0x0FFFu32,
        },
    ));
    builder.add_vgm_command(VgmCommand::MultiPcmWrite(
        Instance::Primary,
        chip::MultiPcmSpec {
            register: 0x0C,
            value: 0x44,
        },
    ));
    builder.add_vgm_command(VgmCommand::Upd7759Write(
        Instance::Primary,
        chip::Upd7759Spec {
            register: 0x0D,
            value: 0x45,
        },
    ));
    builder.add_vgm_command(VgmCommand::Okim6258Write(
        Instance::Primary,
        chip::Okim6258Spec {
            register: 0x0E,
            value: 0x46,
        },
    ));
    builder.add_vgm_command(VgmCommand::Okim6295Write(
        Instance::Primary,
        chip::Okim6295Spec {
            register: 0x0F,
            value: 0x47,
        },
    ));
    builder.add_vgm_command(VgmCommand::K054539Write(
        Instance::Primary,
        chip::K054539Spec {
            register: 0x33,
            value: 0x63,
        },
    ));
    builder.add_vgm_command(VgmCommand::C140Write(
        Instance::Primary,
        chip::C140Spec {
            register: 0x34,
            value: 0x64,
        },
    ));
    builder.add_vgm_command(VgmCommand::C352Write(
        Instance::Primary,
        chip::C352Spec {
            register: 0x40,
            value: 0x66,
        },
    ));
    builder.add_vgm_command(VgmCommand::K053260Write(
        Instance::Primary,
        chip::K053260Spec {
            register: 0x12,
            value: 0x49,
        },
    ));
    builder.add_vgm_command(VgmCommand::QsoundWrite(
        Instance::Primary,
        chip::QsoundSpec {
            register: 0x01,
            value: 0x50,
        },
    ));
    builder.add_vgm_command(VgmCommand::ScspWrite(
        Instance::Primary,
        chip::ScspSpec {
            offset: 0x20,
            value: 0x51,
        },
    ));
    builder.add_vgm_command(VgmCommand::Es5503Write(
        Instance::Primary,
        chip::Es5503Spec {
            register: 0x35,
            value: 0x65,
        },
    ));
    // Use the U16 variant example used elsewhere in the tests.
    builder.add_vgm_command(VgmCommand::Es5506D6Write(
        Instance::Primary,
        chip::Es5506U16Spec {
            register: 0x36,
            value: 0x1234,
        },
    ));
    builder.add_vgm_command(VgmCommand::X1010Write(
        Instance::Primary,
        chip::X1010Spec {
            offset: 0x22,
            value: 0x54,
        },
    ));
    builder.add_vgm_command(VgmCommand::Ga20Write(
        Instance::Primary,
        chip::Ga20Spec {
            register: 0x17,
            value: 0x4E,
        },
    ));
    builder.add_vgm_command(VgmCommand::Ymz280bWrite(
        Instance::Primary,
        chip::Ymz280bSpec {
            register: 0x08,
            value: 0xC2,
        },
    ));

    let doc = builder.finalize();
    let instances = doc.header.chip_instances();

    let mut callback_stream = VgmCallbackStream::from_document(doc);
    callback_stream.track_chips(&instances);

    // Create invocation flags for each typed callback we will register.
    let sega_pcm_called = Rc::new(RefCell::new(false));
    let rf5c68_called = Rc::new(RefCell::new(false));
    let rf5c164_called = Rc::new(RefCell::new(false));
    let pwm_called = Rc::new(RefCell::new(false));
    let multi_pcm_called = Rc::new(RefCell::new(false));
    let upd7759_called = Rc::new(RefCell::new(false));
    let okim6258_called = Rc::new(RefCell::new(false));
    let okim6295_called = Rc::new(RefCell::new(false));
    let k054539_called = Rc::new(RefCell::new(false));
    let c140_called = Rc::new(RefCell::new(false));
    let c352_called = Rc::new(RefCell::new(false));
    let k053260_called = Rc::new(RefCell::new(false));
    let qsound_called = Rc::new(RefCell::new(false));
    let scsp_called = Rc::new(RefCell::new(false));
    let es5503_called = Rc::new(RefCell::new(false));
    let es5506_called = Rc::new(RefCell::new(false));
    let x1010_called = Rc::new(RefCell::new(false));
    let ga20_called = Rc::new(RefCell::new(false));
    let ymz280b_called = Rc::new(RefCell::new(false));

    // Register typed callbacks. Each one sets its flag to true when invoked.
    {
        let f = sega_pcm_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::SegaPcmSpec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = rf5c68_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Rf5c68U8Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = rf5c164_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Rf5c164U8Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = pwm_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::PwmSpec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = multi_pcm_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::MultiPcmSpec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = upd7759_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Upd7759Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = okim6258_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Okim6258Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = okim6295_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Okim6295Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = k054539_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::K054539Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = c140_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::C140Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = c352_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::C352Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = k053260_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::K053260Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = qsound_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::QsoundSpec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = scsp_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::ScspSpec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = es5503_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Es5503Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = es5506_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Es5506U16Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = x1010_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::X1010Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = ga20_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Ga20Spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = ymz280b_called.clone();
        callback_stream.on_write(move |_inst, _spec: chip::Ymz280bSpec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }

    // Drain the stream to trigger callbacks.
    for _ in &mut callback_stream {}

    // Verify each typed callback was invoked at least once.
    assert!(
        *sega_pcm_called.borrow(),
        "SegaPcm typed callback should be invoked"
    );
    assert!(
        *rf5c68_called.borrow(),
        "Rf5c68 typed callback should be invoked"
    );
    assert!(
        *rf5c164_called.borrow(),
        "Rf5c164 typed callback should be invoked"
    );
    assert!(*pwm_called.borrow(), "Pwm typed callback should be invoked");
    assert!(
        *multi_pcm_called.borrow(),
        "MultiPcm typed callback should be invoked"
    );
    assert!(
        *upd7759_called.borrow(),
        "Upd7759 typed callback should be invoked"
    );
    assert!(
        *okim6258_called.borrow(),
        "Okim6258 typed callback should be invoked"
    );
    assert!(
        *okim6295_called.borrow(),
        "Okim6295 typed callback should be invoked"
    );
    assert!(
        *k054539_called.borrow(),
        "K054539 typed callback should be invoked"
    );
    assert!(
        *c140_called.borrow(),
        "C140 typed callback should be invoked"
    );
    assert!(
        *c352_called.borrow(),
        "C352 typed callback should be invoked"
    );
    assert!(
        *k053260_called.borrow(),
        "K053260 typed callback should be invoked"
    );
    assert!(
        *qsound_called.borrow(),
        "QSound typed callback should be invoked"
    );
    assert!(
        *scsp_called.borrow(),
        "SCSP typed callback should be invoked"
    );
    assert!(
        *es5503_called.borrow(),
        "ES5503 typed callback should be invoked"
    );
    assert!(
        *es5506_called.borrow(),
        "ES5506 typed callback should be invoked"
    );
    assert!(
        *x1010_called.borrow(),
        "X1010 typed callback should be invoked"
    );
    assert!(
        *ga20_called.borrow(),
        "GA20 typed callback should be invoked"
    );
    assert!(
        *ymz280b_called.borrow(),
        "YMZ280B typed callback should be invoked"
    );
}

#[test]
fn test_setters_getters_loop_and_fadeout() {
    // Create an empty VGM document and a callback stream from it.
    let builder = VgmBuilder::new();
    let doc = builder.finalize();
    let mut callback_stream = VgmCallbackStream::from_document(doc);

    // Loop base setter/getter (i8)
    callback_stream.set_loop_base(5);
    assert_eq!(callback_stream.loop_base(), 5);
    callback_stream.set_loop_base(-7);
    assert_eq!(callback_stream.loop_base(), -7);

    // Loop modifier setter/getter (u8)
    callback_stream.set_loop_modifier(42);
    assert_eq!(callback_stream.loop_modifier(), 42u8);

    // Fadeout samples setter/getter (Option<usize>)
    callback_stream.set_fadeout_samples(Some(12345usize));
    assert_eq!(callback_stream.fadeout_samples(), Some(12345usize));
    callback_stream.set_fadeout_samples(None);
    assert_eq!(callback_stream.fadeout_samples(), None);
}

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

/// Verify miscellaneous non-chip callbacks are invoked when the corresponding
/// `VgmCommand` variants are present. This covers:
/// - AY8910 stereo mask
/// - Reserved U8/U16/U24/U32 writes
/// - UnknownCommand
/// - WaitSamples
/// - DataBlock
/// - PcmRamWrite
/// - EndOfData (assert it is not invoked in practice)
#[test]
fn test_misc_callbacks_invoked() {
    use soundlog::vgm::command::{
        Ay8910StereoMask, EndOfData, PcmRamWrite, ReservedU8, ReservedU16, ReservedU24,
        ReservedU32, UnknownSpec, VgmCommand, WaitSamples,
    };
    use soundlog::vgm::detail::{RomRamChipType, RomRamDump};
    use soundlog::{VgmBuilder, VgmCallbackStream};
    use std::cell::RefCell;
    use std::rc::Rc;

    let mut builder = VgmBuilder::new();

    // AY8910 stereo mask (opcode 0xD0 style)
    let ay_mask = Ay8910StereoMask {
        chip_instance: Instance::Primary,
        is_ym2203: false,
        left_ch1: true,
        right_ch1: false,
        left_ch2: true,
        right_ch2: false,
        left_ch3: true,
        right_ch3: false,
    };
    builder.add_vgm_command(VgmCommand::AY8910StereoMask(ay_mask.clone()));

    // Reserved U8 / U16 / U24 / U32 writes (use arbitrary dd bytes/opcodes)
    builder.add_vgm_command(VgmCommand::ReservedU8Write(ReservedU8 {
        opcode: 0xC0,
        dd: 0x11,
    }));
    builder.add_vgm_command(VgmCommand::ReservedU16Write(ReservedU16 {
        opcode: 0xC1,
        dd1: 0x22,
        dd2: 0x33,
    }));
    builder.add_vgm_command(VgmCommand::ReservedU24Write(ReservedU24 {
        opcode: 0xC2,
        dd1: 0x44,
        dd2: 0x55,
        dd3: 0x66,
    }));
    builder.add_vgm_command(VgmCommand::ReservedU32Write(ReservedU32 {
        opcode: 0xE3,
        dd1: 0x77,
        dd2: 0x88,
        dd3: 0x99,
        dd4: 0xAA,
    }));

    // Unknown command
    builder.add_vgm_command(VgmCommand::UnknownCommand(UnknownSpec {
        opcode: 0xAB,
        offset: 0,
    }));

    // WaitSamples
    builder.add_vgm_command(VgmCommand::WaitSamples(WaitSamples(123)));

    // DataBlock
    builder.attach_data_block(RomRamDump {
        chip_type: RomRamChipType::C140Rom,
        rom_size: 3,
        start_address: 0,
        data: vec![0x01, 0x02, 0x03],
    });

    // PCM RAM write (boxed)
    let pcm = PcmRamWrite {
        marker: 0x66,
        chip_type: soundlog::vgm::detail::StreamChipType::Unknown(0x7F),
        read_offset: 0,
        write_offset: 0,
        size: 2,
        data: vec![0xAA, 0xBB],
    };
    builder.add_vgm_command(VgmCommand::PcmRamWrite(Box::new(pcm)));

    // Explicit EndOfData
    builder.add_vgm_command(VgmCommand::EndOfData(EndOfData));

    let doc = builder.finalize();
    let mut callback_stream = VgmCallbackStream::from_document(doc);

    // Flags to detect callback invocations
    let ay_invoked = Rc::new(RefCell::new(false));
    let reserved_u8_invoked = Rc::new(RefCell::new(false));
    let reserved_u16_invoked = Rc::new(RefCell::new(false));
    let reserved_u24_invoked = Rc::new(RefCell::new(false));
    let reserved_u32_invoked = Rc::new(RefCell::new(false));
    let unknown_invoked = Rc::new(RefCell::new(false));
    let wait_invoked = Rc::new(RefCell::new(false));
    let data_block_invoked = Rc::new(RefCell::new(false));
    let pcm_invoked = Rc::new(RefCell::new(false));
    let end_of_data_invoked = Rc::new(RefCell::new(false));

    {
        let f = ay_invoked.clone();
        callback_stream.on_ay8910_stereo_mask(move |_mask, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = reserved_u8_invoked.clone();
        callback_stream.on_reserved_u8_write(move |_spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = reserved_u16_invoked.clone();
        callback_stream.on_reserved_u16_write(move |_spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = reserved_u24_invoked.clone();
        callback_stream.on_reserved_u24_write(move |_spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = reserved_u32_invoked.clone();
        callback_stream.on_reserved_u32_write(move |_spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = unknown_invoked.clone();
        callback_stream.on_unknown_command(move |_spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = wait_invoked.clone();
        callback_stream.on_wait(move |_spec, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = data_block_invoked.clone();
        callback_stream.on_data_block(move |_db, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = pcm_invoked.clone();
        callback_stream.on_pcm_ram_write(move |_p, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }
    {
        let f = end_of_data_invoked.clone();
        callback_stream.on_end_of_data(move |_e, _sample, _ev| {
            *f.borrow_mut() = true;
        });
    }

    // Drain the stream to trigger callbacks
    for _ in &mut callback_stream {}

    // Assertions
    assert!(*ay_invoked.borrow(), "AY8910 stereo mask callback must run");
    assert!(
        *reserved_u8_invoked.borrow(),
        "Reserved U8 callback must run"
    );
    assert!(
        *reserved_u16_invoked.borrow(),
        "Reserved U16 callback must run"
    );
    assert!(
        *reserved_u24_invoked.borrow(),
        "Reserved U24 callback must run"
    );
    assert!(
        *reserved_u32_invoked.borrow(),
        "Reserved U32 callback must run"
    );
    assert!(
        *unknown_invoked.borrow(),
        "UnknownCommand callback must run"
    );
    assert!(*wait_invoked.borrow(), "WaitSamples callback must run");
    assert!(*data_block_invoked.borrow(), "DataBlock callback must run");
    assert!(*pcm_invoked.borrow(), "PcmRamWrite callback must run");

    // EndOfData callback is reserved and not invoked in practice (iterator ends
    // before the callback can be called); assert it remains false.
    assert!(
        !*end_of_data_invoked.borrow(),
        "EndOfData callback is reserved and should not be invoked by iteration"
    );
}

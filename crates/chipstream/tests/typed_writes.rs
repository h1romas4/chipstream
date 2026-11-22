use std::cell::RefCell;
use std::rc::Rc;

use chipstream::audio::ChipCapture;
#[cfg(feature = "okim6258")]
use chipstream::chip::Okim6258;
#[cfg(feature = "okim6295")]
use chipstream::chip::Okim6295;
#[cfg(feature = "pwm")]
use chipstream::chip::Pwm;
#[cfg(feature = "segapcm")]
use chipstream::chip::SegaPcm;
#[cfg(feature = "c140")]
use chipstream::chip::{C140, C140Type};
use chipstream::chip::{ChipError, ChipRegisterWrite, ChipWrite, SoundChip, YmfmChip};
#[cfg(feature = "sn76496")]
use chipstream::chip::{Sn76496, Sn76496Config};
use soundlog::chip::*;

type Writes = Rc<RefCell<Vec<(u8, u8, u32)>>>;

struct RecordingChip {
    chip: Chip,
    writes: Writes,
}

impl SoundChip for RecordingChip {
    fn chip_type(&self) -> &Chip {
        &self.chip
    }

    fn clock(&self) -> u32 {
        1
    }

    fn sample_rate(&self) -> u32 {
        1
    }

    fn channels(&self) -> usize {
        1
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        let expected = write.chip();
        if self.chip != expected {
            return Err(ChipError::ChipMismatch {
                expected,
                actual: self.chip.clone(),
            });
        }
        let fields = match write {
            ChipRegisterWrite::Psg(s) => (0, 0, s.value.into()),
            ChipRegisterWrite::GameGearPsg(s) => (0, 1, s.value.into()),
            ChipRegisterWrite::SegaPcm(s) => {
                let [bank, register] = s.offset.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::C140(s) => {
                let [bank, register] = s.register.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::C352(s) => {
                let [bank, register] = s.register.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::Es5503(s) => {
                let [bank, register] = s.register.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::Pwm(s) => (0, s.register, s.value),
            ChipRegisterWrite::Rf5c68U8(s) => (0, s.offset, s.value.into()),
            ChipRegisterWrite::Rf5c164U8(s) => (0, s.offset, s.value.into()),
            ChipRegisterWrite::Rf5c68U16(s) => {
                let [bank, register] = s.offset.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::Rf5c164U16(s) => {
                let [bank, register] = s.offset.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::Saa1099(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Scc1(s) => (s.port, s.register, s.value.into()),
            ChipRegisterWrite::Huc6280(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Pokey(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Ga20(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::GbDmg(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Vsu(s) => {
                let [bank, register] = s.offset.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::WonderSwan(s) => {
                let [bank, register] = s.offset.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::WonderSwanReg(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::K053260(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::K054539(s) => {
                let [bank, register] = s.register.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::X1010(s) => {
                let [bank, register] = s.offset.to_be_bytes();
                (bank, register, s.value.into())
            }
            ChipRegisterWrite::Ymz280b(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::MultiPcm(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::MultiPcmBank(s) => (s.channel, 0, s.bank_offset.into()),
            ChipRegisterWrite::Mikey(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::NesApu(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Upd7759(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Ay8910(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Okim6258(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Okim6295(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Ym2413(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Ym2151(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Ym2203(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Ym3812(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Ym3526(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Y8950(s) => (0, s.register, s.value.into()),
            ChipRegisterWrite::Ym2612(s) => (s.port, s.register, s.value.into()),
            ChipRegisterWrite::Ym2608(s) => (s.port, s.register, s.value.into()),
            ChipRegisterWrite::Ym2610(s) => (s.port, s.register, s.value.into()),
            ChipRegisterWrite::Ymf262(s) => (s.port, s.register, s.value.into()),
            ChipRegisterWrite::Ymf278b(s) => (s.port, s.register, s.value.into()),
        };
        self.writes.borrow_mut().push(fields);
        Ok(())
    }

    fn generate(&mut self, buffer: &mut [i32]) -> Result<(), ChipError> {
        buffer.fill(0);
        Ok(())
    }

    fn reset(&mut self) {}
}

struct CustomWrite(u8);

impl ChipWrite<CustomWrite> for RecordingChip {
    fn apply_typed_write(&mut self, spec: CustomWrite) -> Result<(), ChipError> {
        self.apply_register_write(ChipRegisterWrite::Ym2151(Ym2151Spec {
            register: 7,
            value: spec.0,
        }))
    }
}

#[test]
fn custom_chips_can_implement_typed_writes() {
    let writes = Writes::default();
    let mut route = ChipCapture::new(RecordingChip {
        chip: Chip::Ym2151,
        writes: writes.clone(),
    });
    route.chip_mut().write(CustomWrite(0x55)).unwrap();
    route.chip_mut().write(CustomWrite(0x66)).unwrap();
    assert_eq!(&*writes.borrow(), &[(0, 7, 0x55), (0, 7, 0x66)]);
}

fn check_mapping<S>(chip: Chip, spec: S, expected: (u8, u8, u32))
where
    Box<dyn SoundChip>: ChipWrite<S>,
{
    let writes = Writes::default();
    let backend: Box<dyn SoundChip> = Box::new(RecordingChip {
        chip,
        writes: writes.clone(),
    });
    ChipCapture::new(backend).chip_mut().write(spec).unwrap();
    assert_eq!(&*writes.borrow(), &[expected]);
}

#[test]
fn dynamic_routes_map_all_supported_specs_without_narrowing() {
    check_mapping(Chip::Sn76489, PsgSpec { value: 0x85 }, (0, 0, 0x85));
    check_mapping(Chip::Sn76489, GameGearPsgSpec { value: 0x42 }, (0, 1, 0x42));
    check_mapping(
        Chip::SegaPcm,
        SegaPcmSpec {
            offset: 0x1234,
            value: 0x56,
        },
        (0x12, 0x34, 0x56),
    );
    check_mapping(
        Chip::C140,
        C140Spec {
            register: 0x1234,
            value: 0x56,
        },
        (0x12, 0x34, 0x56),
    );
    check_mapping(
        Chip::C352,
        C352Spec {
            register: 0x0202,
            value: 0xabcd,
        },
        (0x02, 0x02, 0xabcd),
    );
    check_mapping(
        Chip::K053260,
        K053260Spec {
            register: 0x2f,
            value: 0x02,
        },
        (0, 0x2f, 0x02),
    );
    check_mapping(
        Chip::K054539,
        K054539Spec {
            register: 0x022f,
            value: 0x81,
        },
        (2, 0x2f, 0x81),
    );
    check_mapping(
        Chip::X1010,
        X1010Spec {
            offset: 0x1fff,
            value: 0x81,
        },
        (0x1f, 0xff, 0x81),
    );
    check_mapping(
        Chip::Ymz280b,
        Ymz280bSpec {
            register: 0xff,
            value: 0x80,
        },
        (0, 0xff, 0x80),
    );
    check_mapping(
        Chip::MultiPcm,
        MultiPcmSpec {
            register: 2,
            value: 7,
        },
        (0, 2, 7),
    );
    check_mapping(
        Chip::NesApu,
        NesApuSpec {
            register: 0x11,
            value: 0x7f,
        },
        (0, 0x11, 0x7f),
    );
    check_mapping(
        Chip::Upd7759,
        Upd7759Spec {
            register: 2,
            value: 7,
        },
        (0, 2, 7),
    );
    check_mapping(
        Chip::MultiPcm,
        MultiPcmBankSpec {
            channel: 3,
            bank_offset: 0x1234,
        },
        (3, 0, 0x1234),
    );
    check_mapping(
        Chip::Pwm,
        PwmSpec {
            register: 4,
            value: 0xabcdef,
        },
        (0, 4, 0xabcdef),
    );
    check_mapping(
        Chip::Okim6258,
        Okim6258Spec {
            register: 2,
            value: 3,
        },
        (0, 2, 3),
    );
    check_mapping(
        Chip::Okim6295,
        Okim6295Spec {
            register: 0x0c,
            value: 1,
        },
        (0, 0x0c, 1),
    );
    macro_rules! single {
        ($chip:ident, $spec:ident) => {
            check_mapping(
                Chip::$chip,
                $spec {
                    register: 0x22,
                    value: 0x33,
                },
                (0, 0x22, 0x33),
            );
        };
    }
    macro_rules! multi {
        ($chip:ident, $spec:ident) => {
            check_mapping(
                Chip::$chip,
                $spec {
                    port: 1,
                    register: 0x22,
                    value: 0x33,
                },
                (1, 0x22, 0x33),
            );
        };
    }
    single!(Ym2413, Ym2413Spec);
    single!(Ym2151, Ym2151Spec);
    single!(Ym2203, Ym2203Spec);
    single!(Ym3812, Ym3812Spec);
    single!(Ym3526, Ym3526Spec);
    single!(Y8950, Y8950Spec);
    multi!(Ym2612, Ym2612Spec);
    multi!(Ym2608, Ym2608Spec);
    multi!(Ym2610b, Ym2610Spec);
    multi!(Ymf262, Ymf262Spec);
    multi!(Ymf278b, Ymf278bSpec);
}

#[test]
#[cfg(any(
    feature = "c140",
    feature = "okim6258",
    feature = "okim6295",
    feature = "segapcm"
))]
fn concrete_pcm_chips_apply_typed_addresses_and_values() {
    #[cfg(feature = "segapcm")]
    {
        let mut sega = ChipCapture::new(SegaPcm::new(4_000_000).unwrap());
        sega.chip_mut()
            .write(SegaPcmSpec {
                offset: 0x185,
                value: 0xa5,
            })
            .unwrap();
        assert_eq!(sega.chip().read_register(0x185), 0xa5);
    }

    #[cfg(feature = "c140")]
    {
        let mut c140 = ChipCapture::new(C140::new(12_288_000, C140Type::System2).unwrap());
        c140.chip_mut()
            .write(C140Spec {
                register: 0x103,
                value: 0x7a,
            })
            .unwrap();
        assert_eq!(c140.chip().read_register(0x103), 0x7a);
    }

    #[cfg(feature = "okim6258")]
    {
        let mut oki = ChipCapture::new(Okim6258::new(8_000_000).unwrap());
        oki.chip_mut()
            .write(Okim6258Spec {
                register: 2,
                value: 1,
            })
            .unwrap();
        assert_eq!(oki.chip().pan_mask(), 1);
        assert!(matches!(
            oki.chip_mut().write(Okim6258Spec {
                register: 3,
                value: 1
            }),
            Err(ChipError::InvalidRegister { .. })
        ));
    }

    #[cfg(feature = "okim6295")]
    {
        let mut oki6295 = ChipCapture::new(Okim6295::new(1_000_000).unwrap());
        oki6295
            .chip_mut()
            .write(Okim6295Spec {
                register: 0x0c,
                value: 1,
            })
            .unwrap();
        assert_eq!(oki6295.chip().sample_rate_ratio().denominator().get(), 132);
    }
}

#[test]
#[cfg(any(feature = "sn76496", feature = "pwm"))]
fn typed_psg_and_wide_pwm_preserve_output() {
    #[cfg(feature = "sn76496")]
    {
        let config = Sn76496Config {
            stereo: true,
            ..Default::default()
        };
        let mut psg = ChipCapture::new(Sn76496::with_config(3_579_545, config).unwrap());
        for value in [0x82, 0x00, 0x90] {
            psg.chip_mut().write(PsgSpec { value }).unwrap();
        }
        psg.chip_mut()
            .write(GameGearPsgSpec { value: 0x10 })
            .unwrap();
        let mut samples = [0; 16];
        assert_eq!(
            psg.capture(&mut samples).unwrap().samples(),
            &[
                -8191, 0, -8191, 0, 0, 0, 0, 0, -8191, 0, -8191, 0, 0, 0, 0, 0
            ]
        );
    }

    #[cfg(feature = "pwm")]
    {
        let mut pwm = ChipCapture::new(Pwm::new(44_100).unwrap());
        for (register, value) in [(1, 3), (0, 5), (2, 0xabc)] {
            pwm.chip_mut().write(PwmSpec { register, value }).unwrap();
        }
        let mut samples = [0; 2];
        assert_eq!(
            pwm.capture(&mut samples).unwrap().samples(),
            &[0xabc * 32, 0]
        );
        assert!(matches!(
            pwm.chip_mut().write(PwmSpec {
                register: 4,
                value: 0x1000
            }),
            Err(ChipError::InvalidRegisterValue { value: 0x1000, .. })
        ));
    }
}

#[test]
fn dynamic_chip_mismatch_is_rejected_before_writing() {
    let writes = Writes::default();
    let chip: Box<dyn SoundChip> = Box::new(RecordingChip {
        chip: Chip::Ym2151,
        writes: writes.clone(),
    });
    let mut route = ChipCapture::new(chip);
    assert!(matches!(
        route.chip_mut().write(Ym2612Spec {
            port: 1,
            register: 0x22,
            value: 3
        }),
        Err(ChipError::ChipMismatch {
            expected: Chip::Ym2612,
            actual: Chip::Ym2151
        })
    ));
    assert!(writes.borrow().is_empty());

    let mut ymfm = ChipCapture::new(YmfmChip::new(Chip::Ym2151, 4_000_000).unwrap());
    assert!(matches!(
        ymfm.chip_mut().write(Ym2612Spec {
            port: 1,
            register: 0x22,
            value: 3
        }),
        Err(ChipError::ChipMismatch {
            expected: Chip::Ym2612,
            actual: Chip::Ym2151
        })
    ));
    ymfm.chip_mut()
        .write(Ym2151Spec {
            register: 0x20,
            value: 0xc0,
        })
        .unwrap();
}

#[test]
fn typed_ymfm_port_validation_is_preserved() {
    let mut route = ChipCapture::new(YmfmChip::new(Chip::Ym2612, 7_670_454).unwrap());
    route
        .chip_mut()
        .write(Ym2612Spec {
            port: 1,
            register: 0x22,
            value: 8,
        })
        .unwrap();
    assert!(matches!(
        route.chip_mut().write(Ym2612Spec {
            port: 2,
            register: 0x22,
            value: 8
        }),
        Err(ChipError::InvalidRegisterBank { bank: 2, .. })
    ));
}

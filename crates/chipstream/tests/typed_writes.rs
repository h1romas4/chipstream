use std::cell::RefCell;
use std::rc::Rc;

use chipstream::audio::ChipRoute;
use chipstream::chip::{
    C140, C140Type, ChipError, ChipRegisterWrite, ChipWrite, Okim6258, Okim6295, Pwm, SegaPcm,
    Sn76496, Sn76496Config, SoundChip, YmfmChip,
};
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
            ChipRegisterWrite::Pwm(s) => (0, s.register, s.value),
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
    let mut route = ChipRoute::new(RecordingChip {
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
    ChipRoute::new(backend).chip_mut().write(spec).unwrap();
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
fn concrete_pcm_chips_apply_typed_addresses_and_values() {
    let mut sega = ChipRoute::new(SegaPcm::new(4_000_000).unwrap());
    sega.chip_mut()
        .write(SegaPcmSpec {
            offset: 0x185,
            value: 0xa5,
        })
        .unwrap();
    assert_eq!(sega.chip().read_register(0x185), 0xa5);

    let mut c140 = ChipRoute::new(C140::new(12_288_000, C140Type::System2).unwrap());
    c140.chip_mut()
        .write(C140Spec {
            register: 0x103,
            value: 0x7a,
        })
        .unwrap();
    assert_eq!(c140.chip().read_register(0x103), 0x7a);

    let mut oki = ChipRoute::new(Okim6258::new(8_000_000).unwrap());
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

    let mut oki6295 = ChipRoute::new(Okim6295::new(1_000_000).unwrap());
    oki6295
        .chip_mut()
        .write(Okim6295Spec {
            register: 0x0c,
            value: 1,
        })
        .unwrap();
    assert_eq!(oki6295.chip().sample_rate_ratio().denominator().get(), 132);
}

#[test]
fn typed_psg_and_wide_pwm_preserve_output() {
    let config = Sn76496Config {
        stereo: true,
        ..Default::default()
    };
    let mut psg = ChipRoute::new(Sn76496::with_config(3_579_545, config).unwrap());
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

    let mut pwm = ChipRoute::new(Pwm::new(44_100).unwrap());
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

#[test]
fn dynamic_chip_mismatch_is_rejected_before_writing() {
    let writes = Writes::default();
    let chip: Box<dyn SoundChip> = Box::new(RecordingChip {
        chip: Chip::Ym2151,
        writes: writes.clone(),
    });
    let mut route = ChipRoute::new(chip);
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

    let mut ymfm = ChipRoute::new(YmfmChip::new(Chip::Ym2151, 4_000_000).unwrap());
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
    let mut route = ChipRoute::new(YmfmChip::new(Chip::Ym2612, 7_670_454).unwrap());
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

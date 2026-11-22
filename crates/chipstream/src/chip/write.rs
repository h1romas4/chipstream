//! Typed chip register writes and conversions from soundlog VGM commands.
//!
//! [`ChipWrite`] associates a write specification with compatible backends.
//! [`ChipRegisterWrite`] carries supported writes through dynamic dispatch,
//! preserving chip-specific address widths and values.

use soundlog::chip::{
    Ay8910Spec, C140Spec, C352Spec, Chip, Es5503Spec, Ga20Spec, GameGearPsgSpec, GbDmgSpec,
    Huc6280Spec, K053260Spec, K054539Spec, MikeySpec, MultiPcmBankSpec, MultiPcmSpec, NesApuSpec,
    Okim6258Spec, Okim6295Spec, PokeySpec, PsgSpec, PwmSpec, Rf5c68U8Spec, Rf5c68U16Spec,
    Rf5c164U8Spec, Rf5c164U16Spec, Saa1099Spec, Scc1Spec, SegaPcmSpec, Upd7759Spec, VsuSpec,
    WonderSwanRegSpec, WonderSwanSpec, X1010Spec, Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec,
    Ym2608Spec, Ym2610Spec, Ym2612Spec, Ym3526Spec, Ym3812Spec, Ymf262Spec, Ymf278bSpec,
    Ymz280bSpec,
};
use soundlog::vgm::command::{Instance, VgmCommand};

#[cfg(feature = "c140")]
use super::C140;
#[cfg(feature = "c352")]
use super::C352;
#[cfg(feature = "es5503")]
use super::Es5503;
#[cfg(feature = "ga20")]
use super::Ga20;
#[cfg(feature = "gb_dmg")]
use super::GbDmg;
#[cfg(feature = "huc6280")]
use super::Huc6280;
#[cfg(feature = "k051649")]
use super::K051649;
#[cfg(feature = "k053260")]
use super::K053260;
#[cfg(feature = "k054539")]
use super::K054539;
#[cfg(feature = "mikey")]
use super::Mikey;
#[cfg(feature = "multipcm")]
use super::MultiPcm;
#[cfg(feature = "nes_apu")]
use super::NesApu;
#[cfg(feature = "okim6295")]
use super::Okim6295;
#[cfg(feature = "pokey")]
use super::Pokey;
#[cfg(feature = "pwm")]
use super::Pwm;
#[cfg(feature = "rf5c")]
use super::Rf5c;
#[cfg(feature = "saa1099")]
use super::Saa1099;
#[cfg(feature = "segapcm")]
use super::SegaPcm;
#[cfg(feature = "sn76496")]
use super::Sn76496;
#[cfg(feature = "upd7759")]
use super::Upd7759;
#[cfg(feature = "vsu")]
use super::Vsu;
#[cfg(feature = "wonderswan")]
use super::WonderSwan;
#[cfg(feature = "x1010")]
use super::X1010;
#[cfg(feature = "ymz280b")]
use super::Ymz280b;
use super::{ChipError, SoundChip, YmfmChip};
#[cfg(feature = "okim6258")]
use super::{Okim6258, Okim6258Stream};

/// Applies a chip-specific write to a compatible sound chip.
///
/// Concrete chip types implement this only for compatible specifications.
/// `YmfmChip` and dynamically dispatched chips check the chip type at runtime.
/// External chip implementations may implement this trait for their own
/// write types.
pub trait ChipWrite<S>: SoundChip {
    /// Apply a typed write immediately.
    fn apply_typed_write(&mut self, spec: S) -> Result<(), ChipError>;
}

impl<S, C: ChipWrite<S> + ?Sized> ChipWrite<S> for Box<C> {
    /// Forward typed writes through a boxed backend.
    fn apply_typed_write(&mut self, spec: S) -> Result<(), ChipError> {
        (**self).apply_typed_write(spec)
    }
}

macro_rules! define_writes {
    ($( $variant:ident($spec:ty, $chip:ident, |$s:ident| $parts:expr) ),+ $(,)?) => {
        /// A typed register write for a dynamically dispatched sound chip.
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub enum ChipRegisterWrite {
            $(
                #[doc = concat!("Write for ", stringify!($chip), ".")]
                $variant($spec),
            )+
        }

        impl ChipRegisterWrite {
            /// Chip type addressed by this write.
            pub fn chip(&self) -> Chip {
                match self { $(Self::$variant(_) => Chip::$chip,)+ }
            }

            /// Extract bank, register, and value from the typed write.
            pub(crate) fn parts(&self) -> (u8, u8, u32) {
                match self { $(Self::$variant($s) => $parts,)+ }
            }

            /// Extract write parts after checking the target chip.
            pub(crate) fn parts_for(&self, actual: &Chip) -> Result<(u8, u8, u32), ChipError> {
                let expected = self.chip();
                if &expected != actual {
                    return Err(ChipError::ChipMismatch {
                        expected,
                        actual: actual.clone(),
                    });
                }
                Ok(self.parts())
            }

            /// Extract write parts and reject values that do not fit in a byte.
            pub(crate) fn byte_parts_for(&self, actual: &Chip) -> Result<(u8, u8, u8), ChipError> {
                let (bank, register, value) = self.parts_for(actual)?;
                let value = u8::try_from(value).map_err(|_| ChipError::InvalidRegisterValue {
                    chip: actual.clone(),
                    register,
                    value,
                })?;
                Ok((bank, register, value))
            }
        }

        $(
            impl ChipWrite<$spec> for dyn SoundChip + '_ {
                /// Validate the target chip and apply the typed write.
                fn apply_typed_write(&mut self, spec: $spec) -> Result<(), ChipError> {
                    let write = ChipRegisterWrite::$variant(spec);
                    let expected = write.chip();
                    if self.chip_type() != &expected {
                        return Err(ChipError::ChipMismatch {
                            expected,
                            actual: self.chip_type().clone(),
                        });
                    }
                    self.apply_register_write(write)
                }
            }
        )+
    };
}

define_writes! {
    Psg(PsgSpec, Sn76489, |s| (0, 0, u32::from(s.value))),
    GameGearPsg(GameGearPsgSpec, Sn76489, |s| (0, 1, u32::from(s.value))),
    SegaPcm(SegaPcmSpec, SegaPcm, |s| {
        let [bank, register] = s.offset.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    C140(C140Spec, C140, |s| {
        let [bank, register] = s.register.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    C352(C352Spec, C352, |s| {
        let [bank, register] = s.register.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    Es5503(Es5503Spec, Es5503, |s| {
        let [bank, register] = s.register.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    Ga20(Ga20Spec, Ga20, |s| (0, s.register, u32::from(s.value))),
    GbDmg(GbDmgSpec, GbDmg, |s| (0, s.register, u32::from(s.value))),
    Vsu(VsuSpec, Vsu, |s| {
        let [bank, register] = s.offset.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    WonderSwan(WonderSwanSpec, WonderSwan, |s| {
        let [bank, register] = s.offset.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    WonderSwanReg(WonderSwanRegSpec, WonderSwan, |s| (0, s.register, u32::from(s.value))),
    K053260(K053260Spec, K053260, |s| (0, s.register, u32::from(s.value))),
    K054539(K054539Spec, K054539, |s| {
        let [bank, register] = s.register.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    X1010(X1010Spec, X1010, |s| {
        let [bank, register] = s.offset.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    Ymz280b(Ymz280bSpec, Ymz280b, |s| (0, s.register, u32::from(s.value))),
    MultiPcm(MultiPcmSpec, MultiPcm, |s| (0, s.register, u32::from(s.value))),
    MultiPcmBank(MultiPcmBankSpec, MultiPcm, |s| (s.channel, 0, u32::from(s.bank_offset))),
    Mikey(MikeySpec, Mikey, |s| (0, s.register, u32::from(s.value))),
    NesApu(NesApuSpec, NesApu, |s| (0, s.register, u32::from(s.value))),
    Upd7759(Upd7759Spec, Upd7759, |s| (0, s.register, u32::from(s.value))),
    Pwm(PwmSpec, Pwm, |s| (0, s.register, s.value)),
    Rf5c68U8(Rf5c68U8Spec, Rf5c68, |s| (0, s.offset, u32::from(s.value))),
    Rf5c164U8(Rf5c164U8Spec, Rf5c164, |s| (0, s.offset, u32::from(s.value))),
    Rf5c68U16(Rf5c68U16Spec, Rf5c68, |s| {
        let [bank, register] = s.offset.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    Rf5c164U16(Rf5c164U16Spec, Rf5c164, |s| {
        let [bank, register] = s.offset.to_be_bytes();
        (bank, register, u32::from(s.value))
    }),
    Saa1099(Saa1099Spec, Saa1099, |s| (0, s.register, u32::from(s.value))),
    Scc1(Scc1Spec, K051649, |s| (s.port, s.register, u32::from(s.value))),
    Huc6280(Huc6280Spec, Huc6280, |s| (0, s.register, u32::from(s.value))),
    Pokey(PokeySpec, Pokey, |s| (0, s.register, u32::from(s.value))),
    Ay8910(Ay8910Spec, Ay8910, |s| (0, s.register, u32::from(s.value))),
    Okim6258(Okim6258Spec, Okim6258, |s| (0, s.register, u32::from(s.value))),
    Okim6295(Okim6295Spec, Okim6295, |s| (0, s.register, u32::from(s.value))),
    Ym2413(Ym2413Spec, Ym2413, |s| (0, s.register, u32::from(s.value))),
    Ym2151(Ym2151Spec, Ym2151, |s| (0, s.register, u32::from(s.value))),
    Ym2203(Ym2203Spec, Ym2203, |s| (0, s.register, u32::from(s.value))),
    Ym3812(Ym3812Spec, Ym3812, |s| (0, s.register, u32::from(s.value))),
    Ym3526(Ym3526Spec, Ym3526, |s| (0, s.register, u32::from(s.value))),
    Y8950(Y8950Spec, Y8950, |s| (0, s.register, u32::from(s.value))),
    Ym2612(Ym2612Spec, Ym2612, |s| (s.port, s.register, u32::from(s.value))),
    Ym2608(Ym2608Spec, Ym2608, |s| (s.port, s.register, u32::from(s.value))),
    Ym2610(Ym2610Spec, Ym2610b, |s| (s.port, s.register, u32::from(s.value))),
    Ymf262(Ymf262Spec, Ymf262, |s| (s.port, s.register, u32::from(s.value))),
    Ymf278b(Ymf278bSpec, Ymf278b, |s| (s.port, s.register, u32::from(s.value))),
}

impl ChipRegisterWrite {
    /// Convert a supported VGM write command into its typed chip write.
    pub(crate) fn from_vgm_command(command: &VgmCommand) -> Option<(Instance, Self)> {
        use VgmCommand::*;
        Some(match command {
            Sn76489Write(i, s) => (*i, Self::Psg(s.clone())),
            GameGearPsgWrite(i, s) => (*i, Self::GameGearPsg(s.clone())),
            SegaPcmWrite(i, s) => (*i, Self::SegaPcm(s.clone())),
            C140Write(i, s) => (*i, Self::C140(s.clone())),
            C352Write(i, s) => (*i, Self::C352(s.clone())),
            Es5503Write(i, s) => (*i, Self::Es5503(s.clone())),
            Ga20Write(i, s) => (*i, Self::Ga20(s.clone())),
            GbDmgWrite(i, s) => (*i, Self::GbDmg(s.clone())),
            VsuWrite(i, s) => (*i, Self::Vsu(s.clone())),
            WonderSwanWrite(i, s) => (*i, Self::WonderSwan(s.clone())),
            WonderSwanRegWrite(i, s) => (*i, Self::WonderSwanReg(s.clone())),
            K053260Write(i, s) => (*i, Self::K053260(s.clone())),
            K054539Write(i, s) => (*i, Self::K054539(s.clone())),
            X1010Write(i, s) => (*i, Self::X1010(s.clone())),
            Ymz280bWrite(i, s) => (*i, Self::Ymz280b(s.clone())),
            MultiPcmWrite(i, s) => (*i, Self::MultiPcm(s.clone())),
            MultiPcmBankWrite(i, s) => (*i, Self::MultiPcmBank(s.clone())),
            MikeyWrite(i, s) => (*i, Self::Mikey(s.clone())),
            NesApuWrite(i, s) => (*i, Self::NesApu(s.clone())),
            Upd7759Write(i, s) => (*i, Self::Upd7759(s.clone())),
            PwmWrite(i, s) => (*i, Self::Pwm(s.clone())),
            Rf5c68U8Write(i, s) => (*i, Self::Rf5c68U8(s.clone())),
            Rf5c68U16Write(i, s) => (*i, Self::Rf5c68U16(s.clone())),
            Rf5c164U8Write(i, s) => (*i, Self::Rf5c164U8(s.clone())),
            Rf5c164U16Write(i, s) => (*i, Self::Rf5c164U16(s.clone())),
            Saa1099Write(i, s) => (*i, Self::Saa1099(s.clone())),
            Scc1Write(i, s) => (*i, Self::Scc1(s.clone())),
            Huc6280Write(i, s) => (*i, Self::Huc6280(s.clone())),
            PokeyWrite(i, s) => (*i, Self::Pokey(s.clone())),
            Ay8910Write(i, s) => (*i, Self::Ay8910(s.clone())),
            Ym2413Write(i, s) => (*i, Self::Ym2413(s.clone())),
            Ym2612Write(i, s) => (*i, Self::Ym2612(s.clone())),
            Ym2151Write(i, s) => (*i, Self::Ym2151(s.clone())),
            Ym2203Write(i, s) => (*i, Self::Ym2203(s.clone())),
            Ym2608Write(i, s) => (*i, Self::Ym2608(s.clone())),
            Ym2610bWrite(i, s) => (*i, Self::Ym2610(s.clone())),
            Ym3812Write(i, s) => (*i, Self::Ym3812(s.clone())),
            Ym3526Write(i, s) => (*i, Self::Ym3526(s.clone())),
            Y8950Write(i, s) => (*i, Self::Y8950(s.clone())),
            Ymf262Write(i, s) => (*i, Self::Ymf262(s.clone())),
            Ymf278bWrite(i, s) => (*i, Self::Ymf278b(s.clone())),
            Okim6258Write(i, s) => (*i, Self::Okim6258(s.clone())),
            Okim6295Write(i, s) => (*i, Self::Okim6295(s.clone())),
            _ => return None,
        })
    }
}

macro_rules! typed_write {
    ($backend:ty, $spec:ty, $variant:ident) => {
        impl ChipWrite<$spec> for $backend {
            /// Convert the specification to a dynamic write and apply it.
            fn apply_typed_write(&mut self, spec: $spec) -> Result<(), ChipError> {
                self.apply_register_write(ChipRegisterWrite::$variant(spec))
            }
        }
    };
}

#[cfg(feature = "sn76496")]
typed_write!(Sn76496, PsgSpec, Psg);
#[cfg(feature = "sn76496")]
typed_write!(Sn76496, GameGearPsgSpec, GameGearPsg);
#[cfg(feature = "segapcm")]
typed_write!(SegaPcm, SegaPcmSpec, SegaPcm);
#[cfg(feature = "c140")]
typed_write!(C140, C140Spec, C140);
#[cfg(feature = "c352")]
typed_write!(C352, C352Spec, C352);
#[cfg(feature = "es5503")]
typed_write!(Es5503, Es5503Spec, Es5503);
#[cfg(feature = "gb_dmg")]
typed_write!(GbDmg, GbDmgSpec, GbDmg);
#[cfg(feature = "vsu")]
typed_write!(Vsu, VsuSpec, Vsu);
#[cfg(feature = "wonderswan")]
typed_write!(WonderSwan, WonderSwanSpec, WonderSwan);
#[cfg(feature = "wonderswan")]
typed_write!(WonderSwan, WonderSwanRegSpec, WonderSwanReg);
#[cfg(feature = "ga20")]
typed_write!(Ga20, Ga20Spec, Ga20);
#[cfg(feature = "k053260")]
typed_write!(K053260, K053260Spec, K053260);
#[cfg(feature = "k054539")]
typed_write!(K054539, K054539Spec, K054539);
#[cfg(feature = "x1010")]
typed_write!(X1010, X1010Spec, X1010);
#[cfg(feature = "ymz280b")]
typed_write!(Ymz280b, Ymz280bSpec, Ymz280b);
#[cfg(feature = "multipcm")]
typed_write!(MultiPcm, MultiPcmSpec, MultiPcm);
#[cfg(feature = "multipcm")]
typed_write!(MultiPcm, MultiPcmBankSpec, MultiPcmBank);
#[cfg(feature = "mikey")]
typed_write!(Mikey, MikeySpec, Mikey);
#[cfg(feature = "nes_apu")]
typed_write!(NesApu, NesApuSpec, NesApu);
#[cfg(feature = "upd7759")]
typed_write!(Upd7759, Upd7759Spec, Upd7759);
#[cfg(feature = "pwm")]
typed_write!(Pwm, PwmSpec, Pwm);
#[cfg(feature = "rf5c")]
typed_write!(Rf5c, Rf5c68U8Spec, Rf5c68U8);
#[cfg(feature = "rf5c")]
typed_write!(Rf5c, Rf5c68U16Spec, Rf5c68U16);
#[cfg(feature = "rf5c")]
typed_write!(Rf5c, Rf5c164U8Spec, Rf5c164U8);
#[cfg(feature = "rf5c")]
typed_write!(Rf5c, Rf5c164U16Spec, Rf5c164U16);
#[cfg(feature = "saa1099")]
typed_write!(Saa1099, Saa1099Spec, Saa1099);
#[cfg(feature = "k051649")]
typed_write!(K051649, Scc1Spec, Scc1);
#[cfg(feature = "huc6280")]
typed_write!(Huc6280, Huc6280Spec, Huc6280);
#[cfg(feature = "pokey")]
typed_write!(Pokey, PokeySpec, Pokey);
#[cfg(feature = "okim6258")]
typed_write!(Okim6258, Okim6258Spec, Okim6258);
#[cfg(feature = "okim6258")]
typed_write!(Okim6258Stream, Okim6258Spec, Okim6258);
#[cfg(feature = "okim6295")]
typed_write!(Okim6295, Okim6295Spec, Okim6295);
typed_write!(YmfmChip, Ym2413Spec, Ym2413);
typed_write!(YmfmChip, Ay8910Spec, Ay8910);
typed_write!(YmfmChip, Ym2151Spec, Ym2151);
typed_write!(YmfmChip, Ym2203Spec, Ym2203);
typed_write!(YmfmChip, Ym3812Spec, Ym3812);
typed_write!(YmfmChip, Ym3526Spec, Ym3526);
typed_write!(YmfmChip, Y8950Spec, Y8950);
typed_write!(YmfmChip, Ym2612Spec, Ym2612);
typed_write!(YmfmChip, Ym2608Spec, Ym2608);
typed_write!(YmfmChip, Ym2610Spec, Ym2610);
typed_write!(YmfmChip, Ymf262Spec, Ymf262);
typed_write!(YmfmChip, Ymf278bSpec, Ymf278b);

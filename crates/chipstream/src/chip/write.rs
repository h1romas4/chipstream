use soundlog::chip::{
    C140Spec, Chip, GameGearPsgSpec, Okim6258Spec, Okim6295Spec, PsgSpec, PwmSpec, SegaPcmSpec,
    Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec, Ym2612Spec, Ym3526Spec,
    Ym3812Spec, Ymf262Spec, Ymf278bSpec,
};
use soundlog::vgm::command::{Instance, VgmCommand};

#[cfg(feature = "c140")]
use super::C140;
#[cfg(feature = "okim6258")]
use super::Okim6258;
#[cfg(feature = "okim6295")]
use super::Okim6295;
#[cfg(feature = "pwm")]
use super::Pwm;
#[cfg(feature = "segapcm")]
use super::SegaPcm;
#[cfg(feature = "sn76496")]
use super::Sn76496;
use super::{ChipError, SoundChip, YmfmChip};

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

            pub(crate) fn parts(&self) -> (u8, u8, u32) {
                match self { $(Self::$variant($s) => $parts,)+ }
            }

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
    Pwm(PwmSpec, Pwm, |s| (0, s.register, s.value)),
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
    pub(crate) fn from_vgm_command(command: &VgmCommand) -> Option<(Instance, Self)> {
        use VgmCommand::*;
        Some(match command {
            Sn76489Write(i, s) => (*i, Self::Psg(s.clone())),
            GameGearPsgWrite(i, s) => (*i, Self::GameGearPsg(s.clone())),
            SegaPcmWrite(i, s) => (*i, Self::SegaPcm(s.clone())),
            C140Write(i, s) => (*i, Self::C140(s.clone())),
            PwmWrite(i, s) => (*i, Self::Pwm(s.clone())),
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
#[cfg(feature = "pwm")]
typed_write!(Pwm, PwmSpec, Pwm);
#[cfg(feature = "okim6258")]
typed_write!(Okim6258, Okim6258Spec, Okim6258);
#[cfg(feature = "okim6295")]
typed_write!(Okim6295, Okim6295Spec, Okim6295);
typed_write!(YmfmChip, Ym2413Spec, Ym2413);
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

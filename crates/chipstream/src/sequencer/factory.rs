//! User-supplied chip factories for replacing built-in sequencer backends.

use std::collections::HashMap;

use soundlog::chip::Chip;
use soundlog::vgm::command::Instance;

use crate::chip::{ChipError, SoundChip};

/// AY-family chip model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ay8910Variant {
    /// AY-3-8910.
    Ay8910,
    /// AY-3-8912.
    Ay8912,
    /// AY-3-8913.
    Ay8913,
    /// AY-3-8930.
    Ay8930,
    /// AY-3-8914.
    Ay8914,
    /// YM2149.
    Ym2149,
    /// YM3439.
    Ym3439,
    /// YMZ284.
    Ymz284,
    /// YMZ294.
    Ymz294,
    /// An unrecognized model identifier.
    Unknown(u8),
}

/// AY-family model and output configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ay8910Settings {
    /// Requested chip model.
    pub variant: Ay8910Variant,
    /// Output and clock configuration.
    pub output: AyOutputSettings,
}

/// AY-compatible PSG output and clock configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AyOutputSettings {
    /// Legacy output mode.
    pub legacy_output: bool,
    /// Single output mode.
    pub single_output: bool,
    /// Separate channel outputs.
    pub discrete_output: bool,
    /// Raw output mode.
    pub raw_output: bool,
    /// YM-family pin 26 is low.
    pub ym_pin26_low: bool,
    /// Unrecognized source flags.
    pub reserved_flags: u8,
}

impl Ay8910Settings {
    /// Return the clock-divider setting for supported YM2149 approximations.
    ///
    /// `None` means the model or output configuration cannot be represented.
    pub fn ym2149_clock_divider(&self) -> Option<bool> {
        let ym = matches!(
            self.variant,
            Ay8910Variant::Ym2149
                | Ay8910Variant::Ym3439
                | Ay8910Variant::Ymz284
                | Ay8910Variant::Ymz294
        );
        if (!ym
            && !matches!(
                self.variant,
                Ay8910Variant::Ay8910 | Ay8910Variant::Ay8912 | Ay8910Variant::Ay8913
            ))
            || self.output.discrete_output
            || self.output.raw_output
            || self.output.reserved_flags != 0
        {
            return None;
        }
        Some(ym && self.output.ym_pin26_low)
    }
}

/// SN76489-compatible PSG configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sn76489Settings {
    /// Noise shift-register width after applying source defaults.
    pub shift_width: u8,
    /// Noise feedback mask after applying source defaults.
    pub feedback: u16,
    /// Negate generated output.
    pub negate: bool,
    /// Enable Game Gear stereo.
    pub stereo: bool,
    /// The source uses a non-default stereo mask for this instance.
    ///
    /// For VGM this is true only when stereo is enabled and a mask other
    /// than `0xff` appears before end-of-data.
    pub stereo_used: bool,
    /// Enable the input clock's divide-by-eight stage.
    pub divider_8: bool,
    /// Treat a zero tone period as 1024.
    pub frequency_zero_is_1024: bool,
    /// Use XNOR noise feedback.
    pub xnor: bool,
    /// The source identifies the unsupported T6W28 variant.
    pub t6w28: bool,
    /// Unrecognized source flags.
    pub reserved_flags: u8,
}

#[cfg(feature = "sn76496")]
impl From<Sn76489Settings> for crate::chip::Sn76496Config {
    fn from(settings: Sn76489Settings) -> Self {
        Self {
            shift_width: settings.shift_width,
            feedback: settings.feedback,
            negate: settings.negate,
            stereo: settings.stereo,
            divider_8: settings.divider_8,
            frequency_zero_is_1024: settings.frequency_zero_is_1024,
            xnor: settings.xnor,
        }
    }
}

/// OKIM6258 clock divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Okim6258Divider {
    /// Divide the input clock by 1024.
    Div1024,
    /// Divide the input clock by 768.
    Div768,
    /// Divide the input clock by 512.
    Div512,
}

/// OKIM6258 source configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Okim6258Settings {
    /// Initial clock divider.
    pub divider: Okim6258Divider,
    /// The source requests 3-bit ADPCM.
    pub adpcm_3bit: bool,
    /// Use 12-bit rather than 10-bit output.
    pub output_12bit: bool,
    /// Unrecognized source flags.
    pub reserved_flags: u8,
}

/// C140 board variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C140Variant {
    /// Namco System 2.
    System2,
    /// Namco System 21.
    System21,
    /// Namco ASIC219.
    Asic219,
    /// An unrecognized board identifier.
    Unknown(u8),
}

/// Chip-specific settings supplied to a backend factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipBackendSettings {
    /// No additional settings for this chip.
    Standard,
    /// AY-family model and output settings.
    Ay8910(Ay8910Settings),
    /// SN76489-family settings.
    Sn76489(Sn76489Settings),
    /// OKIM6258 settings.
    Okim6258(Okim6258Settings),
    /// OKIM6295 pin 7 selection.
    Okim6295 {
        /// Initial state of the sample-rate selection pin.
        pin7_high: bool,
    },
    /// C140 board variant.
    C140(C140Variant),
    /// SegaPCM banking configuration.
    SegaPcm {
        /// Shift applied to the bank bits in the channel control register.
        bank_shift: u8,
        /// Mask selecting the bank bits in the channel control register.
        bank_mask: u8,
    },
    /// YM2203 embedded PSG settings.
    Ym2203 {
        /// Embedded PSG output configuration.
        ssg: AyOutputSettings,
    },
    /// YM2608 embedded PSG settings.
    Ym2608 {
        /// Embedded PSG output configuration.
        ssg: AyOutputSettings,
    },
    /// YM2610 variant selection.
    Ym2610 {
        /// Select the YM2610B variant when true.
        is_b: bool,
    },
}

/// Configuration passed to a registered sound-chip factory.
pub struct ChipFactoryConfig {
    /// Chip requested by the source.
    chip: Chip,
    /// Primary or secondary instance.
    instance: Instance,
    /// Effective input clock in Hz.
    clock: u32,
    /// Sequencer output rate in Hz.
    output_rate: u32,
    /// Chip-specific header settings.
    settings: ChipBackendSettings,
    /// Original OKIM6258 clock, when present in the source.
    source_okim6258_clock: Option<u32>,
}

impl ChipFactoryConfig {
    /// Assemble the effective settings passed to a factory.
    pub(super) fn new(
        chip: Chip,
        instance: Instance,
        clock: u32,
        output_rate: u32,
        settings: ChipBackendSettings,
        source_okim6258_clock: Option<u32>,
    ) -> Self {
        Self {
            chip,
            instance,
            clock,
            output_rate,
            settings,
            source_okim6258_clock,
        }
    }

    /// Chip type selected by the VGM/MDX playback source.
    pub fn chip(&self) -> &Chip {
        &self.chip
    }

    /// Chip instance to create.
    pub fn instance(&self) -> Instance {
        self.instance
    }

    /// Effective input clock in Hz, including format-specific clock conversion.
    pub fn clock(&self) -> u32 {
        self.clock
    }

    /// Sequencer output rate in frames per second.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Settings for this chip, independent of the input format.
    pub fn settings(&self) -> &ChipBackendSettings {
        &self.settings
    }

    /// AY-family settings, if this factory is creating an AY-family chip.
    pub fn ay8910_settings(&self) -> Option<&Ay8910Settings> {
        match &self.settings {
            ChipBackendSettings::Ay8910(settings) => Some(settings),
            _ => None,
        }
    }

    /// Source-declared OKIM6258 clock, for hardware requiring a timing stream.
    ///
    /// For VGM this is the primary header clock before extra-header overrides.
    pub fn source_okim6258_clock(&self) -> Option<u32> {
        self.source_okim6258_clock
    }
}

type Factory = dyn FnMut(&ChipFactoryConfig) -> Result<Option<Box<dyn SoundChip>>, ChipError>;

/// Factories that replace built-in backends for selected chip types.
///
/// Registrations are keyed by [`Chip`], so YMFM-backed chip types such as
/// YM2151 and YM2612 can be replaced independently. Unregistered chip types
/// continue to use their built-in backend.
#[derive(Default)]
pub struct ChipFactoryRegistry {
    factories: HashMap<Chip, Box<Factory>>,
}

impl ChipFactoryRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    pub(super) fn contains(&self, chip: &Chip) -> bool {
        self.factories.contains_key(chip)
    }

    /// Register a replacement factory for one chip type.
    ///
    /// The factory is called once for each present instance of the selected
    /// chip. Registering the same chip type again replaces the previous
    /// factory.
    pub fn register<F>(&mut self, chip: Chip, mut factory: F) -> &mut Self
    where
        F: FnMut(&ChipFactoryConfig) -> Result<Box<dyn SoundChip>, ChipError> + 'static,
    {
        self.register_optional(chip, move |config| factory(config).map(Some))
    }

    /// Register a factory that can defer individual instances to the built-in backend.
    ///
    /// Returning `Ok(None)` explicitly selects the normal sequencer backend for
    /// that instance. Errors are propagated and never cause a fallback.
    /// AY8910 has no default backend, so a declined instance is an error.
    /// Registering the same chip type again replaces the previous factory.
    pub fn register_optional<F>(&mut self, chip: Chip, factory: F) -> &mut Self
    where
        F: FnMut(&ChipFactoryConfig) -> Result<Option<Box<dyn SoundChip>>, ChipError> + 'static,
    {
        self.factories.insert(chip, Box::new(factory));
        self
    }

    /// Add a fallback for instances declined by the existing factory.
    ///
    /// Existing factory errors are propagated; they never select the fallback.
    /// If no factory is registered, the fallback handles all instances.
    pub fn register_fallback<F>(&mut self, chip: Chip, mut fallback: F) -> &mut Self
    where
        F: FnMut(&ChipFactoryConfig) -> Result<Box<dyn SoundChip>, ChipError> + 'static,
    {
        let mut previous = self.factories.remove(&chip);
        self.register_optional(chip, move |config| {
            if let Some(factory) = &mut previous
                && let Some(backend) = factory(config)?
            {
                return Ok(Some(backend));
            }
            fallback(config).map(Some)
        })
    }

    /// Invoke a registered factory and validate its chip type and clock.
    pub(super) fn create(
        &mut self,
        config: &ChipFactoryConfig,
    ) -> Result<Option<Box<dyn SoundChip>>, ChipError> {
        let Some(factory) = self.factories.get_mut(config.chip()) else {
            return Ok(None);
        };
        let Some(chip) = factory(config)? else {
            return Ok(None);
        };
        let actual_chip = chip.chip_type().clone();
        if actual_chip != config.chip {
            return Err(ChipError::ChipMismatch {
                expected: config.chip.clone(),
                actual: actual_chip,
            });
        }
        let actual_clock = chip.clock();
        if actual_clock != config.clock {
            return Err(ChipError::ClockMismatch {
                chip: config.chip.clone(),
                expected: config.clock,
                actual: actual_clock,
            });
        }
        Ok(Some(chip))
    }
}

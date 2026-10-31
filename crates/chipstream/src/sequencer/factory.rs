use std::collections::HashMap;

use soundlog::chip::Chip;
use soundlog::vgm::command::Instance;
use soundlog::vgm::header::{VgmExtraHeader, VgmHeader};

use crate::chip::{ChipError, SoundChip};

/// Configuration passed to a registered sound-chip factory.
pub struct ChipFactoryConfig<'a> {
    chip: Chip,
    instance: Instance,
    clock: u32,
    output_rate: u32,
    header: &'a VgmHeader,
    extra_header: Option<&'a VgmExtraHeader>,
}

impl<'a> ChipFactoryConfig<'a> {
    pub(super) fn new(
        chip: Chip,
        instance: Instance,
        clock: u32,
        output_rate: u32,
        header: &'a VgmHeader,
        extra_header: Option<&'a VgmExtraHeader>,
    ) -> Self {
        Self {
            chip,
            instance,
            clock,
            output_rate,
            header,
            extra_header,
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

    /// Parsed VGM header, or the synthesized header used for MDX playback.
    pub fn header(&self) -> &VgmHeader {
        self.header
    }

    /// Optional VGM extra header.
    pub fn extra_header(&self) -> Option<&VgmExtraHeader> {
        self.extra_header
    }
}

type Factory = dyn for<'a> FnMut(&ChipFactoryConfig<'a>) -> Result<Box<dyn SoundChip>, ChipError>;

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

    /// Register a replacement factory for one chip type.
    ///
    /// The factory is called once for each present instance of the selected
    /// chip. Registering the same chip type again replaces the previous
    /// factory.
    pub fn register<F>(&mut self, chip: Chip, factory: F) -> &mut Self
    where
        F: for<'a> FnMut(&ChipFactoryConfig<'a>) -> Result<Box<dyn SoundChip>, ChipError> + 'static,
    {
        self.factories.insert(chip, Box::new(factory));
        self
    }

    pub(super) fn create(
        &mut self,
        config: &ChipFactoryConfig<'_>,
    ) -> Result<Option<Box<dyn SoundChip>>, ChipError> {
        let Some(factory) = self.factories.get_mut(config.chip()) else {
            return Ok(None);
        };
        let chip = factory(config)?;
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

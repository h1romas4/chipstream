use std::{cell::RefCell, num::NonZeroU32, rc::Rc};

use ::ndsif::{RegisterWrite, WriteId};
use soundlog::chip::Chip;
use soundlog::vgm::command::Instance;

use crate::{
    chip::{ChipError, ChipRegisterWrite, SampleRate, SoundChip},
    sequencer::{ChipFactoryConfig, ChipFactoryRegistry},
};

/// One timed register write for an NDSIF transport to schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NdsifRegisterEvent {
    pub chip: Chip,
    pub write_id: WriteId,
    pub instance: u8,
    pub page: u8,
    pub write: RegisterWrite,
    pub sample_frame: usize,
    pub sample_rate: u32,
}

/// Schedules chip writes on an NDSIF-capable device.
///
/// Implementations must queue writes for `sample_frame`; they must not treat
/// this call as an instruction to write to the device immediately. Sequencer
/// rendering is ahead of playback, and events from different chip instances
/// may arrive out of chronological order.
pub trait NdsifTransport {
    /// Configure a chip instance before playback.
    fn configure_chip(
        &mut self,
        write_id: WriteId,
        instance: u8,
        clock_hz: u32,
    ) -> Result<(), String>;

    /// Queue a register write at its absolute output-frame position.
    fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String>;
}

/// NDSIF-backed YMFM replacement that emits silence to the host PCM path.
///
/// The physical chip produces the audible output. Register events are handed
/// to the injected transport with absolute output-frame positions.
///
/// Untimed calls to [`SoundChip::apply_register_write`] are scheduled at frame
/// zero. Sequencer playback uses [`SoundChip::apply_register_write_at`] and
/// supplies the correct absolute frame.
pub struct NdsifSoundChip<T> {
    chip: Chip,
    instance: u8,
    clock: u32,
    output_rate: u32,
    sample_rate: SampleRate,
    channels: usize,
    transport: Rc<RefCell<T>>,
}

impl<T: NdsifTransport> NdsifSoundChip<T> {
    /// Configure a physical chip and create its scheduled sound-chip adapter.
    pub fn new(
        config: &ChipFactoryConfig<'_>,
        transport: Rc<RefCell<T>>,
    ) -> Result<Self, ChipError> {
        let chip = config.chip().clone();
        let Some((divisor, channels, write_id)) = ymfm_config(&chip) else {
            return Err(ChipError::UnsupportedChip(chip));
        };
        if config.clock() < divisor {
            return Err(ChipError::InvalidClock {
                clock: config.clock(),
                minimum: divisor,
            });
        }
        let instance = instance_id(config.instance());
        let write_id = write_id_for(write_id, &chip)?;
        transport
            .try_borrow_mut()
            .map_err(|_| external_error(&chip, "NDSIF transport is already borrowed"))?
            .configure_chip(write_id, instance, config.clock())
            .map_err(|error| external_error(&chip, error))?;

        Ok(Self {
            chip,
            instance,
            clock: config.clock(),
            output_rate: config.output_rate(),
            sample_rate: SampleRate::from_ratio(
                config.clock(),
                NonZeroU32::new(divisor).expect("YMFM sample-rate divisors are nonzero"),
            ),
            channels,
            transport,
        })
    }

    fn schedule_write(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        sample_rate: u32,
    ) -> Result<(), ChipError> {
        let actual_chip = write.chip();
        if actual_chip != self.chip {
            return Err(ChipError::ChipMismatch {
                expected: self.chip.clone(),
                actual: actual_chip,
            });
        }
        let (write_id, page, address, value) = register_parts(&write)?;
        let event = NdsifRegisterEvent {
            chip: self.chip.clone(),
            write_id: write_id_for(write_id, &self.chip)?,
            instance: self.instance,
            page,
            write: RegisterWrite::new(address, value),
            sample_frame,
            sample_rate,
        };
        self.transport
            .try_borrow_mut()
            .map_err(|_| external_error(&self.chip, "NDSIF transport is already borrowed"))?
            .schedule_register_write(event)
            .map_err(|error| external_error(&self.chip, error))
    }
}

impl<T: NdsifTransport> SoundChip for NdsifSoundChip<T> {
    fn chip_type(&self) -> &Chip {
        &self.chip
    }

    fn clock(&self) -> u32 {
        self.clock
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate.floor_hz()
    }

    fn sample_rate_ratio(&self) -> SampleRate {
        self.sample_rate
    }

    fn channels(&self) -> usize {
        self.channels
    }

    fn apply_register_write(&mut self, write: ChipRegisterWrite) -> Result<(), ChipError> {
        self.schedule_write(write, 0, self.output_rate)
    }

    fn apply_register_write_at(
        &mut self,
        write: ChipRegisterWrite,
        sample_frame: usize,
        output_rate: u32,
    ) -> Result<(), ChipError> {
        self.schedule_write(write, sample_frame, output_rate)
    }

    fn generate(&mut self, samples: &mut [i32]) -> Result<(), ChipError> {
        if !samples.len().is_multiple_of(self.channels) {
            return Err(ChipError::InvalidBufferLength {
                length: samples.len(),
                channels: self.channels,
            });
        }
        samples.fill(0);
        Ok(())
    }

    fn reset(&mut self) {}
}

/// Register one YMFM chip type with an NDSIF transport.
pub fn register_ymfm_factory<T: NdsifTransport + 'static>(
    registry: &mut ChipFactoryRegistry,
    chip: Chip,
    transport: Rc<RefCell<T>>,
) -> Result<(), ChipError> {
    if ymfm_config(&chip).is_none() {
        return Err(ChipError::UnsupportedChip(chip));
    }
    registry.register(chip, move |config| {
        Ok(Box::new(NdsifSoundChip::new(
            config,
            Rc::clone(&transport),
        )?))
    });
    Ok(())
}

/// Register factories for all YMFM-backed chip types.
pub fn register_ymfm_factories<T: NdsifTransport + 'static>(
    registry: &mut ChipFactoryRegistry,
    transport: Rc<RefCell<T>>,
) -> Result<(), ChipError> {
    for chip in [
        Chip::Ym2413,
        Chip::Ym2151,
        Chip::Ym2203,
        Chip::Ym2608,
        Chip::Ym2610b,
        Chip::Ym2612,
        Chip::Ym3526,
        Chip::Y8950,
        Chip::Ym3812,
        Chip::Ymf262,
        Chip::Ymf278b,
    ] {
        register_ymfm_factory(registry, chip, Rc::clone(&transport))?;
    }
    Ok(())
}

fn ymfm_config(chip: &Chip) -> Option<(u32, usize, u8)> {
    Some(match chip {
        Chip::Ym2413 => (72, 2, 0x51),
        Chip::Ym2151 => (64, 2, 0x54),
        Chip::Ym2203 => (4, 4, 0x55),
        Chip::Ym2608 => (8, 3, 0x56),
        Chip::Ym2610b => (16, 3, 0x58),
        Chip::Ym2612 => (144, 2, 0x52),
        Chip::Ym3526 => (72, 1, 0x5b),
        Chip::Y8950 => (72, 1, 0x5c),
        Chip::Ym3812 => (72, 1, 0x5a),
        Chip::Ymf262 => (288, 4, 0x5e),
        Chip::Ymf278b => (768, 6, 0xd0),
        _ => return None,
    })
}

fn register_parts(write: &ChipRegisterWrite) -> Result<(u8, u8, u8, u8), ChipError> {
    let chip = write.chip();
    let invalid_bank = |bank| ChipError::InvalidRegisterBank {
        chip: chip.clone(),
        bank,
    };
    Ok(match write {
        ChipRegisterWrite::Ym2413(spec) => (0x51, 0, spec.register, spec.value),
        ChipRegisterWrite::Ym2151(spec) => (0x54, 0, spec.register, spec.value),
        ChipRegisterWrite::Ym2203(spec) => (0x55, 0, spec.register, spec.value),
        ChipRegisterWrite::Ym2608(spec) => (
            match spec.port {
                0 => 0x56,
                1 => 0x57,
                bank => return Err(invalid_bank(bank)),
            },
            0,
            spec.register,
            spec.value,
        ),
        ChipRegisterWrite::Ym2610(spec) => (
            match spec.port {
                0 => 0x58,
                1 => 0x59,
                bank => return Err(invalid_bank(bank)),
            },
            0,
            spec.register,
            spec.value,
        ),
        ChipRegisterWrite::Ym2612(spec) => (
            match spec.port {
                0 => 0x52,
                1 => 0x53,
                bank => return Err(invalid_bank(bank)),
            },
            0,
            spec.register,
            spec.value,
        ),
        ChipRegisterWrite::Ym3526(spec) => (0x5b, 0, spec.register, spec.value),
        ChipRegisterWrite::Y8950(spec) => (0x5c, 0, spec.register, spec.value),
        ChipRegisterWrite::Ym3812(spec) => (0x5a, 0, spec.register, spec.value),
        ChipRegisterWrite::Ymf262(spec) => (
            match spec.port {
                0 => 0x5e,
                1 => 0x5f,
                bank => return Err(invalid_bank(bank)),
            },
            0,
            spec.register,
            spec.value,
        ),
        ChipRegisterWrite::Ymf278b(spec) if spec.port <= 2 => {
            (0xd0, spec.port, spec.register, spec.value)
        }
        ChipRegisterWrite::Ymf278b(spec) => return Err(invalid_bank(spec.port)),
        _ => return Err(ChipError::UnsupportedChip(chip)),
    })
}

fn write_id_for(write_id: u8, chip: &Chip) -> Result<WriteId, ChipError> {
    WriteId::try_from(write_id).map_err(|error| external_error(chip, error.to_string()))
}

fn instance_id(instance: Instance) -> u8 {
    match instance {
        Instance::Primary => 0,
        Instance::Secondary => 1,
    }
}

fn external_error(chip: &Chip, message: impl Into<String>) -> ChipError {
    ChipError::External {
        chip: chip.clone(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundlog::chip::{
        Y8950Spec, Ym2151Spec, Ym2203Spec, Ym2413Spec, Ym2608Spec, Ym2610Spec, Ym2612Spec,
        Ym3526Spec, Ym3812Spec, Ymf262Spec, Ymf278bSpec,
    };

    #[test]
    fn maps_ymfm_writes_to_ndsif_ids_and_pages() {
        let writes = [
            (
                ChipRegisterWrite::Ym2413(Ym2413Spec {
                    register: 1,
                    value: 2,
                }),
                (0x51, 0),
            ),
            (
                ChipRegisterWrite::Ym2151(Ym2151Spec {
                    register: 1,
                    value: 2,
                }),
                (0x54, 0),
            ),
            (
                ChipRegisterWrite::Ym2203(Ym2203Spec {
                    register: 1,
                    value: 2,
                }),
                (0x55, 0),
            ),
            (
                ChipRegisterWrite::Ym2608(Ym2608Spec {
                    port: 0,
                    register: 1,
                    value: 2,
                }),
                (0x56, 0),
            ),
            (
                ChipRegisterWrite::Ym2608(Ym2608Spec {
                    port: 1,
                    register: 1,
                    value: 2,
                }),
                (0x57, 0),
            ),
            (
                ChipRegisterWrite::Ym2610(Ym2610Spec {
                    port: 0,
                    register: 1,
                    value: 2,
                }),
                (0x58, 0),
            ),
            (
                ChipRegisterWrite::Ym2610(Ym2610Spec {
                    port: 1,
                    register: 1,
                    value: 2,
                }),
                (0x59, 0),
            ),
            (
                ChipRegisterWrite::Ym2612(Ym2612Spec {
                    port: 0,
                    register: 1,
                    value: 2,
                }),
                (0x52, 0),
            ),
            (
                ChipRegisterWrite::Ym2612(Ym2612Spec {
                    port: 1,
                    register: 1,
                    value: 2,
                }),
                (0x53, 0),
            ),
            (
                ChipRegisterWrite::Ym3526(Ym3526Spec {
                    register: 1,
                    value: 2,
                }),
                (0x5b, 0),
            ),
            (
                ChipRegisterWrite::Y8950(Y8950Spec {
                    register: 1,
                    value: 2,
                }),
                (0x5c, 0),
            ),
            (
                ChipRegisterWrite::Ym3812(Ym3812Spec {
                    register: 1,
                    value: 2,
                }),
                (0x5a, 0),
            ),
            (
                ChipRegisterWrite::Ymf262(Ymf262Spec {
                    port: 0,
                    register: 1,
                    value: 2,
                }),
                (0x5e, 0),
            ),
            (
                ChipRegisterWrite::Ymf262(Ymf262Spec {
                    port: 1,
                    register: 1,
                    value: 2,
                }),
                (0x5f, 0),
            ),
            (
                ChipRegisterWrite::Ymf278b(Ymf278bSpec {
                    port: 2,
                    register: 1,
                    value: 2,
                }),
                (0xd0, 2),
            ),
        ];

        for (write, expected) in writes {
            let (write_id, page, address, value) = register_parts(&write).unwrap();
            assert_eq!((write_id, page), expected);
            assert_eq!((address, value), (1, 2));
        }
    }
}

//! Device selection and chipstream factory registration.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use chipstream::chip::{
    ChipError, ClockDivider, NdsifOkim6258, NdsifSn76489, NdsifSoundChip, Okim6258, OutputBits,
    Sn76496, Sn76496Config, YmfmChip,
};
use chipstream::sequencer::{Chip, ChipFactoryRegistry, Instance};
use ndsif::WriteId;

use crate::{NdsifSerialTransport, NdsifTransportError};

/// Serial connection and reset options for an NDSIF device.
#[derive(Debug, Clone)]
pub struct NdsifSerialConfig {
    /// Path to the serial device.
    pub port: String,
    /// Serial baud rate.
    pub baud_rate: u32,
    /// Timeout for each request/response transaction.
    pub response_timeout: Duration,
    /// Reset the device before querying capabilities.
    pub reset_device: bool,
}

/// Open configured NDSIF devices and register hardware-backed factories.
///
/// Each chip instance is routed to the first configured device advertising a
/// compatible chip/clock capability. When no device matches, the normal
/// emulated backend is retained. An empty list returns an empty registry. All
/// configurations are validated before any device is opened or reset.
///
/// # Errors
///
/// Returns a typed configuration error, or a [`NdsifTransportError::Device`]
/// identifying the port and preserving the initialization failure.
pub fn create_chip_factories(
    configs: &[NdsifSerialConfig],
) -> Result<ChipFactoryRegistry, NdsifTransportError> {
    let mut ports = HashSet::with_capacity(configs.len());
    for config in configs {
        validate_config(&config.port, config.baud_rate, config.response_timeout)?;
        if !ports.insert(&config.port) {
            return Err(NdsifTransportError::DuplicatePort {
                port: config.port.clone(),
            });
        }
    }
    let mut transports = Vec::with_capacity(configs.len());
    for config in configs {
        transports.push(Rc::new(RefCell::new(NdsifSerialTransport::open(
            &config.port,
            config.baud_rate,
            config.response_timeout,
            config.reset_device,
        )?)));
    }

    let transports = Rc::new(transports);
    let mut factories = ChipFactoryRegistry::new();

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
        let Some(write_id) = ymfm_write_id(&chip) else {
            continue;
        };
        let registered_chip = chip.clone();
        let chip_transports = Rc::clone(&transports);
        factories.register(chip, move |config| {
            let instance = instance_number(config.instance());
            let oki_clock = if registered_chip == Chip::Ym2151 {
                config.header().okim6258_clock & 0x3fff_ffff
            } else {
                0
            };
            let oki_timing_clock = (oki_clock != 0).then_some(oki_clock);
            let transport = find_transport(
                &chip_transports,
                &registered_chip,
                write_id,
                instance,
                config.clock(),
                oki_timing_clock,
            )?;
            if let Some(transport) = transport {
                Ok(Box::new(NdsifSoundChip::new(config, transport)?))
            } else {
                Ok(Box::new(YmfmChip::new(
                    registered_chip.clone(),
                    config.clock(),
                )?))
            }
        });
    }

    let psg_transports = Rc::clone(&transports);
    factories.register(Chip::Sn76489, move |config| {
        let flags = &config.header().sn76489_flags;
        if config.header().misc().t6w28_detected == Some(true) {
            return Err(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "T6W28 PSG is not supported",
            });
        }
        let write_id = match config.instance() {
            Instance::Primary => WriteId::SN76489,
            Instance::Secondary => WriteId::SN76489_SECOND,
        };
        let instance = instance_number(config.instance());
        let transport = if flags.gamegear_stereo {
            None
        } else {
            find_transport(
                &psg_transports,
                &Chip::Sn76489,
                write_id,
                instance,
                config.clock(),
                None,
            )?
        };
        if let Some(transport) = transport {
            return Ok(Box::new(NdsifSn76489::new(config, transport)?));
        }

        let feedback = u16::from(config.header().sn76489_feedback);
        let width = u8::from(config.header().sn76489_shift_register_width);
        Ok(Box::new(Sn76496::with_config(
            config.clock(),
            Sn76496Config {
                feedback: if feedback == 0 { 0x0009 } else { feedback },
                shift_width: if width == 0 { 16 } else { width },
                negate: flags.output_negate,
                stereo: flags.gamegear_stereo,
                divider_8: flags.clock_divider_8,
                frequency_zero_is_1024: flags.frequency_0_is_400,
                xnor: flags.xnor_noise_mode,
            },
        )?))
    });

    let oki_transports = Rc::clone(&transports);
    factories.register(Chip::Okim6258, move |config| {
        let instance = instance_number(config.instance());
        if let Some(transport) = find_transport(
            &oki_transports,
            &Chip::Okim6258,
            WriteId::OKIM6258,
            instance,
            config.clock(),
            Some(config.clock()),
        )? {
            return Ok(Box::new(NdsifOkim6258::new(config, transport)?));
        }

        let flags = &config.header().okim6258_flags;
        let mut chip = Okim6258::new_for_vgm(config.clock())?;
        chip.set_divider(match flags.clock_divider {
            0 => ClockDivider::Div1024,
            1 => ClockDivider::Div768,
            _ => ClockDivider::Div512,
        })?;
        chip.set_output_bits(if flags.output_12bit {
            OutputBits::Bits12
        } else {
            OutputBits::Bits10
        });
        Ok(Box::new(chip))
    });

    Ok(factories)
}

/// Reject invalid serial settings before opening any device.
pub(super) fn validate_config(
    path: &str,
    baud_rate: u32,
    timeout: Duration,
) -> Result<(), NdsifTransportError> {
    if path.trim().is_empty() {
        return Err(NdsifTransportError::EmptyPort);
    }
    if baud_rate == 0 {
        return Err(NdsifTransportError::InvalidBaudRate {
            port: path.to_owned(),
        });
    }
    if timeout.is_zero() {
        return Err(NdsifTransportError::InvalidTimeout {
            port: path.to_owned(),
        });
    }
    Ok(())
}

/// Find the first configured device supporting the requested chip and clock.
fn find_transport(
    transports: &[Rc<RefCell<NdsifSerialTransport>>],
    chip: &Chip,
    write_id: WriteId,
    instance: u8,
    clock_hz: u32,
    oki_timing_clock: Option<u32>,
) -> Result<Option<Rc<RefCell<NdsifSerialTransport>>>, ChipError> {
    for transport in transports {
        let device = transport.try_borrow().map_err(|_| ChipError::External {
            chip: chip.clone(),
            message: "NDSIF transport is already borrowed".to_owned(),
        })?;
        if !device.supports_clock(write_id, instance, clock_hz) {
            continue;
        }
        if oki_timing_clock.is_some_and(|clock| {
            !device.supports_clock(WriteId::OKIM6258, 0, clock)
                || !device.supports_audio_stream(0, clock)
        }) {
            continue;
        }
        return Ok(Some(Rc::clone(transport)));
    }
    Ok(None)
}

/// Map supported YMFM chip types to their NDSIF write identifiers.
fn ymfm_write_id(chip: &Chip) -> Option<WriteId> {
    let write_id = match chip {
        Chip::Ym2413 => 0x51,
        Chip::Ym2151 => 0x54,
        Chip::Ym2203 => 0x55,
        Chip::Ym2608 => 0x56,
        Chip::Ym2610b => 0x58,
        Chip::Ym2612 => 0x52,
        Chip::Ym3526 => 0x5b,
        Chip::Y8950 => 0x5c,
        Chip::Ym3812 => 0x5a,
        Chip::Ymf262 => 0x5e,
        Chip::Ymf278b => 0xd0,
        _ => return None,
    };
    WriteId::try_from(write_id).ok()
}

/// Convert a sequencer instance to its protocol instance number.
fn instance_number(instance: Instance) -> u8 {
    match instance {
        Instance::Primary => 0,
        Instance::Secondary => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_ymfm_chips_to_ndsif_write_ids() {
        for (chip, expected) in [
            (Chip::Ym2413, 0x51),
            (Chip::Ym2151, 0x54),
            (Chip::Ym2203, 0x55),
            (Chip::Ym2608, 0x56),
            (Chip::Ym2610b, 0x58),
            (Chip::Ym2612, 0x52),
            (Chip::Ym3526, 0x5b),
            (Chip::Y8950, 0x5c),
            (Chip::Ym3812, 0x5a),
            (Chip::Ymf262, 0x5e),
            (Chip::Ymf278b, 0xd0),
        ] {
            assert_eq!(ymfm_write_id(&chip).map(WriteId::get), Some(expected));
        }
        assert_eq!(ymfm_write_id(&Chip::Sn76489), None);
    }
}

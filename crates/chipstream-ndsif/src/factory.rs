//! Device selection and chipstream factory registration.
//!
//! Automatic factories select the first compatible transport for each chip
//! instance, with software fallback where available. Route policies can
//! validate the selected hardware resources before the backend is created.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use chipstream::chip::{ChipError, Sn76496, YmfmChip};
use chipstream::sequencer::{
    Chip, ChipBackendSettings, ChipFactoryConfig, ChipFactoryRegistry, Instance,
};
use ndsif::WriteId;
use ndsif_transport::NdsifSerialConfig;

use crate::{
    NdsifDirectPolicy, NdsifNanoDrivePolicy, NdsifOkim6258, NdsifRoute, NdsifRouteOptions,
    NdsifRoutePolicy, NdsifRoutingTransport, NdsifSerialTransport, NdsifSn76489, NdsifSoundChip,
    NdsifTransportError,
};

/// Open configured NDSIF devices and register hardware-backed factories.
///
/// Each chip instance is routed to the first configured device advertising a
/// compatible chip/clock capability. Unmatched chips use the normal software
/// backend where available. AY8910 is silent by default without a factory;
/// when an AY device is present, an unroutable AY instance is an error.
/// T6W28 and SN76489 with Game Gear stereo masks use emulation. An empty list
/// selects no hardware. All configurations are validated before any
/// device is opened or reset, although opening devices is not transactional.
/// Devices advertising YM2151 are identified to apply NanoDrive 8 pan correction.
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
        config.validate()?;
        if !ports.insert(&config.port) {
            return Err(NdsifTransportError::DuplicatePort {
                port: config.port.clone(),
            });
        }
    }
    let mut transports = Vec::with_capacity(configs.len());
    let mut nd8_devices = Vec::new();
    for config in configs {
        let mut transport = NdsifSerialTransport::open(config.clone())?;
        if transport
            .capabilities()
            .iter()
            .any(|entry| entry.write_id == 0x54)
        {
            let info = transport
                .device_info()
                .map_err(|source| NdsifTransportError::Device {
                    port: config.port.clone(),
                    source: Box::new(source),
                })?;
            if info.model.trim().is_empty() {
                return Err(NdsifTransportError::Device {
                    port: config.port.clone(),
                    source: Box::new(NdsifTransportError::UnexpectedResponse {
                        operation: "GET_INFO (empty model name)",
                    }),
                });
            }
            if info.model.trim() == "NanoDrive 8" {
                nd8_devices.push(transports.len());
            }
        }
        transports.push(Rc::new(RefCell::new(transport)));
    }

    let supports_ay = transports.iter().any(|transport| {
        transport
            .borrow()
            .capabilities()
            .iter()
            .any(|capability| capability.write_id == 0xa0)
    });
    Ok(create_automatic_transport_factories_with_policy(
        &transports,
        supports_ay,
        NdsifNanoDrivePolicy::new([], nd8_devices, transports.len())
            .expect("discovered device indices are valid"),
    ))
}

/// Build automatic chip factories using scheduled transports with cached capabilities.
///
/// `supports_ay` must indicate whether any transport advertises AY8910.
/// Unmatched chips use their normal software backend where available; AY8910
/// is silent by default without a factory. Non-default Game Gear stereo masks
/// and T6W28 use SN76489 emulation. Capability checks and chip configuration
/// occur when the sequencer creates each instance.
pub fn create_automatic_transport_factories<T: NdsifRoutingTransport + 'static>(
    transports: &[Rc<RefCell<T>>],
    supports_ay: bool,
) -> ChipFactoryRegistry {
    create_automatic_transport_factories_with_policy(transports, supports_ay, NdsifDirectPolicy)
}

/// Build automatic factories with a policy for selected hardware routes.
///
/// The policy is called for each selected hardware instance and shared across
/// factories. It can validate device resources and select AY compatibility.
/// Rejected selections return an error rather than falling back. `supports_ay`
/// must indicate whether any transport advertises AY8910.
pub fn create_automatic_transport_factories_with_policy<T: NdsifRoutingTransport + 'static>(
    transports: &[Rc<RefCell<T>>],
    supports_ay: bool,
    policy: impl NdsifRoutePolicy + 'static,
) -> ChipFactoryRegistry {
    let transports = Rc::new(transports.to_vec());
    let policy = Rc::new(RefCell::new(policy));
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
        let Some(write_id) = chip_write_id(&chip) else {
            continue;
        };
        let registered_chip = chip.clone();
        let chip_transports = Rc::clone(&transports);
        let policy = Rc::clone(&policy);
        factories.register(chip, move |config| {
            let instance = instance_number(config.instance());
            let oki_timing_clock = if registered_chip == Chip::Ym2151 {
                config.source_okim6258_clock()
            } else {
                None
            };
            let transport = find_transport(
                &chip_transports,
                &registered_chip,
                write_id,
                instance,
                config.clock(),
                oki_timing_clock,
            )?;
            if let Some((device, transport)) = transport {
                let options = select_route(&policy, device, config)?;
                Ok(Box::new(NdsifSoundChip::with_target_options(
                    config,
                    config.instance(),
                    transport,
                    options,
                )?))
            } else {
                Ok(Box::new(YmfmChip::new(
                    registered_chip.clone(),
                    config.clock(),
                )?))
            }
        });
    }

    if supports_ay {
        let ay_transports = Rc::clone(&transports);
        let policy = Rc::clone(&policy);
        factories.register(Chip::Ay8910, move |config| {
            let (device, transport) = find_transport(
                &ay_transports,
                &Chip::Ay8910,
                WriteId::try_from(0xa0).expect("AY8910 write ID"),
                instance_number(config.instance()),
                config.clock(),
                None,
            )?
            .ok_or(ChipError::InvalidConfiguration {
                chip: Chip::Ay8910,
                reason: "no NDSIF AY8910 target supports this instance and clock",
            })?;
            let options = select_route(&policy, device, config)?;
            Ok(Box::new(NdsifSoundChip::with_target_options(
                config,
                config.instance(),
                transport,
                options,
            )?))
        });
    }

    let psg_transports = Rc::clone(&transports);
    let psg_policy = Rc::clone(&policy);
    factories.register_optional(Chip::Sn76489, move |config| {
        let ChipBackendSettings::Sn76489(settings) = *config.settings() else {
            return Err(ChipError::UnsupportedChip(config.chip().clone()));
        };
        if settings.t6w28 {
            return Ok(None);
        }
        let write_id = match config.instance() {
            Instance::Primary => WriteId::SN76489,
            Instance::Secondary => WriteId::SN76489_SECOND,
        };
        let instance = instance_number(config.instance());
        let transport = if settings.stereo_used {
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
        if let Some((device, transport)) = transport {
            select_route(&psg_policy, device, config)?;
            return Ok(Some(Box::new(NdsifSn76489::new(config, transport)?)));
        }

        Ok(Some(Box::new(Sn76496::with_config(
            config.clock(),
            settings.into(),
        )?)))
    });

    let oki_transports = Rc::clone(&transports);
    let oki_policy = Rc::clone(&policy);
    factories.register_optional(Chip::Okim6258, move |config| {
        let instance = instance_number(config.instance());
        if let Some((device, transport)) = find_transport(
            &oki_transports,
            &Chip::Okim6258,
            WriteId::OKIM6258,
            instance,
            config.clock(),
            Some(config.clock()),
        )? {
            select_route(&oki_policy, device, config)?;
            return Ok(Some(Box::new(NdsifOkim6258::new(config, transport)?)));
        }
        Ok(None)
    });

    factories
}

/// Ask the policy for options after assembling the active source route.
fn select_route(
    policy: &RefCell<impl NdsifRoutePolicy>,
    device: usize,
    config: &ChipFactoryConfig,
) -> Result<NdsifRouteOptions, ChipError> {
    let route = NdsifRoute {
        chip: config.chip().clone(),
        source: config.instance(),
        device,
        target: config.instance(),
    };
    let error = |message| ChipError::External {
        chip: config.chip().clone(),
        message,
    };
    policy
        .try_borrow_mut()
        .map_err(|_| error("NDSIF route policy is already borrowed".to_owned()))?
        .select(&route, config.clock())
        .map_err(error)
}

/// Optional selected device index and shared transport handle.
type SelectedTransport<T> = Option<(usize, Rc<RefCell<T>>)>;

/// Find the first configured device supporting the requested chip and clock.
fn find_transport<T: NdsifRoutingTransport>(
    transports: &[Rc<RefCell<T>>],
    chip: &Chip,
    write_id: WriteId,
    instance: u8,
    clock_hz: u32,
    oki_timing_clock: Option<u32>,
) -> Result<SelectedTransport<T>, ChipError> {
    for (index, transport) in transports.iter().enumerate() {
        let device = transport.try_borrow().map_err(|_| ChipError::External {
            chip: chip.clone(),
            message: "NDSIF transport is already borrowed".to_owned(),
        })?;
        if !device.supports_route_clock(write_id, instance, clock_hz) {
            continue;
        }
        if oki_timing_clock.is_some_and(|clock| {
            !device.supports_route_clock(WriteId::OKIM6258, 0, clock)
                || !device.supports_route_audio(0, clock)
        }) {
            continue;
        }
        return Ok(Some((index, Rc::clone(transport))));
    }
    Ok(None)
}

/// Map supported chip types to their NDSIF write identifiers.
pub(super) fn chip_write_id(chip: &Chip) -> Option<WriteId> {
    let write_id = match chip {
        Chip::Ym2413 => 0x51,
        Chip::Ay8910 => 0xa0,
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
pub(super) fn instance_number(instance: Instance) -> u8 {
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
            assert_eq!(chip_write_id(&chip).map(WriteId::get), Some(expected));
        }
        assert_eq!(chip_write_id(&Chip::Sn76489), None);
    }
}

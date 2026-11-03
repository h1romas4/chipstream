//! Explicit chip-instance routing for applications with saved connections.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use chipstream::chip::{
    ChipError, NdsifOkim6258, NdsifSn76489, NdsifSoundChip, NdsifTransport, SoundChip,
};
use chipstream::sequencer::{Chip, ChipFactoryRegistry, Instance};
use ndsif::WriteId;

use crate::NdsifSerialTransport;
use crate::factory::{chip_write_id, instance_number};

/// One source chip instance connected to a physical device instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NdsifRoute {
    /// Chip type on both ends of the connection.
    pub chip: Chip,
    /// Instance in the VGM or MDX source.
    pub source: Instance,
    /// Index into the device vector supplied to [`create_routed_chip_factories`].
    pub device: usize,
    /// Instance advertised by the selected device.
    pub target: Instance,
}

/// Invalid routing configuration detected before chip construction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NdsifRoutingError {
    /// The route refers to a device index outside the supplied device list.
    UnknownDevice(usize),
    /// Two device indices refer to the same shared transport.
    DuplicateDevice { first: usize, second: usize },
    /// The chip has no NDSIF adapter.
    UnsupportedChip(Chip),
    /// A source has more than one destination.
    DuplicateSource { chip: Chip, instance: Instance },
    /// More than one source targets the same physical chip instance.
    DuplicateTarget {
        device: usize,
        chip: Chip,
        instance: Instance,
    },
}

impl fmt::Display for NdsifRoutingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownDevice(index) => write!(f, "unknown NDSIF device index {index}"),
            Self::DuplicateDevice { first, second } => {
                write!(
                    f,
                    "NDSIF device indices {first} and {second} refer to the same device"
                )
            }
            Self::UnsupportedChip(chip) => write!(f, "NDSIF cannot route {chip:?}"),
            Self::DuplicateSource { chip, instance } => {
                write!(f, "duplicate NDSIF source {chip:?} {instance:?}")
            }
            Self::DuplicateTarget {
                device,
                chip,
                instance,
            } => {
                write!(f, "duplicate NDSIF target {device}: {chip:?} {instance:?}")
            }
        }
    }
}

impl std::error::Error for NdsifRoutingError {}

/// Build factories for explicit routes using already-opened serial devices.
///
/// Unrouted instances use their normal built-in backend, except AY8910, which
/// has no built-in backend. If any AY instance is routed, an unrouted AY
/// instance in the same track is an error. Routed instances
/// never fall back: an unsupported clock, stream or device capability produces
/// a chip error when the sequencer creates the instance. Devices are retained
/// by the registry and its created chips.
///
/// # Errors
///
/// Returns an error for an unknown device, unsupported chip, or conflicting
/// source or target. Clock and stream compatibility depend on the input file
/// and are checked when the sequencer constructs its chip instances.
pub fn create_routed_chip_factories(
    devices: Vec<NdsifSerialTransport>,
    routes: &[NdsifRoute],
) -> Result<ChipFactoryRegistry, NdsifRoutingError> {
    let devices = devices
        .into_iter()
        .map(|device| Rc::new(RefCell::new(device)))
        .collect();
    routed_factories(devices, routes)
}

/// Build explicit route factories from shared, already-open serial devices.
///
/// The returned factories and their chips retain clones of the handles.
/// After dropping the sequencer and every factory, the caller can regain
/// exclusive ownership of each transport with [`Rc::try_unwrap`] and reset it
/// with [`NdsifSerialTransport::reset_for_reuse`]. Do not reset a transport
/// while a factory or sequencer is still using it.
///
/// # Errors
///
/// Returns an error if two indices refer to the same transport or if routes
/// are invalid. Clock and stream compatibility are checked when the sequencer
/// constructs the chip instances.
pub fn create_shared_routed_chip_factories(
    devices: &[Rc<RefCell<NdsifSerialTransport>>],
    routes: &[NdsifRoute],
) -> Result<ChipFactoryRegistry, NdsifRoutingError> {
    routed_factories(devices.to_vec(), routes)
}

/// A scheduled transport with cached route-capability checks.
///
/// The checks must not perform I/O. They are used before a routed chip is
/// constructed; configuration and write errors still propagate separately.
pub trait NdsifRoutingTransport: NdsifTransport {
    /// Return whether the target supports the requested chip clock.
    fn supports_route_clock(&self, write_id: WriteId, instance: u8, clock: u32) -> bool;
    /// Return whether the target supports an OKIM6258 audio stream.
    fn supports_route_audio(&self, instance: u8, clock: u32) -> bool;
}

impl NdsifRoutingTransport for NdsifSerialTransport {
    fn supports_route_clock(&self, write_id: WriteId, instance: u8, clock: u32) -> bool {
        self.supports_clock(write_id, instance, clock)
    }

    fn supports_route_audio(&self, instance: u8, clock: u32) -> bool {
        self.supports_audio_stream(instance, clock)
    }
}

/// Build explicit route factories from shared scheduled transports.
///
/// This supports transports that queue operations for a separate device
/// worker. The caller must ensure queued work is delivered in order and that
/// the physical target matches the capabilities reported by each transport.
///
/// # Errors
///
/// Returns an error for aliased devices or invalid routes. Clock and stream
/// compatibility are checked when the sequencer constructs its chip instances.
pub fn create_routed_transport_factories<T: NdsifRoutingTransport + 'static>(
    devices: &[Rc<RefCell<T>>],
    routes: &[NdsifRoute],
) -> Result<ChipFactoryRegistry, NdsifRoutingError> {
    routed_factories(devices.to_vec(), routes)
}

fn route_write_id(chip: &Chip, target: Instance) -> Option<WriteId> {
    match chip {
        Chip::Sn76489 => Some(match target {
            Instance::Primary => WriteId::SN76489,
            Instance::Secondary => WriteId::SN76489_SECOND,
        }),
        Chip::Okim6258 => Some(WriteId::OKIM6258),
        _ => chip_write_id(chip),
    }
}

fn validate_routes(device_count: usize, routes: &[NdsifRoute]) -> Result<(), NdsifRoutingError> {
    for (index, route) in routes.iter().enumerate() {
        if route.device >= device_count {
            return Err(NdsifRoutingError::UnknownDevice(route.device));
        }
        if route_write_id(&route.chip, route.target).is_none() {
            return Err(NdsifRoutingError::UnsupportedChip(route.chip.clone()));
        }
        for previous in &routes[..index] {
            if previous.chip == route.chip && previous.source == route.source {
                return Err(NdsifRoutingError::DuplicateSource {
                    chip: route.chip.clone(),
                    instance: route.source,
                });
            }
            if previous.device == route.device
                && previous.chip == route.chip
                && previous.target == route.target
            {
                return Err(NdsifRoutingError::DuplicateTarget {
                    device: route.device,
                    chip: route.chip.clone(),
                    instance: route.target,
                });
            }
        }
    }
    Ok(())
}

fn routed_factories<T: NdsifRoutingTransport + 'static>(
    devices: Vec<Rc<RefCell<T>>>,
    routes: &[NdsifRoute],
) -> Result<ChipFactoryRegistry, NdsifRoutingError> {
    for (second, device) in devices.iter().enumerate() {
        if let Some(first) = devices[..second]
            .iter()
            .position(|other| Rc::ptr_eq(device, other))
        {
            return Err(NdsifRoutingError::DuplicateDevice { first, second });
        }
    }
    validate_routes(devices.len(), routes)?;
    let devices = Rc::new(devices);
    let mut factories = ChipFactoryRegistry::new();
    let mut registered = Vec::new();
    for route in routes {
        if registered.contains(&route.chip) {
            continue;
        }
        registered.push(route.chip.clone());
        let chip = route.chip.clone();
        let chip_routes: Vec<_> = routes
            .iter()
            .filter(|route| route.chip == chip)
            .cloned()
            .collect();
        let devices = Rc::clone(&devices);
        factories.register_optional(chip.clone(), move |config| {
            let Some(route) = chip_routes
                .iter()
                .find(|route| route.source == config.instance())
            else {
                return Ok(None);
            };
            let transport = Rc::clone(&devices[route.device]);
            let instance = instance_number(route.target);
            let write_id = route_write_id(&chip, route.target).expect("validated NDSIF chip type");
            let invalid = |message: String| ChipError::External {
                chip: chip.clone(),
                message,
            };
            {
                let device = transport
                    .try_borrow()
                    .map_err(|_| invalid("NDSIF transport is already borrowed".into()))?;
                if !device.supports_route_clock(write_id, instance, config.clock()) {
                    return Err(invalid(format!(
                        "device {} does not support {chip:?} instance {instance} at {} Hz",
                        route.device,
                        config.clock()
                    )));
                }
                if chip == Chip::Okim6258 && !device.supports_route_audio(instance, config.clock())
                {
                    return Err(invalid(format!(
                        "device {} does not support the requested OKIM6258 stream",
                        route.device
                    )));
                }
                let oki_clock = if chip == Chip::Ym2151 {
                    config.header().okim6258_clock & 0x3fff_ffff
                } else {
                    0
                };
                if oki_clock != 0
                    && (!device.supports_route_clock(WriteId::OKIM6258, 0, oki_clock)
                        || !device.supports_route_audio(0, oki_clock))
                {
                    return Err(invalid(format!(
                        "device {} does not support the paired OKIM6258 timing stream at {oki_clock} Hz",
                        route.device
                    )));
                }
            }
            let backend: Box<dyn SoundChip> = match chip {
                Chip::Sn76489 => {
                    if config.header().misc().t6w28_detected == Some(true)
                        || config.header().sn76489_flags.gamegear_stereo
                    {
                        return Err(invalid("NDSIF does not support this PSG format".into()));
                    }
                    Box::new(NdsifSn76489::with_target_instance(
                        config,
                        route.target,
                        transport,
                    )?)
                }
                Chip::Okim6258 => Box::new(NdsifOkim6258::with_target_instance(
                    config,
                    route.target,
                    transport,
                )?),
                _ => Box::new(NdsifSoundChip::with_target_instance(
                    config,
                    route.target,
                    transport,
                )?),
            };
            Ok(Some(backend))
        });
    }
    Ok(factories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipstream::chip::NdsifRegisterEvent;
    use chipstream::sequencer::{VgmSequencer, VgmSequencerError};
    use soundlog::chip::{Ay8910Spec, Okim6258Spec, Ym2151Spec};
    use soundlog::vgm::{VgmBuilder, command::WaitSamples};

    #[derive(Default)]
    struct Device {
        supported: bool,
        audio_supported: bool,
        configured: Vec<(WriteId, u8, u32)>,
        events: Vec<NdsifRegisterEvent>,
    }

    impl NdsifTransport for Device {
        fn configure_chip(&mut self, id: WriteId, instance: u8, clock: u32) -> Result<(), String> {
            self.configured.push((id, instance, clock));
            Ok(())
        }

        fn schedule_register_write(&mut self, event: NdsifRegisterEvent) -> Result<(), String> {
            self.events.push(event);
            Ok(())
        }
    }

    impl NdsifRoutingTransport for Device {
        fn supports_route_clock(&self, _: WriteId, _: u8, _: u32) -> bool {
            self.supported
        }

        fn supports_route_audio(&self, _: u8, _: u32) -> bool {
            self.audio_supported
        }
    }

    fn route(source: Instance, device: usize, target: Instance) -> NdsifRoute {
        NdsifRoute {
            chip: Chip::Ym2151,
            source,
            device,
            target,
        }
    }

    fn dual_vgm() -> Vec<u8> {
        let mut builder = VgmBuilder::new();
        for instance in [Instance::Primary, Instance::Secondary] {
            builder.register_chip(Chip::Ym2151, instance, 4_000_000);
            builder.add_chip_write(
                instance,
                Ym2151Spec {
                    register: 0x20,
                    value: 0xc7,
                },
            );
        }
        builder.add_vgm_command(WaitSamples(441));
        builder.finalize().into()
    }

    fn dual_ay_vgm() -> Vec<u8> {
        let mut builder = VgmBuilder::new();
        for instance in [Instance::Primary, Instance::Secondary] {
            builder.register_chip(Chip::Ay8910, instance, 2_000_000);
            builder.add_chip_write(
                instance,
                Ay8910Spec {
                    register: 7,
                    value: 0x38,
                },
            );
        }
        builder.add_vgm_command(WaitSamples(441));
        builder.finalize().into()
    }

    #[test]
    fn routes_ay_instances_and_remaps_target_instances() {
        let device = Rc::new(RefCell::new(Device {
            supported: true,
            ..Default::default()
        }));
        let routes = [
            NdsifRoute {
                chip: Chip::Ay8910,
                source: Instance::Primary,
                device: 0,
                target: Instance::Secondary,
            },
            NdsifRoute {
                chip: Chip::Ay8910,
                source: Instance::Secondary,
                device: 0,
                target: Instance::Primary,
            },
        ];
        let factories = routed_factories(vec![Rc::clone(&device)], &routes).unwrap();
        let mut sequencer =
            VgmSequencer::from_vgm_with_factories(dual_ay_vgm(), 44_100, 256, factories).unwrap();
        while sequencer.render().unwrap().is_some() {}
        let device = device.borrow();
        let ay = WriteId::try_from(0xa0).unwrap();
        assert_eq!(device.configured, [(ay, 1, 2_000_000), (ay, 0, 2_000_000)]);
        assert_eq!(device.events.len(), 2);
        assert_eq!(
            (
                device.events[0].write_id,
                device.events[0].instance,
                device.events[0].page,
                device.events[0].write.address,
                device.events[0].write.value
            ),
            (ay, 1, 0, 7, 0x38)
        );
        assert_eq!(
            (
                device.events[1].write_id,
                device.events[1].instance,
                device.events[1].page,
                device.events[1].write.address,
                device.events[1].write.value
            ),
            (ay, 0, 0, 7, 0x38)
        );
    }

    #[test]
    fn ay_without_factory_is_ignored_but_partial_or_incompatible_routes_fail() {
        let mut sequencer = VgmSequencer::from_vgm_with_factories(
            dual_ay_vgm(),
            44_100,
            256,
            ChipFactoryRegistry::new(),
        )
        .unwrap();
        while sequencer.render().unwrap().is_some() {}
        let route = NdsifRoute {
            chip: Chip::Ay8910,
            source: Instance::Primary,
            device: 0,
            target: Instance::Primary,
        };
        let supported = Rc::new(RefCell::new(Device {
            supported: true,
            ..Default::default()
        }));
        let factories = routed_factories(vec![supported], std::slice::from_ref(&route)).unwrap();
        assert!(matches!(
            VgmSequencer::from_vgm_with_factories(dual_ay_vgm(), 44_100, 256, factories),
            Err(VgmSequencerError::Chip(ChipError::UnsupportedChip(
                Chip::Ay8910
            )))
        ));
        let unsupported = Rc::new(RefCell::new(Device::default()));
        let factories = routed_factories(vec![unsupported], &[route]).unwrap();
        assert!(matches!(
            VgmSequencer::from_vgm_with_factories(dual_ay_vgm(), 44_100, 256, factories),
            Err(VgmSequencerError::Chip(ChipError::External { .. }))
        ));
    }

    #[test]
    fn ay_rejects_unsupported_variants_flags_and_stereo_masks() {
        let route = NdsifRoute {
            chip: Chip::Ay8910,
            source: Instance::Primary,
            device: 0,
            target: Instance::Primary,
        };
        let device = Rc::new(RefCell::new(Device {
            supported: true,
            ..Default::default()
        }));
        for (offset, value) in [
            (0x78, 0x10),
            (0x79, 0x04),
            (0x79, 0x08),
            (0x79, 0x10),
            (0x79, 0x20),
        ] {
            let mut builder = VgmBuilder::new();
            builder.register_chip(Chip::Ay8910, Instance::Primary, 2_000_000);
            builder.add_vgm_command(WaitSamples(441));
            let mut bytes: Vec<u8> = builder.finalize().into();
            bytes[offset] = value;
            let factories =
                routed_factories(vec![Rc::clone(&device)], std::slice::from_ref(&route)).unwrap();
            assert!(matches!(
                VgmSequencer::from_vgm_with_factories(bytes, 44_100, 256, factories),
                Err(VgmSequencerError::Chip(ChipError::InvalidConfiguration {
                    chip: Chip::Ay8910,
                    ..
                }))
            ));
            assert!(device.borrow().configured.is_empty());
        }
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ay8910, Instance::Primary, 2_000_000);
        builder.add_vgm_command(soundlog::vgm::command::Ay8910StereoMask::from_mask(1));
        builder.add_vgm_command(WaitSamples(441));
        let factories = routed_factories(vec![device], &[route]).unwrap();
        let mut sequencer =
            VgmSequencer::from_vgm_with_factories(builder.finalize(), 44_100, 256, factories)
                .unwrap();
        assert!(matches!(
            sequencer.render(),
            Err(VgmSequencerError::Chip(ChipError::InvalidConfiguration {
                chip: Chip::Ay8910,
                ..
            }))
        ));
    }
    #[test]
    fn routes_two_source_instances_to_distinct_devices() {
        let first = Rc::new(RefCell::new(Device {
            supported: true,
            ..Default::default()
        }));
        let second = Rc::new(RefCell::new(Device {
            supported: true,
            ..Default::default()
        }));
        let routes = [
            route(Instance::Primary, 0, Instance::Primary),
            route(Instance::Secondary, 1, Instance::Primary),
        ];
        let factories =
            routed_factories(vec![Rc::clone(&first), Rc::clone(&second)], &routes).unwrap();
        let mut sequencer =
            VgmSequencer::from_vgm_with_factories(dual_vgm(), 44_100, 256, factories).unwrap();
        while sequencer.render().unwrap().is_some() {}
        for device in [&first, &second] {
            let device = device.borrow();
            assert_eq!(device.configured, [(WriteId::YM2151, 0, 4_000_000)]);
            assert_eq!(device.events.len(), 1);
            assert_eq!(device.events[0].instance, 0);
        }
        drop(sequencer);
        assert!(Rc::try_unwrap(first).is_ok());
        assert!(Rc::try_unwrap(second).is_ok());
    }

    #[test]
    fn unrouted_instance_uses_emulation_but_incompatible_route_fails() {
        let first = Rc::new(RefCell::new(Device {
            supported: true,
            ..Default::default()
        }));
        let routes = [route(Instance::Primary, 0, Instance::Secondary)];
        let factories = routed_factories(vec![Rc::clone(&first)], &routes).unwrap();
        let mut sequencer =
            VgmSequencer::from_vgm_with_factories(dual_vgm(), 44_100, 256, factories).unwrap();
        while sequencer.render().unwrap().is_some() {}
        assert_eq!(first.borrow().configured, [(WriteId::YM2151, 1, 4_000_000)]);
        assert_eq!(first.borrow().events.len(), 1);

        let unavailable = Rc::new(RefCell::new(Device::default()));
        let factories = routed_factories(vec![unavailable], &routes).unwrap();
        assert!(matches!(
            VgmSequencer::from_vgm_with_factories(dual_vgm(), 44_100, 256, factories),
            Err(VgmSequencerError::Chip(ChipError::External { .. }))
        ));
    }

    #[test]
    fn rejects_unknown_devices_and_duplicate_sources_or_targets() {
        let primary = route(Instance::Primary, 0, Instance::Primary);
        let secondary = route(Instance::Secondary, 0, Instance::Primary);
        assert_eq!(
            validate_routes(0, &[primary.clone()]),
            Err(NdsifRoutingError::UnknownDevice(0))
        );
        assert!(matches!(
            validate_routes(1, &[primary.clone(), primary.clone()]),
            Err(NdsifRoutingError::DuplicateSource { .. })
        ));
        assert!(matches!(
            validate_routes(1, &[primary, secondary]),
            Err(NdsifRoutingError::DuplicateTarget { .. })
        ));
        let unsupported = NdsifRoute {
            chip: Chip::C140,
            ..route(Instance::Primary, 0, Instance::Primary)
        };
        assert!(matches!(
            validate_routes(1, &[unsupported]),
            Err(NdsifRoutingError::UnsupportedChip(Chip::C140))
        ));
    }

    #[test]
    fn shared_routes_reject_aliased_device_indices() {
        let device = Rc::new(RefCell::new(Device::default()));
        assert!(matches!(
            routed_factories(vec![Rc::clone(&device), device], &[]),
            Err(NdsifRoutingError::DuplicateDevice {
                first: 0,
                second: 1
            })
        ));
    }

    #[test]
    fn paired_ym2151_requires_compatible_oki_stream() {
        let device = Rc::new(RefCell::new(Device {
            supported: true,
            audio_supported: false,
            ..Default::default()
        }));
        let routes = [route(Instance::Primary, 0, Instance::Primary)];
        let factories = routed_factories(vec![device], &routes).unwrap();
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.register_chip(Chip::Okim6258, Instance::Primary, 4_000_000);
        assert!(matches!(
            VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories),
            Err(VgmSequencerError::Chip(ChipError::External { message, .. }))
                if message.contains("paired OKIM6258")
        ));
    }

    #[test]
    fn ym2151_hardware_and_okim6258_emulation_can_play_together() {
        let device = Rc::new(RefCell::new(Device {
            supported: true,
            audio_supported: true,
            ..Default::default()
        }));
        let routes = [route(Instance::Primary, 0, Instance::Primary)];
        let factories = routed_factories(vec![Rc::clone(&device)], &routes).unwrap();
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym2151, Instance::Primary, 4_000_000);
        builder.register_chip(Chip::Okim6258, Instance::Primary, 8_000_000);
        builder.add_chip_write(
            Instance::Primary,
            Ym2151Spec {
                register: 0x20,
                value: 0xc7,
            },
        );
        for (register, value) in [(0x0c, 2), (0x00, 2), (0x01, 0x21)] {
            builder.add_chip_write(Instance::Primary, Okim6258Spec { register, value });
        }
        builder.add_vgm_command(WaitSamples(441));
        let mut sequencer =
            VgmSequencer::new_with_factories(builder.finalize(), 44_100, 256, factories).unwrap();
        let mut has_audio = false;
        while let Some(block) = sequencer.render().unwrap() {
            has_audio |= block.frames().iter().flatten().any(|sample| *sample != 0.0);
        }
        assert!(has_audio, "OKIM6258 emulation must produce PCM");
        let device = device.borrow();
        assert_eq!(device.configured, [(WriteId::YM2151, 0, 4_000_000)]);
        assert_eq!(device.events.len(), 1);
        assert_eq!(device.events[0].write_id, WriteId::YM2151);
    }
}

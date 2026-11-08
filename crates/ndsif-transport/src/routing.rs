//! Explicit chip-instance routing for applications with saved connections.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
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

/// Validate the physical chip and clock resources shared by NanoDrive 7 routes.
#[derive(Default)]
pub struct NdsifNd7Validator {
    opn_slots: HashSet<(usize, bool)>,
    opn_clocks: HashMap<usize, u32>,
    opl_slots: HashSet<usize>,
}

impl NdsifNd7Validator {
    /// Add one active hardware route using its source chip's effective clock.
    ///
    /// # Errors
    ///
    /// Returns an error when the route conflicts with an earlier route or its
    /// physical clock cannot be represented.
    pub fn add(&mut self, route: &NdsifRoute, clock: u32) -> Result<(), String> {
        let (multiplier, opn) = match route.chip {
            Chip::Ay8910 => (2, true),
            Chip::Ym2203 => (1, true),
            Chip::Ym3526 | Chip::Ym3812 => (4, false),
            Chip::Ymf262 => (1, false),
            _ => return Ok(()),
        };
        let physical = clock
            .checked_mul(multiplier)
            .ok_or_else(|| "NanoDrive 7 clock overflow".to_owned())?;
        if opn {
            let slot = (route.device, route.target == Instance::Secondary);
            if self.opn_slots.contains(&slot) {
                return Err("NanoDrive 7 AY8910 and YM2203 routes share the same physical chip; change the routing before playback".into());
            }
            let physical = match physical {
                3_579_000..=3_580_000 => 3_579_545,
                3_993_600 => 4_000_000,
                _ => physical,
            };
            if let Some(&previous) = self.opn_clocks.get(&route.device)
                && previous != physical
            {
                return Err(format!(
                    "NanoDrive 7 AY8910 and YM2203 routes require different shared clocks ({previous} Hz and {physical} Hz)"
                ));
            }
            self.opn_slots.insert(slot);
            self.opn_clocks.insert(route.device, physical);
        } else if !self.opl_slots.insert(route.device) {
            return Err("NanoDrive 7 YM3526, YM3812 and YMF262 routes share the same physical chip; change the routing before playback".into());
        }
        Ok(())
    }
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

/// Build explicit route factories with NanoDrive 7 AY-compatible YM2149 support.
///
/// `nd7_devices` identifies NanoDrive 7 indices in `devices`. Their AY targets
/// accept YM2149 as an approximation through YM2203 SSG. The caller must also
/// validate NanoDrive 7 shared chip and clock resources before playback.
///
/// # Errors
///
/// Returns an error for an unknown NanoDrive 7 index, aliased devices, or
/// invalid routes. Clock and stream compatibility are checked when the
/// sequencer constructs its chip instances.
pub fn create_routed_transport_factories_with_nd7<T: NdsifRoutingTransport + 'static>(
    devices: &[Rc<RefCell<T>>],
    routes: &[NdsifRoute],
    nd7_devices: &[usize],
) -> Result<ChipFactoryRegistry, NdsifRoutingError> {
    for &index in nd7_devices {
        if index >= devices.len() {
            return Err(NdsifRoutingError::UnknownDevice(index));
        }
    }
    routed_factories_with_nd7(devices.to_vec(), routes, nd7_devices)
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
    routed_factories_with_nd7(devices, routes, &[])
}

fn routed_factories_with_nd7<T: NdsifRoutingTransport + 'static>(
    devices: Vec<Rc<RefCell<T>>>,
    routes: &[NdsifRoute],
    nd7_devices: &[usize],
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
        let nd7_devices = nd7_devices.to_vec();
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
                _ if nd7_devices.contains(&route.device) => Box::new(
                    NdsifSoundChip::with_target_instance_for_nd7(config, route.target, transport)?,
                ),
                _ => Box::new(NdsifSoundChip::with_target_instance(config, route.target, transport)?),
            };
            Ok(Some(backend))
        });
    }
    Ok(factories)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chipstream::chip::{NdsifPsgEvent, NdsifRegisterEvent};
    use chipstream::sequencer::{VgmSequencer, VgmSequencerError};
    use soundlog::chip::{Ay8910Spec, GameGearPsgSpec, Okim6258Spec, PsgSpec, Ym2151Spec};
    use soundlog::vgm::{
        VgmBuilder,
        command::{EndOfData, WaitSamples},
    };

    #[derive(Default)]
    struct Device {
        supported: bool,
        audio_supported: bool,
        configured: Vec<(WriteId, u8, u32)>,
        events: Vec<NdsifRegisterEvent>,
        psg_events: Vec<NdsifPsgEvent>,
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

        fn schedule_psg_write(&mut self, event: NdsifPsgEvent) -> Result<(), String> {
            self.psg_events.push(event);
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

    #[test]
    fn automatic_nd7_validation_uses_selected_active_chips() {
        use crate::create_automatic_transport_factories_with_nd7;

        for (chips, nd7, expected_error) in [
            (
                vec![(Chip::Ay8910, 2_000_000), (Chip::Ym2203, 4_000_000)],
                true,
                Some("same physical chip"),
            ),
            (
                vec![(Chip::Ym3526, 3_579_545), (Chip::Ymf262, 14_318_180)],
                true,
                Some("same physical chip"),
            ),
            (
                vec![(Chip::Ym3526, 3_579_545), (Chip::Ymf262, 14_318_180)],
                false,
                None,
            ),
            (vec![(Chip::Ym3526, 3_579_545)], true, None),
        ] {
            let mut builder = VgmBuilder::new();
            for (chip, clock) in chips {
                builder.register_chip(chip, Instance::Primary, clock);
            }
            builder.add_vgm_command(WaitSamples(441));
            let device = Rc::new(RefCell::new(Device {
                supported: true,
                ..Default::default()
            }));
            let devices = [device];
            let nd7_devices: &[usize] = if nd7 { &[0] } else { &[] };
            let factories =
                create_automatic_transport_factories_with_nd7(&devices, true, nd7_devices);
            let result =
                VgmSequencer::from_vgm_with_factories(builder.finalize(), 44_100, 256, factories);
            match expected_error {
                Some(expected) => assert!(
                    result
                        .err()
                        .expect("conflict should fail")
                        .to_string()
                        .contains(expected)
                ),
                None => assert!(result.is_ok(), "valid routes should succeed"),
            }
        }
    }

    #[test]
    fn automatic_nd7_validation_uses_extra_header_clocks() {
        use crate::create_automatic_transport_factories_with_nd7;
        use soundlog::vgm::header::{ChipClock, ChipId, VgmExtraHeader};

        struct SelectedInstances;
        impl NdsifTransport for SelectedInstances {
            fn configure_chip(&mut self, _: WriteId, _: u8, _: u32) -> Result<(), String> {
                Ok(())
            }
            fn schedule_register_write(&mut self, _: NdsifRegisterEvent) -> Result<(), String> {
                Ok(())
            }
            fn schedule_psg_write(&mut self, _: NdsifPsgEvent) -> Result<(), String> {
                Ok(())
            }
        }
        impl NdsifRoutingTransport for SelectedInstances {
            fn supports_route_clock(&self, id: WriteId, instance: u8, _: u32) -> bool {
                (id == WriteId::try_from(0xa0).unwrap() && instance == 0)
                    || (id == WriteId::try_from(0x55).unwrap() && instance == 1)
            }
            fn supports_route_audio(&self, _: u8, _: u32) -> bool {
                false
            }
        }
        for (secondary_clock, expected_error) in [(3_993_600, false), (3_500_000, true)] {
            let mut builder = VgmBuilder::new();
            builder.register_chip(Chip::Ay8910, Instance::Primary, 2_000_000);
            builder.register_chip(Chip::Ym2203, Instance::Primary, 4_000_000);
            builder.register_chip(Chip::Ym2203, Instance::Secondary, 4_000_000);
            let mut document = builder.finalize();
            document.header.ay_chip_type = soundlog::vgm::header::Ay8910ChipType::Ay8910;
            document.header.ym2203_clock |= 0x4000_0000;
            document.extra_header = Some(VgmExtraHeader {
                chip_clocks: vec![ChipClock::new(
                    ChipId::Ym2203,
                    Instance::Secondary,
                    secondary_clock,
                )],
                ..Default::default()
            });
            let device = Rc::new(RefCell::new(SelectedInstances));
            let factories = create_automatic_transport_factories_with_nd7(&[device], true, &[0]);
            let result = VgmSequencer::new_with_factories(document, 44_100, 256, factories);
            if expected_error {
                assert!(
                    result
                        .err()
                        .expect("clock mismatch should fail")
                        .to_string()
                        .contains("different shared clocks")
                );
            } else if let Err(error) = result {
                panic!("valid shared clock should succeed: {error}");
            }
        }
    }

    #[test]
    fn automatic_nd7_validation_is_per_selected_device() {
        use crate::create_automatic_transport_factories_with_nd7;

        struct ChipDevice(WriteId);
        impl NdsifTransport for ChipDevice {
            fn configure_chip(&mut self, _: WriteId, _: u8, _: u32) -> Result<(), String> {
                Ok(())
            }
            fn schedule_register_write(&mut self, _: NdsifRegisterEvent) -> Result<(), String> {
                Ok(())
            }
            fn schedule_psg_write(&mut self, _: NdsifPsgEvent) -> Result<(), String> {
                Ok(())
            }
        }
        impl NdsifRoutingTransport for ChipDevice {
            fn supports_route_clock(&self, id: WriteId, _: u8, _: u32) -> bool {
                self.0 == id
            }
            fn supports_route_audio(&self, _: u8, _: u32) -> bool {
                false
            }
        }

        let devices = [
            Rc::new(RefCell::new(ChipDevice(WriteId::try_from(0x5b).unwrap()))),
            Rc::new(RefCell::new(ChipDevice(WriteId::try_from(0x5e).unwrap()))),
        ];
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ym3526, Instance::Primary, 3_579_545);
        builder.register_chip(Chip::Ymf262, Instance::Primary, 14_318_180);
        let factories = create_automatic_transport_factories_with_nd7(&devices, false, &[0, 1]);
        let document = builder.finalize();
        assert!(VgmSequencer::new_with_factories(document.clone(), 44_100, 256, factories).is_ok());
        let factories = create_automatic_transport_factories_with_nd7(&devices[..1], false, &[0]);
        assert!(VgmSequencer::new_with_factories(document, 44_100, 256, factories).is_ok());
    }

    #[test]
    fn psg_hardware_uses_actual_stereo_commands_instead_of_header_capability() {
        let route = NdsifRoute {
            chip: Chip::Sn76489,
            source: Instance::Primary,
            device: 0,
            target: Instance::Primary,
        };
        for (stereo_enabled, mask, succeeds) in [
            (true, None, true),
            (true, Some(0xff), true),
            (true, Some(0x12), false),
            (false, Some(0x12), true),
        ] {
            let mut builder = VgmBuilder::new();
            builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
            if let Some(value) = mask {
                builder.add_chip_write(Instance::Primary, GameGearPsgSpec { value });
            }
            builder.add_chip_write(Instance::Primary, PsgSpec { value: 0x90 });
            builder.add_vgm_command(WaitSamples(441));
            let mut document = builder.finalize();
            document.header.sn76489_flags.gamegear_stereo = stereo_enabled;

            for use_document in [false, true] {
                let device = Rc::new(RefCell::new(Device {
                    supported: true,
                    ..Default::default()
                }));
                let factories =
                    routed_factories(vec![Rc::clone(&device)], std::slice::from_ref(&route))
                        .unwrap();
                let result = if use_document {
                    VgmSequencer::new_with_factories(document.clone(), 44_100, 256, factories)
                } else {
                    VgmSequencer::from_vgm_with_factories(
                        Vec::<u8>::from(document.clone()),
                        44_100,
                        256,
                        factories,
                    )
                };
                if succeeds {
                    let mut sequencer = result.unwrap();
                    while sequencer.render().unwrap().is_some() {}
                    let device = device.borrow();
                    assert_eq!(device.configured, [(WriteId::SN76489, 0, 3_579_545)]);
                    assert_eq!(device.psg_events.len(), 1);
                    assert_eq!(device.psg_events[0].value, 0x90);
                } else {
                    assert!(matches!(
                        result,
                        Err(VgmSequencerError::Chip(ChipError::InvalidConfiguration {
                            chip: Chip::Sn76489,
                            ..
                        }))
                    ));
                    assert!(device.borrow().configured.is_empty());
                }
            }
        }
    }

    #[test]
    fn psg_stereo_detection_is_per_instance() {
        let mut builder = VgmBuilder::new();
        for instance in [Instance::Primary, Instance::Secondary] {
            builder.register_chip(Chip::Sn76489, instance, 3_579_545);
            builder.add_chip_write(instance, PsgSpec { value: 0x90 });
        }
        builder.add_chip_write(Instance::Secondary, GameGearPsgSpec { value: 0x12 });
        builder.add_vgm_command(WaitSamples(441));
        let mut document = builder.finalize();
        document.header.sn76489_flags.gamegear_stereo = true;
        for use_document in [false, true] {
            for source in [Instance::Primary, Instance::Secondary] {
                let device = Rc::new(RefCell::new(Device {
                    supported: true,
                    ..Default::default()
                }));
                let route = NdsifRoute {
                    chip: Chip::Sn76489,
                    source,
                    device: 0,
                    target: Instance::Secondary,
                };
                let factories = routed_factories(vec![Rc::clone(&device)], &[route]).unwrap();
                let result = if use_document {
                    VgmSequencer::new_with_factories(document.clone(), 44_100, 256, factories)
                } else {
                    VgmSequencer::from_vgm_with_factories(
                        Vec::<u8>::from(document.clone()),
                        44_100,
                        256,
                        factories,
                    )
                };
                if source == Instance::Primary {
                    let mut sequencer = result.unwrap();
                    while sequencer.render().unwrap().is_some() {}
                    let device = device.borrow();
                    assert_eq!(device.configured, [(WriteId::SN76489_SECOND, 1, 3_579_545)]);
                    assert_eq!(device.psg_events.len(), 1);
                    assert_eq!(device.psg_events[0].instance, 1);
                } else {
                    assert!(matches!(
                        result,
                        Err(VgmSequencerError::Chip(ChipError::InvalidConfiguration {
                            chip: Chip::Sn76489,
                            ..
                        }))
                    ));
                    assert!(device.borrow().configured.is_empty());
                }
            }
        }
    }

    #[test]
    fn psg_hardware_rejects_t6w28_before_configuration() {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
        let mut document = builder.finalize();
        document.header.sn76489_clock |= 0x8000_0000;
        let device = Rc::new(RefCell::new(Device {
            supported: true,
            ..Default::default()
        }));
        let route = NdsifRoute {
            chip: Chip::Sn76489,
            source: Instance::Primary,
            device: 0,
            target: Instance::Primary,
        };
        let factories = routed_factories(vec![Rc::clone(&device)], &[route]).unwrap();
        assert!(matches!(
            VgmSequencer::new_with_factories(document, 44_100, 256, factories),
            Err(VgmSequencerError::Chip(ChipError::InvalidConfiguration {
                chip: Chip::Sn76489,
                reason: "T6W28 PSG is not supported",
            }))
        ));
        assert!(device.borrow().configured.is_empty());
    }

    #[test]
    fn psg_stereo_commands_after_end_of_data_are_ignored() {
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Sn76489, Instance::Primary, 3_579_545);
        builder.add_chip_write(Instance::Primary, PsgSpec { value: 0x90 });
        builder.add_vgm_command(WaitSamples(441));
        builder.add_vgm_command(EndOfData);
        builder.add_chip_write(Instance::Primary, GameGearPsgSpec { value: 0x12 });
        let mut document = builder.finalize();
        document.header.sn76489_flags.gamegear_stereo = true;
        for use_document in [false, true] {
            let device = Rc::new(RefCell::new(Device {
                supported: true,
                ..Default::default()
            }));
            let route = NdsifRoute {
                chip: Chip::Sn76489,
                source: Instance::Primary,
                device: 0,
                target: Instance::Primary,
            };
            let factories = routed_factories(vec![Rc::clone(&device)], &[route]).unwrap();
            let mut sequencer = if use_document {
                VgmSequencer::new_with_factories(document.clone(), 44_100, 256, factories)
            } else {
                VgmSequencer::from_vgm_with_factories(
                    Vec::<u8>::from(document.clone()),
                    44_100,
                    256,
                    factories,
                )
            }
            .unwrap();
            while sequencer.render().unwrap().is_some() {}
            assert_eq!(device.borrow().psg_events.len(), 1);
        }
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
    fn ym2149_routes_only_to_identified_nd7_ay_targets() {
        use crate::create_automatic_transport_factories_with_nd7;

        let device = Rc::new(RefCell::new(Device {
            supported: true,
            ..Default::default()
        }));
        let route = NdsifRoute {
            chip: Chip::Ay8910,
            source: Instance::Primary,
            device: 0,
            target: Instance::Secondary,
        };
        let mut builder = VgmBuilder::new();
        builder.register_chip(Chip::Ay8910, Instance::Primary, 2_000_000);
        builder.add_chip_write(
            Instance::Primary,
            Ay8910Spec {
                register: 8,
                value: 15,
            },
        );
        builder.add_vgm_command(WaitSamples(441));
        let mut bytes: Vec<u8> = builder.finalize().into();
        bytes[0x78] = 0x10;
        bytes[0x79] = 0x01;

        for automatic in [false, true] {
            for nd7 in [false, true] {
                device.borrow_mut().configured.clear();
                device.borrow_mut().events.clear();
                let nd7_devices: &[usize] = if nd7 { &[0] } else { &[] };
                let factories = if automatic {
                    create_automatic_transport_factories_with_nd7(
                        std::slice::from_ref(&device),
                        true,
                        nd7_devices,
                    )
                } else {
                    create_routed_transport_factories_with_nd7(
                        std::slice::from_ref(&device),
                        std::slice::from_ref(&route),
                        nd7_devices,
                    )
                    .unwrap()
                };
                let sequencer =
                    VgmSequencer::from_vgm_with_factories(bytes.clone(), 44_100, 256, factories);
                if !nd7 {
                    assert!(matches!(
                        sequencer,
                        Err(VgmSequencerError::Chip(
                            ChipError::InvalidConfiguration { .. }
                        ))
                    ));
                    assert!(device.borrow().configured.is_empty());
                    continue;
                }
                let mut sequencer = sequencer.unwrap();
                while sequencer.render().unwrap().is_some() {}
                let target = if automatic { 0 } else { 1 };
                let device = device.borrow();
                let ay = WriteId::try_from(0xa0).unwrap();
                assert_eq!(device.configured, [(ay, target, 2_000_000)]);
                assert_eq!(device.events.len(), 1);
                assert_eq!(
                    (
                        device.events[0].write_id,
                        device.events[0].instance,
                        device.events[0].page,
                        device.events[0].write.address,
                        device.events[0].write.value,
                    ),
                    (ay, target, 0, 8, 15)
                );
            }
        }

        for (offset, value) in [
            (0x78, 0x11),
            (0x79, 0x04),
            (0x79, 0x08),
            (0x79, 0x10),
            (0x79, 0x20),
        ] {
            device.borrow_mut().configured.clear();
            let mut invalid = bytes.clone();
            invalid[offset] = value;
            let factories = create_routed_transport_factories_with_nd7(
                std::slice::from_ref(&device),
                std::slice::from_ref(&route),
                &[0],
            )
            .unwrap();
            assert!(matches!(
                VgmSequencer::from_vgm_with_factories(invalid, 44_100, 256, factories),
                Err(VgmSequencerError::Chip(
                    ChipError::InvalidConfiguration { .. }
                ))
            ));
            assert!(device.borrow().configured.is_empty());
        }
        assert!(matches!(
            create_routed_transport_factories_with_nd7(
                std::slice::from_ref(&device),
                &[route],
                &[1]
            ),
            Err(NdsifRoutingError::UnknownDevice(1))
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

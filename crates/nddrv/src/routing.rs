//! Persisted NanoDrive devices and session-local routing.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use chipstream::sequencer::{Chip, Instance};
use ndsif::CapabilityEntry;
use ndsif_transport::{NdsifNd7Validator, NdsifRoute};
use serde::{Deserialize, Serialize};

use crate::library::VgmSettings;

/// Chip types that can be routed to an NDSIF device.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareChip {
    Sn76489,
    Ay8910,
    Ym2413,
    Ym2151,
    Ym2203,
    Ym2608,
    Ym2610b,
    Ym2612,
    Ym3526,
    Y8950,
    Ym3812,
    Ymf262,
    Ymf278b,
    Okim6258,
}

impl HardwareChip {
    pub const ALL: [Self; 14] = [
        Self::Sn76489,
        Self::Ay8910,
        Self::Ym2413,
        Self::Ym2151,
        Self::Ym2203,
        Self::Ym2608,
        Self::Ym2610b,
        Self::Ym2612,
        Self::Ym3526,
        Self::Y8950,
        Self::Ym3812,
        Self::Ymf262,
        Self::Ymf278b,
        Self::Okim6258,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Sn76489 => "SN76489",
            Self::Ay8910 => "AY8910",
            Self::Ym2413 => "YM2413",
            Self::Ym2151 => "YM2151",
            Self::Ym2203 => "YM2203",
            Self::Ym2608 => "YM2608",
            Self::Ym2610b => "YM2610/B",
            Self::Ym2612 => "YM2612",
            Self::Ym3526 => "YM3526",
            Self::Y8950 => "Y8950",
            Self::Ym3812 => "YM3812",
            Self::Ymf262 => "YMF262",
            Self::Ymf278b => "YMF278B",
            Self::Okim6258 => "OKIM6258",
        }
    }

    pub fn sequencer_chip(self) -> Chip {
        match self {
            Self::Sn76489 => Chip::Sn76489,
            Self::Ay8910 => Chip::Ay8910,
            Self::Ym2413 => Chip::Ym2413,
            Self::Ym2151 => Chip::Ym2151,
            Self::Ym2203 => Chip::Ym2203,
            Self::Ym2608 => Chip::Ym2608,
            Self::Ym2610b => Chip::Ym2610b,
            Self::Ym2612 => Chip::Ym2612,
            Self::Ym3526 => Chip::Ym3526,
            Self::Y8950 => Chip::Y8950,
            Self::Ym3812 => Chip::Ym3812,
            Self::Ymf262 => Chip::Ymf262,
            Self::Ymf278b => Chip::Ymf278b,
            Self::Okim6258 => Chip::Okim6258,
        }
    }

    pub fn write_id(self, instance: ChipInstance) -> u8 {
        match self {
            Self::Sn76489 if instance == ChipInstance::Secondary => 0x30,
            Self::Sn76489 => 0x50,
            Self::Ay8910 => 0xa0,
            Self::Ym2413 => 0x51,
            Self::Ym2612 => 0x52,
            Self::Ym2151 => 0x54,
            Self::Ym2203 => 0x55,
            Self::Ym2608 => 0x56,
            Self::Ym2610b => 0x58,
            Self::Ym3812 => 0x5a,
            Self::Ym3526 => 0x5b,
            Self::Y8950 => 0x5c,
            Self::Ymf262 => 0x5e,
            Self::Ymf278b => 0xd0,
            Self::Okim6258 => 0xb7,
        }
    }

    pub fn advertised(self, instance: ChipInstance, capabilities: &[CapabilityEntry]) -> bool {
        !(self == Self::Okim6258 && instance == ChipInstance::Secondary)
            && capabilities.iter().any(|entry| {
                entry.write_id == self.write_id(instance) && entry.instance == instance.number()
            })
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChipInstance {
    Primary,
    Secondary,
}

impl ChipInstance {
    pub const ALL: [Self; 2] = [Self::Primary, Self::Secondary];

    pub fn number(self) -> u8 {
        match self {
            Self::Primary => 0,
            Self::Secondary => 1,
        }
    }

    fn sequencer_instance(self) -> Instance {
        match self {
            Self::Primary => Instance::Primary,
            Self::Secondary => Instance::Secondary,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlaybackPlan {
    pub device_ids: Vec<u64>,
    pub routes: Vec<NdsifRoute>,
    pub nd7_devices: Vec<usize>,
    pub routing: RoutingPlan,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: u64,
    pub name: String,
    pub port: String,
    pub baud_rate: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    pub chip: HardwareChip,
    pub source: ChipInstance,
    pub device_id: u64,
    pub target: ChipInstance,
}

/// A logical source in the sequencer, independent of its destination.
#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct Source {
    pub chip: Chip,
    pub instance: ChipInstance,
}

/// The physical or software endpoint selected for a source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Hardware {
        device_id: u64,
        chip: HardwareChip,
        instance: ChipInstance,
    },
    Software {
        chip: SoftwareTarget,
        instance: ChipInstance,
    },
    Silent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SoftwareTarget {
    Native(Chip),
    Ym2149,
}

/// How the source is interpreted by the selected target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Adapter {
    Direct,
    AyToYm2149,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Assignment {
    pub source: Source,
    pub target: Target,
    pub adapter: Adapter,
}

/// Resolved session assignments, including software and silent fallbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutingPlan {
    assignments: Vec<Assignment>,
    settings: VgmSettings,
}

impl RoutingPlan {
    pub fn new(connections: &[Connection], settings: VgmSettings) -> Self {
        let assignments = HardwareChip::ALL
            .into_iter()
            .flat_map(|chip| {
                ChipInstance::ALL.into_iter().map(move |instance| {
                    let source = Source {
                        chip: chip.sequencer_chip(),
                        instance,
                    };
                    let hardware = connections.iter().find(|connection| {
                        connection.chip == chip && connection.source == instance
                    });
                    match hardware {
                        Some(connection) => Assignment {
                            source,
                            target: Target::Hardware {
                                device_id: connection.device_id,
                                chip: connection.chip,
                                instance: connection.target,
                            },
                            adapter: Adapter::Direct,
                        },
                        None => Self::software_assignment(source, settings),
                    }
                })
            })
            .chain(
                [Chip::SegaPcm, Chip::Pwm, Chip::C140, Chip::Okim6295]
                    .into_iter()
                    .flat_map(|chip| {
                        ChipInstance::ALL.map(move |instance| {
                            Self::software_assignment(
                                Source {
                                    chip: chip.clone(),
                                    instance,
                                },
                                settings,
                            )
                        })
                    }),
            )
            .collect();
        Self {
            assignments,
            settings,
        }
    }

    pub fn assignment(&self, source: &Source) -> &Assignment {
        self.assignments
            .iter()
            .find(|assignment| &assignment.source == source)
            .expect("all routable source instances have an assignment")
    }

    pub fn software_assignment(source: Source, settings: VgmSettings) -> Assignment {
        let (target, adapter) = match &source.chip {
            Chip::Ay8910 if settings.ay8910_ym2149 => (
                Target::Software {
                    chip: SoftwareTarget::Ym2149,
                    instance: source.instance,
                },
                Adapter::AyToYm2149,
            ),
            Chip::Ay8910 => (Target::Silent, Adapter::Direct),
            chip => (
                Target::Software {
                    chip: SoftwareTarget::Native(chip.clone()),
                    instance: source.instance,
                },
                Adapter::Direct,
            ),
        };
        Assignment {
            source,
            target,
            adapter,
        }
    }

    pub fn software_fallback(&self, source: Source) -> Assignment {
        Self::software_assignment(source, self.settings)
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }
}

/// Older profile fields are ignored when loading; device settings are retained.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingConfig {
    #[serde(default)]
    pub devices: Vec<Device>,
}

impl RoutingConfig {
    pub fn validate(&self) -> Result<()> {
        let mut ids = HashSet::new();
        let mut ports = HashSet::new();
        for device in &self.devices {
            if !ids.insert(device.id) {
                bail!("duplicate NanoDrive device ID {}", device.id);
            }
            if device.baud_rate == 0 {
                bail!("NanoDrive device {} has an invalid baud rate", device.id);
            }
            if !device.port.is_empty() && !ports.insert(&device.port) {
                bail!(
                    "NanoDrive serial port is configured more than once: {}",
                    device.port
                );
            }
        }
        Ok(())
    }
}

type SourceKey = (HardwareChip, ChipInstance);
type TargetKey = (u64, ChipInstance);

/// Manual choices, including explicit emulation, live only for this session.
#[derive(Clone, Debug, Default)]
pub struct SessionRouting {
    overrides: HashMap<SourceKey, Option<TargetKey>>,
    connections: Vec<Connection>,
}

impl SessionRouting {
    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }

    pub fn reset(&mut self, devices: &[Device], available: &HashMap<u64, Vec<CapabilityEntry>>) {
        self.overrides.clear();
        self.refresh(devices, available);
    }

    pub fn refresh(&mut self, devices: &[Device], available: &HashMap<u64, Vec<CapabilityEntry>>) {
        self.overrides.retain(|(chip, _), target| {
            target.is_none_or(|(id, instance)| {
                available
                    .get(&id)
                    .is_some_and(|entries| chip.advertised(instance, entries))
            })
        });
        let mut used = HashSet::new();
        for (&(chip, _), target) in &self.overrides {
            if let Some((id, instance)) = target {
                used.insert((chip, *id, *instance));
            }
        }
        let mut connections = Vec::new();
        for chip in HardwareChip::ALL {
            for source in ChipInstance::ALL {
                let target = match self.overrides.get(&(chip, source)) {
                    Some(target) => *target,
                    None => devices.iter().find_map(|device| {
                        let entries = available.get(&device.id)?;
                        ChipInstance::ALL.into_iter().find_map(|instance| {
                            (chip.advertised(instance, entries)
                                && !used.contains(&(chip, device.id, instance)))
                            .then_some((device.id, instance))
                        })
                    }),
                };
                if let Some((device_id, target)) = target {
                    used.insert((chip, device_id, target));
                    connections.push(Connection {
                        chip,
                        source,
                        device_id,
                        target,
                    });
                }
            }
        }
        self.connections = connections;
    }

    pub fn set_route(
        &mut self,
        chip: HardwareChip,
        source: ChipInstance,
        target: Option<TargetKey>,
        devices: &[Device],
        available: &HashMap<u64, Vec<CapabilityEntry>>,
    ) -> Result<()> {
        if let Some((id, instance)) = target {
            if !devices.iter().any(|device| device.id == id)
                || !available
                    .get(&id)
                    .is_some_and(|entries| chip.advertised(instance, entries))
            {
                bail!("The device is disconnected or does not advertise this chip instance");
            }
            if self
                .overrides
                .iter()
                .any(|(&(other_chip, other_source), &other_target)| {
                    other_chip == chip && other_source != source && other_target == target
                })
            {
                bail!("The target is already assigned to another source");
            }
        }
        self.overrides.insert((chip, source), target);
        self.refresh(devices, available);
        Ok(())
    }

    pub fn playback_plan(&self, devices: &[Device], settings: VgmSettings) -> Option<PlaybackPlan> {
        if self.connections.is_empty() {
            return None;
        }
        let mut device_ids = Vec::new();
        let mut indices = HashMap::new();
        let routing = RoutingPlan::new(&self.connections, settings);
        let routes = routing
            .assignments()
            .iter()
            .filter_map(|assignment| {
                let Target::Hardware {
                    device_id,
                    chip,
                    instance,
                } = &assignment.target
                else {
                    return None;
                };
                let device = *indices.entry(*device_id).or_insert_with(|| {
                    let index = device_ids.len();
                    device_ids.push(*device_id);
                    index
                });
                Some(NdsifRoute {
                    chip: chip.sequencer_chip(),
                    source: assignment.source.instance.sequencer_instance(),
                    device,
                    target: instance.sequencer_instance(),
                })
            })
            .collect();
        let nd7_devices = device_ids
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                devices
                    .iter()
                    .find(|device| device.id == *id)
                    .filter(|device| is_nanodrive7(&device.name))
                    .map(|_| index)
            })
            .collect();
        Some(PlaybackPlan {
            device_ids,
            routes,
            nd7_devices,
            routing,
        })
    }
}

fn is_nanodrive7(name: &str) -> bool {
    name == "NanoDrive 7"
        || name
            .strip_prefix("NanoDrive 7 #")
            .and_then(|suffix| suffix.parse::<u64>().ok())
            .is_some_and(|number| number >= 2)
}

impl PlaybackPlan {
    /// Validate ND7 shared chips and clocks against the instances used by this track.
    pub fn validate_nd7(
        &self,
        header: &soundlog::vgm::VgmHeader,
        extra: Option<&soundlog::vgm::header::VgmExtraHeader>,
    ) -> Result<()> {
        use soundlog::vgm::header::ChipId;

        let mut validator = NdsifNd7Validator::default();
        for route in &self.routes {
            if !self.nd7_devices.contains(&route.device) {
                continue;
            }
            let (raw, chip_id) = match route.chip {
                Chip::Ay8910 => (header.ay8910_clock, ChipId::Ay8910),
                Chip::Ym2203 => (header.ym2203_clock, ChipId::Ym2203),
                Chip::Ym3526 => (header.ym3526_clock, ChipId::Ym3526),
                Chip::Ym3812 => (header.ym3812_clock, ChipId::Ym3812),
                Chip::Ymf262 => (header.ymf262_clock, ChipId::Ymf262),
                _ => continue,
            };
            if raw & 0x3fff_ffff == 0
                || (route.source == Instance::Secondary && raw & 0x4000_0000 == 0)
            {
                continue;
            }
            let clock = extra
                .and_then(|extra| {
                    extra
                        .chip_clocks
                        .iter()
                        .find(|clock| clock.chip_id == chip_id && clock.instance == route.source)
                })
                .map_or(raw & 0x3fff_ffff, |clock| clock.clock & 0x3fff_ffff);
            validator.add(route, clock).map_err(anyhow::Error::msg)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: u64) -> Device {
        Device {
            id,
            name: format!("Drive {id}"),
            port: format!("port{id}"),
            baud_rate: 115_200,
        }
    }

    fn ym2151(instance: u8) -> CapabilityEntry {
        CapabilityEntry {
            write_id: 0x54,
            instance,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        }
    }

    fn nd7_plan(routes: Vec<NdsifRoute>, nd7_devices: Vec<usize>) -> PlaybackPlan {
        PlaybackPlan {
            device_ids: vec![1, 2],
            routes,
            nd7_devices,
            routing: RoutingPlan::new(&[], VgmSettings::default()),
        }
    }

    fn nd7_route(chip: Chip, source: Instance, device: usize, target: Instance) -> NdsifRoute {
        NdsifRoute {
            chip,
            source,
            device,
            target,
        }
    }

    #[test]
    fn routing_plan_resolves_hardware_software_and_silent_per_instance() {
        let primary = Source {
            chip: Chip::Ay8910,
            instance: ChipInstance::Primary,
        };
        let secondary = Source {
            instance: ChipInstance::Secondary,
            ..primary.clone()
        };
        let connection = Connection {
            chip: HardwareChip::Ay8910,
            source: ChipInstance::Primary,
            device_id: 7,
            target: ChipInstance::Secondary,
        };
        for enabled in [false, true] {
            let settings = VgmSettings {
                ay8910_ym2149: enabled,
            };
            let plan = RoutingPlan::new(std::slice::from_ref(&connection), settings);
            assert_eq!(
                plan.assignment(&primary),
                &Assignment {
                    source: primary.clone(),
                    target: Target::Hardware {
                        device_id: 7,
                        chip: HardwareChip::Ay8910,
                        instance: ChipInstance::Secondary
                    },
                    adapter: Adapter::Direct,
                }
            );
            let software = if enabled {
                Assignment {
                    source: secondary.clone(),
                    target: Target::Software {
                        chip: SoftwareTarget::Ym2149,
                        instance: ChipInstance::Secondary,
                    },
                    adapter: Adapter::AyToYm2149,
                }
            } else {
                Assignment {
                    source: secondary.clone(),
                    target: Target::Silent,
                    adapter: Adapter::Direct,
                }
            };
            assert_eq!(*plan.assignment(&secondary), software);
            assert_eq!(
                plan.software_fallback(primary.clone()).adapter,
                software.adapter
            );
            let fm = Source {
                chip: Chip::Ym2151,
                instance: ChipInstance::Primary,
            };
            assert_eq!(
                plan.assignment(&fm).target,
                Target::Software {
                    chip: SoftwareTarget::Native(Chip::Ym2151),
                    instance: ChipInstance::Primary
                }
            );
            let pcm = Source {
                chip: Chip::SegaPcm,
                instance: ChipInstance::Secondary,
            };
            assert_eq!(
                plan.assignment(&pcm).target,
                Target::Software {
                    chip: SoftwareTarget::Native(Chip::SegaPcm),
                    instance: ChipInstance::Secondary,
                }
            );
        }
    }

    #[test]
    fn nd7_detects_shared_physical_chips_only_for_active_routes() {
        let routes = vec![
            nd7_route(Chip::Ay8910, Instance::Primary, 0, Instance::Primary),
            nd7_route(Chip::Ym2203, Instance::Primary, 0, Instance::Primary),
            nd7_route(Chip::Ym3812, Instance::Primary, 0, Instance::Primary),
            nd7_route(Chip::Ymf262, Instance::Primary, 0, Instance::Primary),
        ];
        let plan = nd7_plan(routes.clone(), vec![0]);
        let mut header = soundlog::vgm::VgmHeader {
            ay8910_clock: 2_000_000,
            ..Default::default()
        };
        assert!(plan.validate_nd7(&header, None).is_ok());
        header.ym2203_clock = 4_000_000;
        assert!(
            plan.validate_nd7(&header, None)
                .unwrap_err()
                .to_string()
                .contains("same physical chip")
        );
        header.ym2203_clock = 0;
        header.ym3812_clock = 3_579_545;
        header.ymf262_clock = 14_318_180;
        assert!(
            plan.validate_nd7(&header, None)
                .unwrap_err()
                .to_string()
                .contains("same physical chip")
        );
        let mut separate = routes;
        separate[3].device = 1;
        assert!(
            nd7_plan(separate, vec![0, 1])
                .validate_nd7(&header, None)
                .is_ok()
        );
    }

    #[test]
    fn nd7_compares_normalized_clocks_and_extra_header_instances() {
        use soundlog::vgm::header::{ChipClock, ChipId, VgmExtraHeader};
        let routes = vec![
            nd7_route(Chip::Ay8910, Instance::Primary, 0, Instance::Primary),
            nd7_route(Chip::Ym2203, Instance::Secondary, 0, Instance::Secondary),
        ];
        let plan = nd7_plan(routes, vec![0]);
        let header = soundlog::vgm::VgmHeader {
            ay8910_clock: 2_000_000,
            ym2203_clock: 0x4000_0000 | 3_993_600,
            ..Default::default()
        };
        assert!(plan.validate_nd7(&header, None).is_ok());
        let extra = VgmExtraHeader {
            header_size: 0,
            chip_clock_offset: 0,
            chip_vol_offset: 0,
            chip_clocks: vec![ChipClock::new(
                ChipId::Ym2203,
                Instance::Secondary,
                3_500_000,
            )],
            chip_volumes: vec![],
        };
        assert!(
            plan.validate_nd7(&header, Some(&extra))
                .unwrap_err()
                .to_string()
                .contains("different shared clocks")
        );
        assert!(
            nd7_plan(plan.routes, vec![])
                .validate_nd7(&header, Some(&extra))
                .is_ok()
        );
        assert!(is_nanodrive7("NanoDrive 7 #2"));
        assert!(!is_nanodrive7("NanoDrive 70"));
    }

    #[test]
    fn allocates_connected_targets_in_device_order_and_ignores_duplicate_pages() {
        let devices = vec![device(2), device(1)];
        let available = HashMap::from([(1, vec![ym2151(0)]), (2, vec![ym2151(0), ym2151(0)])]);
        let mut routing = SessionRouting::default();
        routing.refresh(&devices, &available);
        assert_eq!(
            routing.connections(),
            [
                Connection {
                    chip: HardwareChip::Ym2151,
                    source: ChipInstance::Primary,
                    device_id: 2,
                    target: ChipInstance::Primary
                },
                Connection {
                    chip: HardwareChip::Ym2151,
                    source: ChipInstance::Secondary,
                    device_id: 1,
                    target: ChipInstance::Primary
                },
            ]
        );
        let plan = routing
            .playback_plan(&devices, VgmSettings::default())
            .unwrap();
        assert_eq!(plan.device_ids, [2, 1]);
        assert_eq!(plan.routes[1].source, Instance::Secondary);
        assert_eq!(plan.routes[1].target, Instance::Primary);
    }

    #[test]
    fn manual_choices_reserve_targets_and_emulation_until_reset() {
        let devices = vec![device(1), device(2)];
        let available = HashMap::from([(1, vec![ym2151(0)]), (2, vec![ym2151(0)])]);
        let mut routing = SessionRouting::default();
        routing.refresh(&devices, &available);
        routing
            .set_route(
                HardwareChip::Ym2151,
                ChipInstance::Secondary,
                Some((1, ChipInstance::Primary)),
                &devices,
                &available,
            )
            .unwrap();
        assert_eq!(routing.connections()[0].device_id, 2);
        routing
            .set_route(
                HardwareChip::Ym2151,
                ChipInstance::Primary,
                None,
                &devices,
                &available,
            )
            .unwrap();
        assert_eq!(routing.connections().len(), 1);
        routing.reset(&devices, &available);
        assert_eq!(routing.connections().len(), 2);
        assert_eq!(routing.connections()[0].device_id, 1);
    }

    #[test]
    fn invalid_and_duplicate_manual_targets_are_rejected_without_changing_routes() {
        let devices = vec![device(1)];
        let available = HashMap::from([(1, vec![ym2151(0)])]);
        let mut routing = SessionRouting::default();
        routing
            .set_route(
                HardwareChip::Ym2151,
                ChipInstance::Primary,
                Some((1, ChipInstance::Primary)),
                &devices,
                &available,
            )
            .unwrap();
        let before = routing.connections().to_vec();
        assert!(
            routing
                .set_route(
                    HardwareChip::Ym2151,
                    ChipInstance::Secondary,
                    Some((1, ChipInstance::Primary)),
                    &devices,
                    &available
                )
                .is_err()
        );
        assert!(
            routing
                .set_route(
                    HardwareChip::Ym2151,
                    ChipInstance::Secondary,
                    Some((2, ChipInstance::Primary)),
                    &devices,
                    &available
                )
                .is_err()
        );
        assert_eq!(routing.connections(), before);
    }

    #[test]
    fn connecting_another_device_preserves_explicit_emulation() {
        let devices = vec![device(1), device(2)];
        let first = HashMap::from([(1, vec![ym2151(0)])]);
        let both = HashMap::from([(1, vec![ym2151(0)]), (2, vec![ym2151(0)])]);
        let mut routing = SessionRouting::default();
        routing
            .set_route(
                HardwareChip::Ym2151,
                ChipInstance::Primary,
                None,
                &devices,
                &first,
            )
            .unwrap();
        routing.refresh(&devices, &both);
        assert_eq!(routing.connections().len(), 1);
        assert_eq!(routing.connections()[0].source, ChipInstance::Secondary);
        assert_eq!(routing.connections()[0].device_id, 1);
        routing.reset(&devices, &HashMap::from([(2, vec![ym2151(0)])]));
        assert_eq!(routing.connections()[0].source, ChipInstance::Primary);
        assert_eq!(routing.connections()[0].device_id, 2);
    }

    #[test]
    fn one_device_with_two_instances_routes_both_without_duplicate_targets() {
        let devices = vec![device(1), device(2)];
        let available = HashMap::from([(1, vec![ym2151(0), ym2151(1)]), (2, vec![ym2151(0)])]);
        let mut routing = SessionRouting::default();
        routing.refresh(&devices, &available);
        assert_eq!(routing.connections().len(), 2);
        assert_eq!(routing.connections()[0].device_id, 1);
        assert_eq!(routing.connections()[0].target, ChipInstance::Primary);
        assert_eq!(routing.connections()[1].device_id, 1);
        assert_eq!(routing.connections()[1].target, ChipInstance::Secondary);
        assert_eq!(
            routing
                .playback_plan(&devices, VgmSettings::default())
                .unwrap()
                .device_ids,
            [1]
        );
        routing.reset(&devices, &HashMap::new());
        assert!(
            routing
                .playback_plan(&devices, VgmSettings::default())
                .is_none()
        );
    }

    #[test]
    fn device_configuration_rejects_duplicate_ids_ports_and_invalid_baud() {
        let mut config = RoutingConfig {
            devices: vec![device(1), device(2)],
        };
        assert!(config.validate().is_ok());
        config.devices[1].id = 1;
        assert!(config.validate().is_err());
        config.devices[1].id = 2;
        config.devices[1].port = config.devices[0].port.clone();
        assert!(config.validate().is_err());
        config.devices[1].port.clear();
        config.devices[1].baud_rate = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn legacy_profiles_are_ignored_and_devices_are_retained() {
        let json = r#"{"devices":[{"id":1,"name":"Drive","port":"port1","baud_rate":115200}],"profiles":[{"id":2,"name":"Old","connections":[]}],"selected_profile":2}"#;
        let config: RoutingConfig = serde_json::from_str(json).unwrap();
        config.validate().unwrap();
        assert_eq!(config.devices[0].name, "Drive");
        assert_eq!(config.devices[0].port, "port1");
        assert!(!serde_json::to_string(&config).unwrap().contains("profiles"));
    }

    #[test]
    fn every_hardware_chip_has_its_matching_sequencer_chip() {
        assert_eq!(
            HardwareChip::ALL.map(HardwareChip::sequencer_chip),
            [
                Chip::Sn76489,
                Chip::Ay8910,
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
                Chip::Okim6258,
            ]
        );
    }

    #[test]
    fn advertised_targets_require_matching_chip_and_instance() {
        let entry = ym2151(0);
        assert!(HardwareChip::Ym2151.advertised(ChipInstance::Primary, &[entry]));
        assert!(!HardwareChip::Ym2151.advertised(ChipInstance::Secondary, &[entry]));
        assert!(!HardwareChip::Ym2203.advertised(ChipInstance::Primary, &[entry]));
        let secondary_psg = CapabilityEntry {
            write_id: 0x30,
            instance: 1,
            ..entry
        };
        assert!(HardwareChip::Sn76489.advertised(ChipInstance::Secondary, &[secondary_psg]));
        let secondary_oki = CapabilityEntry {
            write_id: 0xb7,
            instance: 1,
            ..entry
        };
        assert!(!HardwareChip::Okim6258.advertised(ChipInstance::Secondary, &[secondary_oki]));
    }
}

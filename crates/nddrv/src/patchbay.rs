//! Deterministic patchbay layout and endpoint selection.

use std::collections::HashMap;

use crate::DeviceStatus;
use crate::routing::{ChipInstance, Connection, Device, HardwareChip, Profile};

pub const WIDTH: f32 = 866.;
pub const SOURCE_X: f32 = 16.;
pub const SOURCE_WIDTH: f32 = 180.;
pub const TARGET_X: f32 = 336.;
pub const TARGET_WIDTH: f32 = 200.;
pub const DEVICE_X: f32 = 620.;
pub const DEVICE_WIDTH: f32 = 230.;
pub const HEADER: f32 = 40.;
pub const ROW: f32 = 30.;
const GAP: f32 = 16.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Hardware(HardwareChip, ChipInstance),
    Software(&'static str, ChipInstance),
}

impl Source {
    pub fn label(self) -> String {
        let (name, instance) = match self {
            Self::Hardware(chip, instance) => (chip.name(), instance),
            Self::Software(name, instance) => (name, instance),
        };
        format!("{name} #{}", instance.number() + 1)
    }
}

#[derive(Debug)]
pub struct Endpoint {
    pub chip: HardwareChip,
    pub instance: ChipInstance,
    pub y: f32,
    pub available: bool,
}

#[derive(Debug)]
pub struct DeviceBox {
    pub id: u64,
    pub name: String,
    pub connected: bool,
    pub top: f32,
    pub height: f32,
    pub endpoints: Vec<Endpoint>,
}

#[derive(Debug)]
pub struct Cable {
    pub source_y: f32,
    pub target_x: f32,
    pub target_y: f32,
    pub hardware: bool,
    pub available: bool,
}

#[derive(Debug)]
pub struct Graph {
    pub sources: Vec<Source>,
    pub devices: Vec<DeviceBox>,
    pub cables: Vec<Cable>,
    pub emulator_height: f32,
    pub height: f32,
}

pub fn row_y(index: usize) -> f32 {
    HEADER + ROW * index as f32
}

pub fn build(
    devices: &[Device],
    statuses: &HashMap<u64, DeviceStatus>,
    profile: Option<&Profile>,
) -> Graph {
    let sources: Vec<_> = HardwareChip::ALL
        .into_iter()
        .flat_map(|chip| ChipInstance::ALL.map(|instance| Source::Hardware(chip, instance)))
        .chain(
            ["Sega PCM", "PWM", "C140", "OKIM6295"]
                .into_iter()
                .flat_map(|name| {
                    ChipInstance::ALL.map(|instance| Source::Software(name, instance))
                }),
        )
        .collect();
    let routes = profile.map_or(&[][..], |profile| profile.connections.as_slice());
    let emulator_height = HEADER + ROW * sources.len() as f32 + 8.;
    let mut next_top = 0.;
    let mut boxes = Vec::new();
    for device in devices {
        let connected = matches!(statuses.get(&device.id), Some(DeviceStatus::Connected(_)));
        let entries = match statuses.get(&device.id) {
            Some(DeviceStatus::Connected(entries)) => entries.as_slice(),
            _ => &[],
        };
        let endpoints: Vec<_> = HardwareChip::ALL
            .into_iter()
            .flat_map(|chip| ChipInstance::ALL.map(move |instance| (chip, instance)))
            .filter_map(|(chip, instance)| {
                let available = chip.advertised(instance, entries);
                let saved = routes.iter().any(|route| {
                    route.device_id == device.id && route.chip == chip && route.target == instance
                });
                (available || saved).then_some((chip, instance, available))
            })
            .enumerate()
            .map(|(index, (chip, instance, available))| Endpoint {
                chip,
                instance,
                y: next_top + row_y(index),
                available,
            })
            .collect();
        let height = HEADER + ROW * endpoints.len().max(1) as f32 + 8.;
        boxes.push(DeviceBox {
            id: device.id,
            name: if device.name.is_empty() {
                device.port.clone()
            } else {
                device.name.clone()
            },
            connected,
            top: next_top,
            height,
            endpoints,
        });
        next_top += height + GAP;
    }
    let cables = sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let route: Option<&Connection> = match source {
                Source::Hardware(chip, instance) => routes
                    .iter()
                    .find(|route| route.chip == *chip && route.source == *instance),
                Source::Software(_, _) => None,
            };
            if let Some(route) = route {
                let endpoint = boxes
                    .iter()
                    .find(|device| device.id == route.device_id)
                    .and_then(|device| {
                        device.endpoints.iter().find(|endpoint| {
                            endpoint.chip == route.chip && endpoint.instance == route.target
                        })
                    })
                    .expect("validated routes have a device and retained endpoint");
                Cable {
                    source_y: row_y(index) + ROW / 2.,
                    target_x: DEVICE_X,
                    target_y: endpoint.y + ROW / 2.,
                    hardware: true,
                    available: endpoint.available,
                }
            } else {
                Cable {
                    source_y: row_y(index) + ROW / 2.,
                    target_x: TARGET_X,
                    target_y: row_y(index) + ROW / 2.,
                    hardware: false,
                    available: true,
                }
            }
        })
        .collect();
    Graph {
        sources,
        devices: boxes,
        cables,
        emulator_height,
        height: (next_top - GAP).max(emulator_height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::{Connection, RoutingConfig};

    #[test]
    fn saved_offline_endpoints_and_default_cables_remain_visible() {
        let devices = vec![Device {
            id: 1,
            name: "Drive".into(),
            port: "port".into(),
            baud_rate: 115_200,
        }];
        let profile = Profile {
            id: 1,
            name: "Test".into(),
            connections: vec![Connection {
                chip: HardwareChip::Ym2151,
                source: ChipInstance::Secondary,
                device_id: 1,
                target: ChipInstance::Primary,
            }],
        };
        RoutingConfig {
            devices: devices.clone(),
            profiles: vec![profile.clone()],
            selected_profile: Some(1),
        }
        .validate()
        .unwrap();
        let graph = build(&devices, &HashMap::new(), Some(&profile));
        assert_eq!(graph.sources.len(), 34);
        assert_eq!(graph.devices[0].endpoints.len(), 1);
        assert!(!graph.devices[0].endpoints[0].available);
        assert_eq!(graph.cables.iter().filter(|c| c.hardware).count(), 1);
        let cable = graph.cables.iter().find(|c| c.hardware).unwrap();
        assert!(!cable.available);
        assert_eq!(cable.target_x, DEVICE_X);
        assert_eq!(cable.target_y, graph.devices[0].endpoints[0].y + ROW / 2.);
        assert_eq!(graph.devices[0].top, 0.);
        assert_eq!(graph.height, graph.emulator_height);
        assert!(
            graph
                .cables
                .iter()
                .filter(|c| !c.hardware)
                .all(|c| c.target_x == TARGET_X)
        );
    }

    #[test]
    fn advertised_endpoints_are_deduplicated_and_keep_instances() {
        let device = Device {
            id: 1,
            name: "Drive".into(),
            port: "port".into(),
            baud_rate: 115_200,
        };
        let entry = ndsif::CapabilityEntry {
            write_id: 0x54,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        let statuses = HashMap::from([(
            1,
            DeviceStatus::Connected(vec![
                entry,
                ndsif::CapabilityEntry { page: 1, ..entry },
                ndsif::CapabilityEntry {
                    instance: 1,
                    ..entry
                },
            ]),
        )]);
        let graph = build(&[device], &statuses, None);
        assert_eq!(graph.devices[0].endpoints.len(), 2);
        assert!(
            graph.devices[0]
                .endpoints
                .iter()
                .all(|endpoint| endpoint.available)
        );
        assert!(graph.cables.iter().all(|cable| !cable.hardware));
    }

    #[test]
    fn two_independent_device_routes_survive_disconnect_and_restore_emulation() {
        let devices = vec![
            Device {
                id: 1,
                name: "One".into(),
                port: "one".into(),
                baud_rate: 115_200,
            },
            Device {
                id: 2,
                name: "Two".into(),
                port: "two".into(),
                baud_rate: 115_200,
            },
        ];
        let entry = ndsif::CapabilityEntry {
            write_id: 0x54,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        let mut statuses = HashMap::from([
            (1, DeviceStatus::Connected(vec![entry])),
            (2, DeviceStatus::Connected(vec![entry])),
        ]);
        let mut config = RoutingConfig {
            devices,
            ..RoutingConfig::default()
        };
        config.add_profile("Both").unwrap();
        config
            .set_route(
                HardwareChip::Ym2151,
                ChipInstance::Primary,
                Some((1, ChipInstance::Primary)),
            )
            .unwrap();
        config
            .set_route(
                HardwareChip::Ym2151,
                ChipInstance::Secondary,
                Some((2, ChipInstance::Primary)),
            )
            .unwrap();
        let graph = build(&config.devices, &statuses, config.profiles.first());
        let hardware: Vec<_> = graph.cables.iter().filter(|c| c.hardware).collect();
        assert_eq!(hardware.len(), 2);
        assert!(hardware.iter().all(|c| c.available));
        assert_eq!(graph.devices[0].top, 0.);
        assert_eq!(graph.devices[1].top, graph.devices[0].height + GAP);
        assert_eq!(hardware[0].target_x, DEVICE_X);
        assert_eq!(hardware[1].target_x, DEVICE_X);
        assert_ne!(hardware[0].target_y, hardware[1].target_y);
        statuses.remove(&1);
        let offline = build(&config.devices, &statuses, config.profiles.first());
        assert_eq!(
            offline
                .cables
                .iter()
                .filter(|c| c.hardware && !c.available)
                .count(),
            1
        );
        assert_eq!(config.profiles[0].connections.len(), 2);
        config
            .set_route(HardwareChip::Ym2151, ChipInstance::Primary, None)
            .unwrap();
        let restored = build(&config.devices, &statuses, config.profiles.first());
        assert_eq!(restored.cables.iter().filter(|c| c.hardware).count(), 1);
    }

    #[test]
    fn device_column_extends_graph_when_it_exceeds_emulator_height() {
        let devices: Vec<_> = (0..20)
            .map(|id| Device {
                id,
                name: format!("Drive {id}"),
                port: format!("port{id}"),
                baud_rate: 115_200,
            })
            .collect();
        let graph = build(&devices, &HashMap::new(), None);
        let last = graph.devices.last().unwrap();
        assert!(graph.height > graph.emulator_height);
        assert_eq!(graph.height, last.top + last.height);
        assert!(TARGET_X + TARGET_WIDTH < DEVICE_X);
        assert!(DEVICE_X + DEVICE_WIDTH <= WIDTH);
    }
}

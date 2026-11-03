//! Deterministic patchbay layout and endpoint selection.

use std::collections::HashMap;

use crate::DeviceStatus;
use crate::routing::{ChipInstance, Connection, Device, HardwareChip};

pub const WIDTH: f32 = 866.;
pub const SOURCE_X: f32 = 336.;
pub const SOURCE_WIDTH: f32 = 180.;
pub const TARGET_X: f32 = 16.;
pub const TARGET_WIDTH: f32 = 200.;
pub const DEVICE_X: f32 = 620.;
pub const DEVICE_WIDTH: f32 = 230.;
pub const HEADER: f32 = 40.;
pub const DEVICE_HEADER: f32 = 112.;
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
    pub extra_labels: Vec<String>,
}

#[derive(Debug)]
pub struct Cable {
    pub source_x: f32,
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

fn unnumbered_name(name: &str) -> &str {
    name.rsplit_once(" #")
        .filter(|(_, suffix)| suffix.parse::<usize>().is_ok_and(|number| number >= 2))
        .map_or(name, |(model, _)| model)
}

pub fn build(
    devices: &[Device],
    statuses: &HashMap<u64, DeviceStatus>,
    routes: &[Connection],
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
                y: next_top + DEVICE_HEADER + ROW * index as f32,
                available,
            })
            .collect();
        let mut extra_labels: Vec<_> = crate::capability_labels(entries)
            .into_iter()
            .filter(|label| {
                !endpoints.iter().any(|endpoint| {
                    *label
                        == format!(
                            "{} #{}",
                            endpoint.chip.name(),
                            u16::from(endpoint.instance.number()) + 1
                        )
                })
            })
            .collect();
        if connected && entries.is_empty() {
            extra_labels.push("No chips advertised".into());
        }
        let height = DEVICE_HEADER + ROW * (endpoints.len() + extra_labels.len()) as f32 + 8.;
        boxes.push(DeviceBox {
            id: device.id,
            name: if device.name.is_empty() {
                if device.port.is_empty() {
                    "NanoDrive".into()
                } else {
                    device.port.clone()
                }
            } else {
                if connected {
                    device.name.clone()
                } else {
                    unnumbered_name(&device.name).to_owned()
                }
            },
            connected,
            top: next_top,
            height,
            endpoints,
            extra_labels,
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
                    source_x: SOURCE_X + SOURCE_WIDTH,
                    source_y: row_y(index) + ROW / 2.,
                    target_x: DEVICE_X,
                    target_y: endpoint.y + ROW / 2.,
                    hardware: true,
                    available: endpoint.available,
                }
            } else {
                Cable {
                    source_x: SOURCE_X,
                    source_y: row_y(index) + ROW / 2.,
                    target_x: TARGET_X + TARGET_WIDTH,
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
    use crate::routing::{Connection, SessionRouting};

    #[test]
    fn offline_device_names_do_not_show_stale_connection_numbers() {
        let devices = vec![
            Device {
                id: 1,
                name: "NanoDrive 7 #2".into(),
                port: "port1".into(),
                baud_rate: 115_200,
            },
            Device {
                id: 2,
                name: "NanoDrive 7 #2".into(),
                port: "port2".into(),
                baud_rate: 115_200,
            },
        ];
        let statuses = HashMap::from([(2, DeviceStatus::Connected(vec![]))]);
        let graph = build(&devices, &statuses, &[]);
        assert_eq!(graph.devices[0].name, "NanoDrive 7");
        assert_eq!(graph.devices[1].name, "NanoDrive 7 #2");
    }

    #[test]
    fn saved_offline_endpoints_and_default_cables_remain_visible() {
        let devices = vec![Device {
            id: 1,
            name: "Drive".into(),
            port: "port".into(),
            baud_rate: 115_200,
        }];
        let routes = vec![Connection {
            chip: HardwareChip::Ym2151,
            source: ChipInstance::Secondary,
            device_id: 1,
            target: ChipInstance::Primary,
        }];
        let graph = build(&devices, &HashMap::new(), &routes);
        assert_eq!(graph.sources.len(), 36);
        assert_eq!(graph.devices[0].endpoints.len(), 1);
        assert!(!graph.devices[0].endpoints[0].available);
        assert_eq!(graph.cables.iter().filter(|c| c.hardware).count(), 1);
        let cable = graph.cables.iter().find(|c| c.hardware).unwrap();
        assert!(!cable.available);
        assert_eq!(cable.source_x, SOURCE_X + SOURCE_WIDTH);
        assert_eq!(cable.target_x, DEVICE_X);
        assert_eq!(cable.target_y, graph.devices[0].endpoints[0].y + ROW / 2.);
        assert_eq!(graph.devices[0].top, 0.);
        assert_eq!(graph.height, graph.emulator_height);
        assert!(
            graph
                .cables
                .iter()
                .filter(|c| !c.hardware)
                .all(|c| c.source_x == SOURCE_X && c.target_x == TARGET_X + TARGET_WIDTH)
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
        let graph = build(&[device], &statuses, &[]);
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
    fn nd7_aliases_and_ay_are_not_shown_as_unknown_or_duplicates() {
        let device = Device {
            id: 1,
            name: "NanoDrive 7".into(),
            port: "port".into(),
            baud_rate: 115_200,
        };
        let entry = ndsif::CapabilityEntry {
            write_id: 0x55,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        let entries = vec![
            entry,
            ndsif::CapabilityEntry {
                instance: 1,
                ..entry
            },
            ndsif::CapabilityEntry {
                write_id: 0xa5,
                instance: 1,
                ..entry
            },
            ndsif::CapabilityEntry {
                write_id: 0x5e,
                ..entry
            },
            ndsif::CapabilityEntry {
                write_id: 0x5f,
                page: 1,
                ..entry
            },
            ndsif::CapabilityEntry {
                write_id: 0xa0,
                ..entry
            },
            ndsif::CapabilityEntry {
                write_id: 0xa0,
                instance: 1,
                ..entry
            },
        ];
        let graph = build(
            &[device],
            &HashMap::from([(1, DeviceStatus::Connected(entries))]),
            &[],
        );
        assert_eq!(graph.devices[0].endpoints.len(), 5);
        assert!(graph.devices[0].extra_labels.is_empty());
    }

    #[test]
    fn nd6_ym2612_page_one_is_grouped_with_primary_endpoint() {
        let device = Device {
            id: 1,
            name: "NanoDrive 6.1".into(),
            port: "port".into(),
            baud_rate: 115_200,
        };
        let page_zero = ndsif::CapabilityEntry {
            write_id: 0x52,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        let page_one = ndsif::CapabilityEntry {
            write_id: 0x53,
            page: 1,
            ..page_zero
        };
        let entries = vec![page_zero, page_one];
        let mut routing = SessionRouting::default();
        routing.refresh(
            std::slice::from_ref(&device),
            &HashMap::from([(1, entries.clone())]),
        );
        let graph = build(
            std::slice::from_ref(&device),
            &HashMap::from([(1, DeviceStatus::Connected(entries))]),
            routing.connections(),
        );
        assert_eq!(graph.devices[0].endpoints.len(), 1);
        assert_eq!(graph.devices[0].endpoints[0].chip, HardwareChip::Ym2612);
        assert!(graph.devices[0].extra_labels.is_empty());
        assert_eq!(
            graph.cables.iter().filter(|cable| cable.hardware).count(),
            1
        );

        let graph = build(
            &[device],
            &HashMap::from([(1, DeviceStatus::Connected(vec![page_one]))]),
            &[],
        );
        assert!(graph.devices[0].endpoints.is_empty());
        assert_eq!(graph.devices[0].extra_labels, ["YM2612 #1"]);
    }

    #[test]
    fn connected_device_shows_unrouteable_and_empty_capabilities() {
        let device = Device {
            id: 1,
            name: "Drive".into(),
            port: "port".into(),
            baud_rate: 115_200,
        };
        let entry = ndsif::CapabilityEntry {
            write_id: 0xff,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        let graph = build(
            std::slice::from_ref(&device),
            &HashMap::from([(1, DeviceStatus::Connected(vec![entry]))]),
            &[],
        );
        assert_eq!(graph.devices[0].extra_labels, ["Unknown target 0xff #1"]);
        let graph = build(
            &[device],
            &HashMap::from([(1, DeviceStatus::Connected(vec![]))]),
            &[],
        );
        assert_eq!(graph.devices[0].extra_labels, ["No chips advertised"]);
    }

    #[test]
    fn two_device_routes_reassign_after_disconnect() {
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
        let mut routing = SessionRouting::default();
        let available = HashMap::from([(1, vec![entry]), (2, vec![entry])]);
        routing.refresh(&devices, &available);
        let graph = build(&devices, &statuses, routing.connections());
        let hardware: Vec<_> = graph.cables.iter().filter(|c| c.hardware).collect();
        assert_eq!(hardware.len(), 2);
        assert!(hardware.iter().all(|c| c.available));
        assert_eq!(graph.devices[0].top, 0.);
        assert_eq!(graph.devices[1].top, graph.devices[0].height + GAP);
        assert_eq!(hardware[0].target_x, DEVICE_X);
        assert_eq!(hardware[1].target_x, DEVICE_X);
        assert_ne!(hardware[0].target_y, hardware[1].target_y);
        statuses.remove(&1);
        routing.reset(&devices, &HashMap::from([(2, vec![entry])]));
        let restored = build(&devices, &statuses, routing.connections());
        assert_eq!(restored.cables.iter().filter(|c| c.hardware).count(), 1);
        assert!(restored.cables.iter().all(|c| c.available));
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
        let graph = build(&devices, &HashMap::new(), &[]);
        let last = graph.devices.last().unwrap();
        assert!(graph.height > graph.emulator_height);
        assert_eq!(graph.height, last.top + last.height);
        assert!(TARGET_X + TARGET_WIDTH < SOURCE_X);
        assert!(SOURCE_X + SOURCE_WIDTH < DEVICE_X);
        assert!(DEVICE_X + DEVICE_WIDTH <= WIDTH);
    }
}

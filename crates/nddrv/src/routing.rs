//! Persisted NanoDrive devices and explicit chip routing profiles.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, bail};
use chipstream::sequencer::{Chip, Instance};
use ndsif_transport::NdsifRoute;
use serde::{Deserialize, Serialize};

/// Chip types that can be routed to an NDSIF device.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub enum HardwareChip {
    Sn76489,
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
    pub const ALL: [Self; 13] = [
        Self::Sn76489,
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

    fn sequencer_chip(self) -> Chip {
        match self {
            Self::Sn76489 => Chip::Sn76489,
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

    pub fn advertised(
        self,
        instance: ChipInstance,
        capabilities: &[ndsif::CapabilityEntry],
    ) -> bool {
        !(self == Self::Okim6258 && instance == ChipInstance::Secondary)
            && capabilities.iter().any(|entry| {
                entry.write_id == self.write_id(instance) && entry.instance == instance.number()
            })
    }
}

/// Source or destination chip instance.
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

#[derive(Debug, PartialEq, Eq)]
pub struct PlaybackPlan {
    pub device_ids: Vec<u64>,
    pub routes: Vec<NdsifRoute>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: u64,
    pub name: String,
    pub port: String,
    pub baud_rate: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub chip: HardwareChip,
    pub source: ChipInstance,
    pub device_id: u64,
    pub target: ChipInstance,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: u64,
    pub name: String,
    pub connections: Vec<Connection>,
}

/// Unconnected source instances use emulation. Device availability is runtime
/// state and is deliberately not stored in the configuration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingConfig {
    pub devices: Vec<Device>,
    pub profiles: Vec<Profile>,
    pub selected_profile: Option<u64>,
}

impl RoutingConfig {
    pub fn add_profile(&mut self, name: &str) -> Result<u64> {
        let name = name.trim();
        if name.is_empty() {
            bail!("Profile name is required");
        }
        if self.profiles.iter().any(|profile| profile.name == name) {
            bail!("A profile with this name already exists");
        }
        let id = self
            .profiles
            .iter()
            .map(|profile| profile.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .context("No more profile IDs are available")?;
        self.profiles.push(Profile {
            id,
            name: name.into(),
            connections: Vec::new(),
        });
        self.selected_profile = Some(id);
        Ok(id)
    }

    pub fn set_route(
        &mut self,
        chip: HardwareChip,
        source: ChipInstance,
        target: Option<(u64, ChipInstance)>,
    ) -> Result<()> {
        let id = self
            .selected_profile
            .context("Select a routing profile first")?;
        let mut updated = self.clone();
        let profile = updated
            .profiles
            .iter_mut()
            .find(|profile| profile.id == id)
            .context("selected routing profile does not exist")?;
        profile
            .connections
            .retain(|route| route.chip != chip || route.source != source);
        if let Some((device_id, target)) = target {
            profile.connections.push(Connection {
                chip,
                source,
                device_id,
                target,
            });
        }
        updated.validate()?;
        *self = updated;
        Ok(())
    }

    /// Resolve the selected profile into stable device indices and chip routes.
    pub fn playback_plan(&self) -> Result<Option<PlaybackPlan>> {
        self.validate()?;
        let Some(id) = self.selected_profile else {
            return Ok(None);
        };
        let profile = self
            .profiles
            .iter()
            .find(|profile| profile.id == id)
            .context("selected routing profile does not exist")?;
        if profile.connections.is_empty() {
            return Ok(None);
        }

        let mut device_ids = Vec::new();
        let mut indices = HashMap::new();
        let mut routes = Vec::with_capacity(profile.connections.len());
        for connection in &profile.connections {
            let device = *indices.entry(connection.device_id).or_insert_with(|| {
                let index = device_ids.len();
                device_ids.push(connection.device_id);
                index
            });
            routes.push(NdsifRoute {
                chip: connection.chip.sequencer_chip(),
                source: connection.source.sequencer_instance(),
                device,
                target: connection.target.sequencer_instance(),
            });
        }
        Ok(Some(PlaybackPlan { device_ids, routes }))
    }

    /// Validate identities and connections without opening any serial port.
    pub fn validate(&self) -> Result<()> {
        let mut device_ids = HashSet::new();
        let mut ports = HashSet::new();
        for device in &self.devices {
            if !device_ids.insert(device.id) {
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

        let mut profile_ids = HashSet::new();
        for profile in &self.profiles {
            if !profile_ids.insert(profile.id) {
                bail!("duplicate routing profile ID {}", profile.id);
            }
            let mut sources = HashSet::new();
            let mut targets = HashSet::new();
            for connection in &profile.connections {
                if !device_ids.contains(&connection.device_id) {
                    bail!(
                        "routing profile {} refers to unknown device {}",
                        profile.id,
                        connection.device_id
                    );
                }
                if !sources.insert((connection.chip, connection.source)) {
                    bail!("routing profile {} has a duplicate source", profile.id);
                }
                if !targets.insert((connection.device_id, connection.chip, connection.target)) {
                    bail!("routing profile {} has a duplicate target", profile.id);
                }
            }
        }
        if self
            .selected_profile
            .is_some_and(|id| !profile_ids.contains(&id))
        {
            bail!("selected routing profile does not exist");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> RoutingConfig {
        RoutingConfig {
            devices: vec![
                Device {
                    id: 1,
                    name: "First".into(),
                    port: "port-a".into(),
                    baud_rate: 115_200,
                },
                Device {
                    id: 2,
                    name: "Second".into(),
                    port: "port-b".into(),
                    baud_rate: 115_200,
                },
            ],
            profiles: vec![Profile {
                id: 3,
                name: "Both".into(),
                connections: vec![
                    Connection {
                        chip: HardwareChip::Ym2151,
                        source: ChipInstance::Primary,
                        device_id: 1,
                        target: ChipInstance::Primary,
                    },
                    Connection {
                        chip: HardwareChip::Ym2151,
                        source: ChipInstance::Secondary,
                        device_id: 2,
                        target: ChipInstance::Primary,
                    },
                ],
            }],
            selected_profile: Some(3),
        }
    }

    #[test]
    fn multiple_devices_and_profiles_round_trip_without_live_connections() {
        let config = example();
        config.validate().unwrap();
        let json = serde_json::to_string(&config).unwrap();
        let decoded: RoutingConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, config);
        decoded.validate().unwrap();
        assert!(decoded.playback_plan().unwrap().is_some());
        assert!(RoutingConfig::default().playback_plan().unwrap().is_none());
    }

    #[test]
    fn playback_plan_preserves_instances_and_reuses_device_indices() {
        let mut config = example();
        config.profiles[0].connections.reverse();
        config.profiles[0].connections.push(Connection {
            chip: HardwareChip::Sn76489,
            source: ChipInstance::Primary,
            device_id: 2,
            target: ChipInstance::Secondary,
        });
        let plan = config.playback_plan().unwrap().unwrap();
        assert_eq!(plan.device_ids, [2, 1]);
        assert_eq!(
            plan.routes,
            [
                NdsifRoute {
                    chip: Chip::Ym2151,
                    source: Instance::Secondary,
                    device: 0,
                    target: Instance::Primary,
                },
                NdsifRoute {
                    chip: Chip::Ym2151,
                    source: Instance::Primary,
                    device: 1,
                    target: Instance::Primary,
                },
                NdsifRoute {
                    chip: Chip::Sn76489,
                    source: Instance::Primary,
                    device: 0,
                    target: Instance::Secondary,
                },
            ]
        );
        config.profiles[0].connections.clear();
        assert!(config.playback_plan().unwrap().is_none());
    }

    #[test]
    fn every_hardware_chip_has_its_matching_sequencer_chip() {
        let expected = [
            Chip::Sn76489,
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
        ];
        assert_eq!(
            HardwareChip::ALL.map(HardwareChip::sequencer_chip),
            expected
        );
    }

    #[test]
    fn editing_profiles_preserves_routes_and_rejects_duplicate_targets_atomically() {
        let mut config = example();
        let id = config.add_profile("  New route  ").unwrap();
        assert_eq!(config.selected_profile, Some(id));
        assert_eq!(config.profiles.last().unwrap().name, "New route");
        assert!(config.add_profile("New route").is_err());
        assert!(config.add_profile("  ").is_err());
        config
            .set_route(
                HardwareChip::Ym2151,
                ChipInstance::Primary,
                Some((1, ChipInstance::Secondary)),
            )
            .unwrap();
        let before = config.clone();
        assert!(
            config
                .set_route(
                    HardwareChip::Ym2151,
                    ChipInstance::Secondary,
                    Some((1, ChipInstance::Secondary))
                )
                .unwrap_err()
                .to_string()
                .contains("duplicate target")
        );
        assert_eq!(config, before);
        assert!(
            config
                .set_route(
                    HardwareChip::Ym2151,
                    ChipInstance::Secondary,
                    Some((99, ChipInstance::Primary))
                )
                .is_err()
        );
        assert_eq!(config, before);
        config
            .set_route(
                HardwareChip::Ym2151,
                ChipInstance::Primary,
                Some((2, ChipInstance::Primary)),
            )
            .unwrap();
        assert_eq!(config.profiles.last().unwrap().connections.len(), 1);
        config
            .set_route(HardwareChip::Ym2151, ChipInstance::Primary, None)
            .unwrap();
        assert!(config.profiles.last().unwrap().connections.is_empty());
        config.validate().unwrap();
    }

    #[test]
    fn advertised_targets_require_matching_chip_and_instance() {
        let entry = ndsif::CapabilityEntry {
            write_id: 0x54,
            instance: 0,
            page: 0,
            features: 0,
            clock_mode: ndsif::ClockMode::Variable,
            fixed_clock_hz: 0,
        };
        assert!(HardwareChip::Ym2151.advertised(ChipInstance::Primary, &[entry]));
        assert!(!HardwareChip::Ym2151.advertised(ChipInstance::Secondary, &[entry]));
        assert!(!HardwareChip::Ym2203.advertised(ChipInstance::Primary, &[entry]));
        let secondary_psg = ndsif::CapabilityEntry {
            write_id: 0x30,
            instance: 1,
            ..entry
        };
        assert!(HardwareChip::Sn76489.advertised(ChipInstance::Secondary, &[secondary_psg]));
        let secondary_oki = ndsif::CapabilityEntry {
            write_id: 0xb7,
            instance: 1,
            ..entry
        };
        assert!(!HardwareChip::Okim6258.advertised(ChipInstance::Secondary, &[secondary_oki]));
    }

    #[test]
    fn rejects_dangling_and_conflicting_routes() {
        let mut config = example();
        config.profiles[0].connections[1].device_id = 99;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("unknown device")
        );
        let mut config = example();
        config.profiles[0].connections[1].source = ChipInstance::Primary;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate source")
        );
        let mut config = example();
        config.profiles[0].connections[1].device_id = 1;
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate target")
        );
        let mut config = example();
        config.devices[1].port = "port-a".into();
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("serial port")
        );
        let mut config = example();
        config.selected_profile = Some(99);
        assert!(
            config
                .validate()
                .unwrap_err()
                .to_string()
                .contains("does not exist")
        );
        assert!(config.playback_plan().is_err());
    }
}

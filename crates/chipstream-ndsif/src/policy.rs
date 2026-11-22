//! Device-specific policies for selected hardware routes.
//!
//! A policy receives selected hardware routes before configuration. The
//! direct policy accepts them unchanged; NanoDrive policies also handle
//! shared resources, AY compatibility, and device-specific pan correction.

use crate::NdsifAyCompatibility;
use std::collections::HashSet;

use crate::{NdsifNd7Validator, NdsifRoute, NdsifRoutingError};

/// Options for constructing a selected hardware route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NdsifRouteOptions {
    /// AY-family approximation accepted by the selected target.
    pub ay_compatibility: NdsifAyCompatibility,
    /// Swap YM2151 left and right output bits before sending register writes.
    pub invert_ym2151_pan: bool,
}

/// Validate a hardware selection and choose its backend compatibility.
///
/// Called once per selected hardware instance, before it is configured. The
/// same policy instance is shared by all chip factories in one registry.
/// Software fallbacks do not invoke the policy. Errors stop construction and
/// never cause a different device or software fallback to be selected.
/// Chip instances are constructed sequentially, so earlier devices may already
/// be configured when a later selection fails.
pub trait NdsifRoutePolicy {
    /// Check the selected route and effective source clock.
    ///
    /// # Errors
    ///
    /// Return a descriptive error if the selected route cannot be used.
    fn select(&mut self, route: &NdsifRoute, clock: u32) -> Result<NdsifRouteOptions, String>;
}

/// Default policy for devices without special route constraints.
#[derive(Default)]
pub struct NdsifDirectPolicy;

impl NdsifRoutePolicy for NdsifDirectPolicy {
    /// Accept the route without additional policy checks.
    fn select(&mut self, _: &NdsifRoute, _: u32) -> Result<NdsifRouteOptions, String> {
        Ok(NdsifRouteOptions::default())
    }
}

/// NanoDrive 7 shared-resource and AY-compatibility policy.
///
/// The policy tracks selected resources and must be created anew for each
/// playback registry.
#[derive(Default)]
pub struct NdsifNd7Policy {
    /// Transport indices corresponding to NanoDrive 7 devices.
    devices: HashSet<usize>,
    /// Resources already selected for this playback registry.
    validator: NdsifNd7Validator,
}

impl NdsifNd7Policy {
    /// Create a policy for identified NanoDrive 7 indices in the transport list.
    ///
    /// # Errors
    ///
    /// Returns [`NdsifRoutingError::UnknownDevice`] if an index is outside
    /// the transport list of `device_count` entries. This count must match the
    /// list subsequently supplied to the factory.
    pub fn new(
        devices: impl IntoIterator<Item = usize>,
        device_count: usize,
    ) -> Result<Self, NdsifRoutingError> {
        let devices: HashSet<_> = devices.into_iter().collect();
        if let Some(&index) = devices.iter().find(|&&index| index >= device_count) {
            return Err(NdsifRoutingError::UnknownDevice(index));
        }
        Ok(Self {
            devices,
            validator: NdsifNd7Validator::default(),
        })
    }
}

impl NdsifRoutePolicy for NdsifNd7Policy {
    /// Validate NanoDrive 7 resource sharing and select its AY compatibility.
    fn select(&mut self, route: &NdsifRoute, clock: u32) -> Result<NdsifRouteOptions, String> {
        if self.devices.contains(&route.device) {
            self.validator.add(route, clock)?;
            Ok(NdsifRouteOptions {
                ay_compatibility: NdsifAyCompatibility::Ay8910AndYm2149,
                ..Default::default()
            })
        } else {
            Ok(NdsifRouteOptions::default())
        }
    }
}

/// NanoDrive 7 resource validation and NanoDrive 8 YM2151 pan correction.
///
/// Create a new policy for each playback registry.
pub struct NdsifNanoDrivePolicy {
    nd7: NdsifNd7Policy,
    nd8_devices: HashSet<usize>,
}

impl NdsifNanoDrivePolicy {
    /// Create a policy for identified NanoDrive 7 and 8 transport indices.
    ///
    /// # Errors
    ///
    /// Returns [`NdsifRoutingError::UnknownDevice`] for an out-of-range index.
    pub fn new(
        nd7_devices: impl IntoIterator<Item = usize>,
        nd8_devices: impl IntoIterator<Item = usize>,
        device_count: usize,
    ) -> Result<Self, NdsifRoutingError> {
        let nd7 = NdsifNd7Policy::new(nd7_devices, device_count)?;
        let nd8_devices: HashSet<_> = nd8_devices.into_iter().collect();
        if let Some(&index) = nd8_devices.iter().find(|&&index| index >= device_count) {
            return Err(NdsifRoutingError::UnknownDevice(index));
        }
        Ok(Self { nd7, nd8_devices })
    }
}

impl NdsifRoutePolicy for NdsifNanoDrivePolicy {
    fn select(&mut self, route: &NdsifRoute, clock: u32) -> Result<NdsifRouteOptions, String> {
        let mut options = self.nd7.select(route, clock)?;
        options.invert_ym2151_pan = self.nd8_devices.contains(&route.device)
            && route.chip == chipstream::sequencer::Chip::Ym2151;
        Ok(options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nd7_policy_rejects_unknown_device_indices() {
        assert!(matches!(
            NdsifNd7Policy::new([1], 1),
            Err(NdsifRoutingError::UnknownDevice(1))
        ));
        assert!(NdsifNd7Policy::new([0], 1).is_ok());
        assert!(NdsifNd7Policy::new([], 0).is_ok());
    }

    #[test]
    fn nanodrive_policy_combines_nd7_and_nd8_behavior() {
        use chipstream::sequencer::{Chip, Instance};
        assert!(matches!(
            NdsifNanoDrivePolicy::new([], [1], 1),
            Err(NdsifRoutingError::UnknownDevice(1))
        ));
        let mut policy = NdsifNanoDrivePolicy::new([0], [1], 3).unwrap();
        let route = |chip, device| NdsifRoute {
            chip,
            source: Instance::Primary,
            device,
            target: Instance::Primary,
        };
        assert_eq!(
            policy
                .select(&route(Chip::Ay8910, 0), 2_000_000)
                .unwrap()
                .ay_compatibility,
            NdsifAyCompatibility::Ay8910AndYm2149
        );
        assert!(
            policy
                .select(&route(Chip::Ym2151, 1), 4_000_000)
                .unwrap()
                .invert_ym2151_pan
        );
        assert!(
            !policy
                .select(&route(Chip::Ym2151, 2), 4_000_000)
                .unwrap()
                .invert_ym2151_pan
        );
        assert!(
            !policy
                .select(&route(Chip::Ym2203, 1), 4_000_000)
                .unwrap()
                .invert_ym2151_pan
        );
    }
}

//! Device-specific policies for selected hardware routes.

use crate::NdsifAyCompatibility;
use std::collections::HashSet;

use crate::{NdsifNd7Validator, NdsifRoute, NdsifRoutingError};

/// Options for constructing a selected hardware route.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NdsifRouteOptions {
    /// AY-family approximation accepted by the selected target.
    pub ay_compatibility: NdsifAyCompatibility,
}

/// Validate a hardware selection and choose its backend compatibility.
///
/// Called once per selected hardware instance, before it is configured. The
/// same policy instance is shared by all chip factories in one registry.
/// Software fallbacks do not invoke the policy. Errors stop construction and
/// never cause a different device or software fallback to be selected.
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
    devices: HashSet<usize>,
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
            })
        } else {
            Ok(NdsifRouteOptions::default())
        }
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
}

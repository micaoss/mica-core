//! Reconciler for the `services` subtree: the user's own services, through
//! [`crate::user_units`].

use micad_settings::Settings;

use super::Reconciler;
use crate::user_units::UserUnits;

/// Reconciler for the `services` subtree.
pub struct ServicesReconciler {
    units: UserUnits,
}

impl ServicesReconciler {
    /// The host's, on `init`.
    pub fn production(init: micad_settings::Init) -> Self {
        Self {
            units: UserUnits::production(init),
        }
    }
}

#[async_trait::async_trait]
impl Reconciler for ServicesReconciler {
    fn name(&self) -> &'static str {
        "services"
    }

    fn subtree(&self) -> &'static str {
        "services"
    }

    async fn apply(&self, settings: &Settings) -> anyhow::Result<serde_json::Value> {
        self.units.apply(&settings.services).await
    }
}

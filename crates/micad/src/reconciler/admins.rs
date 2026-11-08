//! Cloud administrators reconciler: whether the cloud's SSH users are
//! administrators (docs/mica-core.md section 3.8).
//!
//! `mica-gce-agent` adds the users the cloud's metadata names to Base's group
//! `mica-admins`; the grant is not the group but the sudo rule for it, which
//! micad alone renders, into `/run/mica/sudoers.d`, which Base's sudoers
//! includes. The switch is `access.cloudAdmins.enabled`, else the product's
//! `CLOUD_ADMINS=on`: on renders the rule, off removes it, so membership
//! grants nothing at once. `/run/mica/cloud-admins` (`on` or `off`) says
//! which to the agent, which grants `--admin` only while it reads `on`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use micad_settings::Settings;
use serde_json::json;

use super::Reconciler;
use crate::fswrite::write_config_if_changed;

/// The rule, sudo's `0440`.
const RULE: &str = "%mica-admins ALL=(ALL:ALL) NOPASSWD: ALL\n";
const RULE_MODE: u32 = 0o440;
/// The switch as the agent reads it, world-readable.
const SWITCH_MODE: u32 = 0o644;

/// Reconciler for the `access.cloudAdmins` settings subtree.
pub struct CloudAdminsReconciler {
    /// The rule file, a sudo `@includedir` entry: no dot in its name.
    rule: PathBuf,
    /// The switch the agent reads.
    switch: PathBuf,
    product_default: bool,
}

impl CloudAdminsReconciler {
    pub(crate) fn new(rule: PathBuf, switch: PathBuf, product_default: bool) -> Self {
        Self {
            rule,
            switch,
            product_default,
        }
    }

    /// The production paths, for this root's product.
    pub fn production() -> Self {
        let conf = std::fs::read_to_string(micad_settings::PRODUCT_FILE).unwrap_or_default();
        Self::new(
            PathBuf::from("/run/mica/sudoers.d/50-mica-admins"),
            PathBuf::from("/run/mica/cloud-admins"),
            micad_settings::product_cloud_admins(&conf),
        )
    }
}

#[async_trait::async_trait]
impl Reconciler for CloudAdminsReconciler {
    fn name(&self) -> &'static str {
        "cloudAdmins"
    }

    fn subtree(&self) -> &'static str {
        "access.cloudAdmins"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let switch = settings.access.cloud_admins;
        let enabled = switch.effective(self.product_default);
        // The rule is written before the switch says `on` and removed
        // before it says `off`: the agent never reads `off` while the rule
        // still grants.
        if enabled {
            write_config_if_changed(&self.rule, RULE, RULE_MODE)?;
        } else {
            match std::fs::remove_file(&self.rule) {
                Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                    return Err(err).with_context(|| format!("remove {}", self.rule.display()));
                }
                _ => {}
            }
        }
        write_config_if_changed(
            &self.switch,
            if enabled { "on\n" } else { "off\n" },
            SWITCH_MODE,
        )?;
        Ok(json!({
            "enabled": enabled,
            "source": if switch.enabled.is_some() { "setting" } else { "product" },
            "productDefault": self.product_default,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn reconciler(dir: &std::path::Path, product_default: bool) -> CloudAdminsReconciler {
        CloudAdminsReconciler::new(
            dir.join("sudoers.d/50-mica-admins"),
            dir.join("cloud-admins"),
            product_default,
        )
    }

    #[tokio::test]
    async fn the_rule_follows_the_switch_else_the_product() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rule = dir.path().join("sudoers.d/50-mica-admins");
        let switch = dir.path().join("cloud-admins");
        let mut settings = Settings::default();

        // A product that names no grant renders none.
        let state = reconciler(dir.path(), false)
            .apply(&settings)
            .await
            .expect("apply");
        assert_eq!(
            state,
            json!({ "enabled": false, "source": "product", "productDefault": false })
        );
        assert!(!rule.exists());
        assert_eq!(std::fs::read_to_string(&switch).expect("switch"), "off\n");

        // The product's grant.
        let on = reconciler(dir.path(), true);
        let state = on.apply(&settings).await.expect("apply");
        assert_eq!(
            state,
            json!({ "enabled": true, "source": "product", "productDefault": true })
        );
        assert_eq!(std::fs::read_to_string(&rule).expect("rule"), RULE);
        let mode = std::fs::metadata(&rule).expect("rule").permissions().mode();
        assert_eq!(mode & 0o777, 0o440);
        assert_eq!(std::fs::read_to_string(&switch).expect("switch"), "on\n");

        // Turned off here: the rule goes, whatever the product says.
        settings.access.cloud_admins.enabled = Some(false);
        let state = on.apply(&settings).await.expect("apply");
        assert_eq!(
            state,
            json!({ "enabled": false, "source": "setting", "productDefault": true })
        );
        assert!(!rule.exists());
        assert_eq!(std::fs::read_to_string(&switch).expect("switch"), "off\n");
        // Again: nothing to remove is no failure.
        on.apply(&settings).await.expect("apply");

        // Turned on here on a product that grants nothing.
        settings.access.cloud_admins.enabled = Some(true);
        let state = reconciler(dir.path(), false)
            .apply(&settings)
            .await
            .expect("apply");
        assert_eq!(
            state,
            json!({ "enabled": true, "source": "setting", "productDefault": false })
        );
        assert!(rule.exists());
    }
}

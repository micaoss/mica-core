//! Native signed deployment status and actions through mica-deploy.
use std::{path::Path, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::update_lifecycle::UpdateClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootBackend {
    Uefi,
    UbootFit,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Boot {
    pub backend: BootBackend,
    pub deployment_id: String,
    pub entry: String,
    pub kernel_id: String,
    pub rootfs_id: String,
    pub content_verified: bool,
    pub secure_boot: bool,
    pub boot_verified: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct State {
    pub highest_generation: u64,
    pub current: Option<String>,
    pub fallback: Option<String>,
    pub candidate: Option<String>,
    pub failed: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Deployment {
    pub id: String,
    pub file: String,
    pub generation: u64,
    pub tries_left: Option<u8>,
    pub version: String,
    pub kernel_id: String,
    pub kernel_release: String,
    pub rootfs_id: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub boot: Boot,
    pub state: State,
    pub deployments: Vec<Deployment>,
    /// The device's core sets, from `mica-deploy core-status`. Not part of
    /// `status`' own answer, which an older micad reads strictly: asked
    /// separately, and absent when the client predates core sets.
    #[serde(skip)]
    pub core: Option<CoreStatus>,
}

/// One held core set.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreSet {
    pub id: String,
    pub channel: String,
    pub generation: u64,
    pub version: String,
    /// The packages this device composes from it.
    #[serde(default)]
    pub selected: Vec<String>,
}

/// What the runkit composed this boot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreBoot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core_set_id: Option<String>,
    #[serde(default)]
    pub pending: bool,
}

/// The core sets a device holds. Read leniently: the client that prints it
/// may be newer than this daemon and say more.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoreStatus {
    pub current: Option<CoreSet>,
    pub pending: Option<CoreSet>,
    /// The boots the pending set has left to reach healthy.
    pub attempts_left: Option<u8>,
    #[serde(default)]
    pub boot: CoreBoot,
}

impl CoreStatus {
    pub fn parse(text: &str) -> Result<Self> {
        ensure!(text.len() <= 65536, "core set status exceeds bound");
        let status: Self = serde_json::from_str(text)?;
        for set in [&status.current, &status.pending].into_iter().flatten() {
            ensure!(valid_id(&set.id), "invalid core set identity");
        }
        Ok(status)
    }

    /// The phase a pending set puts the device in, if one is pending.
    fn phase(&self) -> Option<(&'static str, String)> {
        let pending = self.pending.as_ref()?;
        Some(if self.boot.core_set_id.as_ref() == Some(&pending.id) {
            (
                "validating",
                "The running core set awaits health confirmation".into(),
            )
        } else {
            (
                "reboot-required",
                format!(
                    "Core set {} ({} generation {}) is installed and awaits reboot",
                    pending.version, pending.channel, pending.generation
                ),
            )
        })
    }
}

/// The prefix of a staged core set in `verified/`: `core-<id>.json`.
pub const CORE_DESCRIPTOR_PREFIX: &str = "core-";

/// The id a staged file in `verified/` carries, and whether it is a core
/// set's: `<id>.json` for a deployment, `core-<id>.json` for a core set.
#[must_use]
pub fn staged_id(path: &Path) -> Option<(&str, bool)> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".json")?;
    let (id, core) = match stem.strip_prefix(CORE_DESCRIPTOR_PREFIX) {
        Some(id) => (id, true),
        None => (stem, false),
    };
    valid_id(id).then_some((id, core))
}

pub fn valid_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Status {
    pub fn parse(text: &str) -> Result<Self> {
        ensure!(text.len() <= 65536, "deployment status exceeds bound");
        let status: Self = serde_json::from_str(text)?;
        ensure!(
            status.deployments.len() <= 16 && status.state.failed.len() <= 128,
            "deployment count exceeds bound"
        );
        for (index, deployment) in status.deployments.iter().enumerate() {
            ensure!(
                valid_id(&deployment.id)
                    && valid_id(&deployment.kernel_id)
                    && valid_id(&deployment.rootfs_id)
                    && deployment.generation > 0
                    && deployment.tries_left.is_none_or(|tries| tries <= 3),
                "invalid deployment identity"
            );
            ensure!(
                !status.deployments[..index]
                    .iter()
                    .any(|other| other.id == deployment.id
                        || other.generation == deployment.generation),
                "ambiguous deployment status"
            );
        }
        let booted = status.booted().context("running deployment is absent")?;
        match status.boot.backend {
            BootBackend::Uefi => {
                ensure!(
                    ["", "+3", "+2-1", "+1-2", "+0-3"].iter().any(|suffix| {
                        status.boot.entry
                            == format!("mica-{}{suffix}.conf", status.boot.deployment_id)
                    }),
                    "running boot entry identity mismatch"
                );
                ensure!(
                    status.boot.boot_verified == status.boot.secure_boot,
                    "inconsistent UEFI verification evidence"
                );
            }
            BootBackend::UbootFit => {
                ensure!(
                    status.boot.entry == format!("fit:{}", status.boot.deployment_id),
                    "running FIT entry identity mismatch"
                );
                ensure!(
                    status.boot.boot_verified && !status.boot.secure_boot,
                    "inconsistent FIT verification evidence"
                );
            }
        }
        ensure!(
            status.boot.content_verified
                && booted.kernel_id == status.boot.kernel_id
                && booted.rootfs_id == status.boot.rootfs_id,
            "running component identity mismatch"
        );
        for id in [
            &status.state.current,
            &status.state.fallback,
            &status.state.candidate,
        ]
        .into_iter()
        .flatten()
        {
            ensure!(
                status
                    .deployments
                    .iter()
                    .any(|deployment| &deployment.id == id),
                "state references an absent deployment"
            );
        }
        ensure!(
            status.state.failed.iter().all(|id| valid_id(id)),
            "invalid failed deployment ID"
        );
        Ok(status)
    }

    pub fn booted(&self) -> Option<&Deployment> {
        self.deployments
            .iter()
            .find(|deployment| deployment.id == self.boot.deployment_id)
    }

    /// Whether something installed still waits for its reboot or its health
    /// confirmation: a deployment or a core set.
    #[must_use]
    pub fn update_pending(&self) -> bool {
        self.state.candidate.is_some()
            || self
                .core
                .as_ref()
                .is_some_and(|core| core.pending.is_some())
    }

    pub fn phase(&self) -> (&'static str, String) {
        // A core set on trial runs over a confirmed deployment, so its phase
        // is the device's.
        if let Some(phase) = self.core.as_ref().and_then(CoreStatus::phase) {
            return phase;
        }
        let running = &self.boot.deployment_id;
        if self.state.failed.contains(running) {
            return (
                "reboot-required",
                "The running deployment was rejected; reboot to the retained fallback".into(),
            );
        }
        if let Some(candidate) = &self.state.candidate {
            if candidate != running {
                return (
                    "reboot-required",
                    format!("Deployment {candidate} is installed and awaits reboot"),
                );
            }
            return (
                "validating",
                "The running candidate awaits health confirmation".into(),
            );
        }
        if self.state.current.as_ref() != Some(running) {
            return (
                "validating",
                "The running deployment awaits health confirmation".into(),
            );
        }
        if self
            .booted()
            .is_some_and(|deployment| deployment.generation < self.state.highest_generation)
            && !self.state.failed.is_empty()
        {
            return (
                "rolled-back",
                "The system is running a retained deployment after a failed update".into(),
            );
        }
        (
            "succeeded",
            "The boot health gate confirmed this deployment".into(),
        )
    }

    pub fn rollback(&self) -> Value {
        let reason = if self.state.candidate.is_some() {
            Some("candidate_pending")
        } else if self.state.current.as_ref() != Some(&self.boot.deployment_id)
            || self.state.failed.contains(&self.boot.deployment_id)
        {
            Some("running_not_confirmed")
        } else if self.state.fallback.as_ref().is_none_or(|id| {
            id == &self.boot.deployment_id
                || self.state.failed.contains(id)
                || !self
                    .deployments
                    .iter()
                    .any(|deployment| &deployment.id == id && deployment.tries_left != Some(0))
        }) {
            Some("no_usable_fallback")
        } else {
            None
        };
        json!({"permitted":reason.is_none(), "target":if reason.is_none() { self.state.fallback.as_ref() } else { None }, "reason":reason})
    }

    pub fn merge_into(&self, entry: &mut serde_json::Map<String, Value>) -> Result<()> {
        for (key, value) in serde_json::to_value(self)?
            .as_object()
            .context("invalid status object")?
        {
            entry.insert(key.clone(), value.clone());
        }
        entry.insert("rollback".into(), self.rollback());
        match &self.core {
            Some(core) => entry.insert("core".into(), serde_json::to_value(core)?),
            None => entry.remove("core"),
        };
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait DeploymentClient: Send + Sync {
    async fn status(&self) -> Result<Status>;
    async fn install(&self, descriptor: &Path) -> Result<()>;
    async fn confirm(&self) -> Result<()>;
    async fn reject(&self, id: &str) -> Result<()>;
    async fn rollback(&self) -> Result<()>;
}

pub struct NativeClient {
    client: Arc<dyn UpdateClient>,
}
impl NativeClient {
    pub fn new(client: Arc<dyn UpdateClient>) -> Self {
        Self { client }
    }

    async fn action(&self, args: Vec<String>, timeout: Duration) -> Result<String> {
        let output = self.client.run(&args, timeout).await?;
        ensure!(
            output.code == Some(0),
            "mica-deploy {}: {}",
            args[0],
            output.stderr.trim()
        );
        ensure!(
            output.stdout.len() <= 65536,
            "deployment output exceeds bound"
        );
        Ok(output.stdout)
    }
}

#[async_trait::async_trait]
impl DeploymentClient for NativeClient {
    async fn status(&self) -> Result<Status> {
        let mut status = Status::parse(
            &self
                .action(vec!["status".into()], Duration::from_secs(30))
                .await?,
        )?;
        // A client from before core sets has no such command, and the device
        // then has no sets to report.
        status.core = match self
            .action(vec!["core-status".into()], Duration::from_secs(30))
            .await
        {
            Ok(text) => Some(CoreStatus::parse(&text)?),
            Err(error) => {
                tracing::debug!(%error, "core set status unavailable");
                None
            }
        };
        Ok(status)
    }
    async fn install(&self, descriptor: &Path) -> Result<()> {
        let objects = descriptor
            .parent()
            .context("missing descriptor directory")?
            .join("objects");
        let core = staged_id(descriptor).is_some_and(|(_, core)| core);
        self.action(
            vec![
                if core { "core-install" } else { "install" }.into(),
                descriptor.to_string_lossy().into_owned(),
                "--objects".into(),
                objects.to_string_lossy().into_owned(),
            ],
            Duration::from_secs(1800),
        )
        .await?;
        Ok(())
    }
    async fn confirm(&self) -> Result<()> {
        self.action(vec!["confirm".into()], Duration::from_secs(30))
            .await?;
        Ok(())
    }
    async fn reject(&self, id: &str) -> Result<()> {
        ensure!(valid_id(id), "invalid deployment ID");
        self.action(vec!["reject".into(), id.into()], Duration::from_secs(30))
            .await?;
        Ok(())
    }
    async fn rollback(&self) -> Result<()> {
        self.action(vec!["rollback".into()], Duration::from_secs(30))
            .await?;
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn fixture() -> Value {
        let current = "a".repeat(64);
        let fallback = "b".repeat(64);
        let kernel = "c".repeat(64);
        let root = "d".repeat(64);
        let deployments = [(&current, 2), (&fallback, 1)].map(|(id, generation)| json!({
            "id":id,"file":format!("mica-{id}.conf"),"generation":generation,"triesLeft":null,
            "version":format!("test-{generation}"),"kernelId":kernel,"kernelRelease":"6.12.107","rootfsId":root,
        }));
        json!({"boot":{"deploymentId":current,"entry":format!("mica-{current}.conf"),"kernelId":kernel,
            "rootfsId":root,"contentVerified":true,"secureBoot":true,"bootVerified":true,"backend":"uefi"},
            "state":{"highestGeneration":2,"current":current,"fallback":fallback,"candidate":null,"failed":[]},"deployments":deployments})
    }

    /// A core set on trial is the device's phase, over whatever the confirmed
    /// deployment under it says, and counts as an update still pending.
    #[test]
    fn a_pending_core_set_drives_the_phase_until_it_settles() {
        let mut status = Status::parse(&fixture().to_string()).unwrap();
        assert!(!status.update_pending());
        let set = |id: &str| CoreSet {
            id: id.repeat(64),
            channel: "general".into(),
            generation: 4,
            version: "0.0.6".into(),
            selected: vec!["micad".into()],
        };
        status.core = Some(CoreStatus {
            current: Some(set("e")),
            pending: Some(set("f")),
            attempts_left: Some(3),
            boot: CoreBoot {
                core_set_id: Some("e".repeat(64)),
                pending: false,
            },
        });
        assert!(status.update_pending());
        let (phase, reason) = status.phase();
        assert_eq!(phase, "reboot-required");
        assert!(reason.contains("0.0.6"), "{reason}");
        status.core.as_mut().unwrap().boot = CoreBoot {
            core_set_id: Some("f".repeat(64)),
            pending: true,
        };
        assert_eq!(status.phase().0, "validating");
        let mut entry = serde_json::Map::new();
        status.merge_into(&mut entry).unwrap();
        assert_eq!(entry["core"]["pending"]["generation"], 4);
        assert_eq!(entry["core"]["attemptsLeft"], 3);
        status.core.as_mut().unwrap().pending = None;
        assert_eq!(status.phase().0, "succeeded");
    }

    /// A staged file says what it is by its name, and by nothing else.
    #[test]
    fn a_staged_file_names_a_deployment_or_a_core_set() {
        let id = "a".repeat(64);
        let path = |name: String| std::path::PathBuf::from("/mica/updates/verified").join(name);
        assert_eq!(
            staged_id(&path(format!("{id}.json"))),
            Some((id.as_str(), false))
        );
        assert_eq!(
            staged_id(&path(format!("core-{id}.json"))),
            Some((id.as_str(), true))
        );
        for name in [
            format!("{id}.partial"),
            "core-.json".into(),
            format!("core-core-{id}.json"),
        ] {
            assert_eq!(staged_id(&path(name.clone())), None, "{name}");
        }
        // The client's own answer is read leniently, and its ids are held to the rule.
        assert!(
            CoreStatus::parse(
                r#"{"current":null,"pending":null,"attemptsLeft":null,"boot":{},"later":1}"#
            )
            .is_ok()
        );
        assert!(CoreStatus::parse(r#"{"current":{"id":"x","channel":"general","generation":1,"version":"1"},"pending":null,"attemptsLeft":null}"#).is_err());
    }

    #[test]
    fn native_confirmation_and_trial_states_drive_the_product_status() {
        let mut value = fixture();
        let status = Status::parse(&value.to_string()).unwrap();
        assert_eq!(status.phase().0, "succeeded");
        assert_eq!(status.rollback()["target"], "b".repeat(64));
        let mut entry = serde_json::Map::from_iter([("install".into(), json!({"status":"done"}))]);
        status.merge_into(&mut entry).unwrap();
        assert_eq!(entry["install"]["status"], "done");
        value["state"]["candidate"] = value["boot"]["deploymentId"].clone();
        value["state"]["current"] = value["state"]["fallback"].clone();
        value["state"]["fallback"] = Value::Null;
        value["deployments"][0]["triesLeft"] = json!(2);
        let status = Status::parse(&value.to_string()).unwrap();
        assert_eq!(status.phase().0, "validating");
        assert_eq!(status.rollback()["reason"], "candidate_pending");
        value["boot"]["deploymentId"] = "b".repeat(64).into();
        value["boot"]["entry"] = format!("mica-{}.conf", "b".repeat(64)).into();
        assert_eq!(
            Status::parse(&value.to_string()).unwrap().phase().0,
            "reboot-required"
        );
        value["state"]["candidate"] = Value::Null;
        value["state"]["failed"] = json!(["a".repeat(64)]);
        assert_eq!(
            Status::parse(&value.to_string()).unwrap().phase().0,
            "rolled-back"
        );
    }

    #[test]
    fn fit_status_distinguishes_required_fit_verification_from_uefi_secure_boot() {
        let mut value = fixture();
        value["boot"]["backend"] = json!("uboot-fit");
        value["boot"]["entry"] = json!(format!("fit:{}", "a".repeat(64)));
        value["boot"]["secureBoot"] = json!(false);
        let status = Status::parse(&value.to_string()).unwrap();
        assert_eq!(status.boot.backend, BootBackend::UbootFit);
        assert!(status.boot.boot_verified);
        assert!(!status.boot.secure_boot);
        value["boot"]["secureBoot"] = json!(true);
        assert!(Status::parse(&value.to_string()).is_err());
        value["boot"]["secureBoot"] = json!(false);
        value["boot"]["bootVerified"] = json!(false);
        assert!(Status::parse(&value.to_string()).is_err());
        value["boot"]["backend"] = json!("unknown");
        assert!(Status::parse(&value.to_string()).is_err());
    }

    #[test]
    fn invalid_or_substituted_native_status_is_refused() {
        let mut value = fixture();
        value["slots"] = json!({});
        assert!(Status::parse(&value.to_string()).is_err());
        value.as_object_mut().unwrap().remove("slots");
        value["boot"]["kernelId"] = "e".repeat(64).into();
        assert!(Status::parse(&value.to_string()).is_err());
        value = fixture();
        value["deployments"][1] = value["deployments"][0].clone();
        assert!(Status::parse(&value.to_string()).is_err());
        assert!(Status::parse(&" ".repeat(65537)).is_err());
    }
}

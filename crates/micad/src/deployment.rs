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
#[serde(rename_all = "camelCase")]
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
#[serde(rename_all = "camelCase")]
pub struct State {
    pub highest_generation: u64,
    pub current: Option<String>,
    pub fallback: Option<String>,
    pub candidate: Option<String>,
    pub failed: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub id: String,
    pub file: String,
    pub generation: u64,
    pub tries_left: Option<u8>,
    pub version: String,
    pub kernel_id: String,
    pub kernel_release: String,
    /// The kernel's signed features; absent when it claims none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_features: Option<Vec<String>>,
    pub rootfs_id: String,
}

/// `mica-deploy status`, read leniently in every struct of it: micad comes
/// from the device's core set and the client from its root, so the client may
/// be the newer of the two and say more than this daemon knows to read.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Status {
    pub boot: Boot,
    pub state: State,
    pub deployments: Vec<Deployment>,
    /// The sets the device holds, by kind.
    #[serde(default)]
    pub sets: SetsStatus,
}

/// The sets a device holds, by kind. A kind this daemon does not know is not
/// read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct SetsStatus {
    #[serde(default)]
    pub core: CoreStatus,
}

/// The kind of set mica-core's own packages are released as.
pub const CORE_KIND: &str = micad_settings::configuration::CORE_KIND;

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

/// The core sets a device holds.
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

/// The name of a staged set in `verified/`: `set-<kind>-<id>.json`.
#[must_use]
pub fn staged_set_name(kind: &str, id: &str) -> String {
    format!("set-{kind}-{id}.json")
}

/// The id a staged file in `verified/` carries, and the kind when it is a
/// set's: `<id>.json` for a deployment, `set-<kind>-<id>.json` for a set. A
/// kind is letters and digits, so the name reads one way.
#[must_use]
pub fn staged_id(path: &Path) -> Option<(&str, Option<&str>)> {
    let stem = path.file_name()?.to_str()?.strip_suffix(".json")?;
    let (id, kind) = match stem.strip_prefix("set-") {
        Some(rest) => {
            let (kind, id) = rest.split_once('-')?;
            micad_settings::configuration::is_set_kind(kind).then_some((id, Some(kind)))?
        }
        None => (stem, None),
    };
    valid_id(id).then_some((id, kind))
}

/// The staged `general` core set of the highest generation in `verified`:
/// the one a system update offers `install` as the channel's latest. Read
/// without authenticating it; `install` authenticates what it is given, and
/// passes over a set older than the image's own.
#[must_use]
pub fn newest_general_set(verified: &Path) -> Option<std::path::PathBuf> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let mut newest: Option<(u64, std::path::PathBuf)> = None;
    for entry in std::fs::read_dir(verified).ok()?.flatten() {
        let path = entry.path();
        if staged_id(&path).and_then(|(_, kind)| kind) != Some("core") {
            continue;
        }
        let read = || -> Option<u64> {
            let envelope: Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            let payload = STANDARD.decode(envelope.get("payload")?.as_str()?).ok()?;
            let set: Value = serde_json::from_slice(&payload).ok()?;
            (set.get("channel")?.as_str()? == "general").then_some(())?;
            set.get("generation")?.as_u64()
        };
        if let Some(generation) = read()
            && newest.as_ref().is_none_or(|(held, _)| generation > *held)
        {
            newest = Some((generation, path));
        }
    }
    newest.map(|(_, path)| path)
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
        for set in [&status.sets.core.current, &status.sets.core.pending]
            .into_iter()
            .flatten()
        {
            ensure!(valid_id(&set.id), "invalid core set identity");
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
                // Whether a FIT boot is verified is the board's boot trust,
                // which mica-deploy holds the receipt to; a FIT never claims
                // Secure Boot.
                ensure!(
                    !status.boot.secure_boot,
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
        self.state.candidate.is_some() || self.sets.core.pending.is_some()
    }

    pub fn phase(&self) -> (&'static str, String) {
        // A core set on trial runs over a confirmed deployment, so its phase
        // is the device's.
        if let Some(phase) = self.sets.core.phase() {
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
        Status::parse(
            &self
                .action(vec!["status".into()], Duration::from_secs(30))
                .await?,
        )
    }
    async fn install(&self, descriptor: &Path) -> Result<()> {
        let objects = descriptor
            .parent()
            .context("missing descriptor directory")?
            .join("objects");
        let set = staged_id(descriptor).is_some_and(|(_, kind)| kind.is_some());
        let mut args = vec![
            if set { "set-install" } else { "install" }.into(),
            descriptor.to_string_lossy().into_owned(),
            "--objects".into(),
            objects.to_string_lossy().into_owned(),
        ];
        if !set
            && let Some(core) = newest_general_set(
                descriptor
                    .parent()
                    .context("missing descriptor directory")?,
            )
        {
            args.extend(["--core-set".into(), core.to_string_lossy().into_owned()]);
        }
        self.action(args, Duration::from_secs(1800)).await?;
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
        status.sets.core = CoreStatus {
            current: Some(set("e")),
            pending: Some(set("f")),
            attempts_left: Some(3),
            boot: CoreBoot {
                core_set_id: Some("e".repeat(64)),
                pending: false,
            },
        };
        assert!(status.update_pending());
        let (phase, reason) = status.phase();
        assert_eq!(phase, "reboot-required");
        assert!(reason.contains("0.0.6"), "{reason}");
        status.sets.core.boot = CoreBoot {
            core_set_id: Some("f".repeat(64)),
            pending: true,
        };
        assert_eq!(status.phase().0, "validating");
        let mut entry = serde_json::Map::new();
        status.merge_into(&mut entry).unwrap();
        assert_eq!(entry["sets"]["core"]["pending"]["generation"], 4);
        assert_eq!(entry["sets"]["core"]["attemptsLeft"], 3);
        status.sets.core.pending = None;
        assert_eq!(status.phase().0, "succeeded");
    }

    /// A staged file says what it is by its name, and by nothing else.
    #[test]
    fn a_staged_file_names_a_deployment_or_a_core_set() {
        let id = "a".repeat(64);
        let path = |name: String| std::path::PathBuf::from("/mica/updates/verified").join(name);
        assert_eq!(
            staged_id(&path(format!("{id}.json"))),
            Some((id.as_str(), None))
        );
        assert_eq!(
            staged_id(&path(staged_set_name("core", &id))),
            Some((id.as_str(), Some("core")))
        );
        for name in [
            format!("{id}.partial"),
            "set-core-.json".into(),
            format!("set-{id}.json"),
            format!("set-Core-{id}.json"),
            format!("set-core-core-{id}.json"),
            format!("core-{id}.json"),
        ] {
            assert_eq!(staged_id(&path(name.clone())), None, "{name}");
        }
    }

    /// micad and the client come from different releases. A client that says
    /// more than this daemon reads is still read, at every level, and what it
    /// says of the core sets is held to the same rule as the rest.
    #[test]
    fn a_newer_clients_status_is_read_and_its_ids_are_checked() {
        let mut value = fixture();
        value["later"] = json!(1);
        value["boot"]["later"] = json!(1);
        value["state"]["later"] = json!(1);
        value["deployments"][0]["later"] = json!(1);
        value["sets"] = json!({"later": {"current": null}, "core": {
            "current": {"id": "e".repeat(64), "channel": "general", "generation": 4,
                "version": "0.0.6", "selected": ["micad"], "later": 1},
            "pending": null, "attemptsLeft": null,
            "boot": {"coreSetId": "e".repeat(64), "pending": false, "later": 1},
            "later": 1,
        }});
        let status = Status::parse(&value.to_string()).unwrap();
        assert_eq!(status.sets.core.current.as_ref().unwrap().generation, 4);
        assert_eq!(status.phase().0, "succeeded");
        value["sets"]["core"]["current"]["id"] = json!("x");
        assert!(Status::parse(&value.to_string()).is_err());
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
    fn fit_status_never_claims_secure_boot_and_may_be_unverified() {
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
        // A board whose loader checks only the FIT's hashes boots unverified.
        value["boot"]["bootVerified"] = json!(false);
        assert!(
            !Status::parse(&value.to_string())
                .unwrap()
                .boot
                .boot_verified
        );
        value["boot"]["backend"] = json!("unknown");
        assert!(Status::parse(&value.to_string()).is_err());
    }

    #[test]
    fn invalid_or_substituted_native_status_is_refused() {
        let mut value = fixture();
        value["boot"]["kernelId"] = "e".repeat(64).into();
        assert!(Status::parse(&value.to_string()).is_err());
        value = fixture();
        value["deployments"][1] = value["deployments"][0].clone();
        assert!(Status::parse(&value.to_string()).is_err());
        assert!(Status::parse(&" ".repeat(65537)).is_err());
    }

    /// A system update offers `install` the staged general set of the
    /// highest generation, and nothing else of the staged sets.
    #[test]
    fn the_newest_staged_general_set_is_the_one_offered() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        let dir = tempfile::tempdir().unwrap();
        let stage = |id: char, channel: &str, generation: u64| {
            let payload = json!({"channel": channel, "generation": generation});
            let envelope = json!({"payload": STANDARD.encode(payload.to_string())});
            let path = dir
                .path()
                .join(staged_set_name("core", &id.to_string().repeat(64)));
            std::fs::write(&path, envelope.to_string()).unwrap();
            path
        };
        assert_eq!(newest_general_set(dir.path()), None);
        stage('a', "general", 3);
        let newest = stage('b', "general", 5);
        stage('c', "lts", 9);
        std::fs::write(
            dir.path().join(staged_set_name("core", &"d".repeat(64))),
            "not a set",
        )
        .unwrap();
        std::fs::write(dir.path().join(format!("{}.json", "e".repeat(64))), "{}").unwrap();
        assert_eq!(newest_general_set(dir.path()), Some(newest));
    }
}

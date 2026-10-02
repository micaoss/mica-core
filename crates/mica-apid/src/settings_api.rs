//! Minimal async facade over the micad settings/state API.
//!
//! Handlers depend on this trait so tests can substitute an in-memory fake
//! for the D-Bus client.

use serde_json::Value;

use crate::task_registry::TaskRecord;

#[cfg(test)]
mod fake;
#[cfg(test)]
pub use fake::*;

#[derive(Debug)]
pub struct TaskNotFound(pub String);

impl std::fmt::Display for TaskNotFound {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "task not found: `{}`", self.0)
    }
}

impl std::error::Error for TaskNotFound {}

/// micad answered a task read, but the payload did not match the public task
/// record. This is a daemon-side 500, distinct from a transport outage.
#[derive(Debug)]
pub struct InvalidTaskPayload(pub serde_json::Error);

impl std::fmt::Display for InvalidTaskPayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid task payload from micad: {}", self.0)
    }
}

impl std::error::Error for InvalidTaskPayload {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// The micad operations apid needs, JSON in and out.
///
/// The power actions are here rather than executed locally because micad owns
/// every system action: apid never spawns a process and never talks to
/// systemd itself.
#[async_trait::async_trait]
pub trait SettingsApi: Send + Sync {
    /// Settings subtree at dot-path `path` (`""` = whole tree).
    async fn get_settings(&self, path: &str) -> anyhow::Result<Value>;
    /// Persist `value` at dot-path `path`, enqueue its apply and return the
    /// task id.
    async fn set_settings(&self, path: &str, value: &Value) -> anyhow::Result<String>;
    /// Task record by id.
    async fn get_task(&self, id: &str) -> anyhow::Result<TaskRecord> {
        Err(TaskNotFound(id.to_string()).into())
    }
    /// Live-state subtree at dot-path `path` (`""` = whole tree).
    async fn get_state(&self, path: &str) -> anyhow::Result<Value>;
    /// Live interface details observed by systemd-networkd.
    async fn get_network_state(&self) -> anyhow::Result<Value> {
        self.get_state("network").await
    }
    /// Time-synchronization status observed from timesyncd, classified by
    /// micad. Read-only: there is deliberately no method beside it that could
    /// pause or stop synchronization.
    async fn get_time_status(&self) -> anyhow::Result<Value>;
    /// Storage status observed by micad: the fixed tiers, their space and
    /// check evidence, the physical media and the low-space policy.
    ///
    /// Read-only, and deliberately alone: there is no method here that
    /// formats, repartitions or erases anything, because the layout is fixed
    /// by the image assembler and a management API that could rewrite it
    /// would be a remote destructive surface with no product use.
    async fn get_storage_status(&self) -> anyhow::Result<Value>;
    /// The system-information surface micad assembles: machine id,
    /// board, kernel, release, image version with git stamp and build date,
    /// installed packages, booted slot and uptime. Read-only.
    async fn get_system_info(&self) -> anyhow::Result<Value>;
    /// Board telemetry observed by micad: thermal, watchdog and
    /// reset reason, absence explicit. Read-only.
    async fn get_telemetry(&self) -> anyhow::Result<Value>;
    /// The OBSERVED network state: link/carrier, addresses, DHCP
    /// lease, default routes, DNS reachability, Wi-Fi association and the
    /// radio/modem capabilities. Distinct from `get_network_state`'s reduced
    /// view and from the desired `network` settings. Read-only.
    async fn get_observed_network(&self) -> anyhow::Result<Value>;
    /// Failure evidence for the diagnostic snapshot: failed units
    /// and a bounded journal excerpt, bounded by micad. Read-only.
    async fn get_failure_evidence(&self) -> anyhow::Result<Value>;
    /// The newest lines of one allowlisted service's log, bounded by micad;
    /// a source off the list is micad's `InvalidArgs`. Read-only.
    async fn get_log(&self, source: &str) -> anyhow::Result<Value>;
    /// Ask micad to reboot the appliance.
    async fn reboot(&self) -> anyhow::Result<()>;
    /// Ask micad to power the appliance off.
    async fn power_off(&self) -> anyhow::Result<()>;
    /// Ask micad to set a TRANSIENT root password, cleared on the next boot.
    ///
    /// Deliberately not a `set_settings` call: a password that reached the
    /// settings tree would be persisted, re-applied on the next boot and
    /// readable by anything that can call `GetSettings`.
    async fn set_transient_root_password(&self, password: &str) -> anyhow::Result<String>;
    /// Draw a new WireGuard private key for `iface` and return its new base64
    /// public key.
    ///
    /// Deliberately not a `set_settings` call, and for a stronger reason than
    /// the transient password's: the settings tree holds no key to write. The
    /// private half never leaves micad and there is no accessor that returns
    /// one, so the only thing this call can hand back is the public half.
    async fn rotate_wireguard_key(&self, iface: &str) -> anyhow::Result<String>;
    /// The Bluetooth surface micad joins: the declared trust list, the
    /// adapter, the devices BlueZ holds, the pairing code and whatever is
    /// waiting to be confirmed (`GetBluetooth`).
    ///
    /// Unavailable by default, like every other observation.
    async fn get_bluetooth(&self) -> anyhow::Result<Value> {
        anyhow::bail!("this build reads no Bluetooth adapter")
    }
    /// Start or stop a Bluetooth scan (`SetBluetoothDiscovery`).
    async fn set_bluetooth_discovery(&self, _on: bool) -> anyhow::Result<()> {
        anyhow::bail!("this build drives no Bluetooth adapter")
    }
    /// Pair with a device (`PairBluetoothDevice`).
    async fn pair_bluetooth_device(&self, _address: &str) -> anyhow::Result<()> {
        anyhow::bail!("this build drives no Bluetooth adapter")
    }
    /// Answer the agent's pending confirmation (`ConfirmBluetoothPairing`).
    async fn confirm_bluetooth_pairing(&self, _address: &str, _accept: bool) -> anyhow::Result<()> {
        anyhow::bail!("this build drives no Bluetooth adapter")
    }
    /// Drop a device (`RemoveBluetoothDevice`).
    async fn remove_bluetooth_device(&self, _address: &str) -> anyhow::Result<()> {
        anyhow::bail!("this build drives no Bluetooth adapter")
    }
    /// Scan for WiFi networks on the station's radio (`ScanWifi`).
    ///
    /// Unavailable by default, like every other observation: a build with no
    /// bus behind it reports the scan as unavailable rather than inventing an
    /// empty list of networks on the air.
    async fn scan_wifi(&self) -> anyhow::Result<Value> {
        anyhow::bail!("this build drives no radio")
    }
    /// The declared containers joined with what the engine reports
    /// (`GetContainers`). Unavailable by default: a build with no bus behind
    /// it reports the engine as absent rather than inventing an empty list.
    async fn get_containers(&self) -> anyhow::Result<Value> {
        anyhow::bail!("this build reads no container engine")
    }
    /// One container lifecycle verb: `start`, `stop` or `restart`
    /// (`StartContainer` and its siblings).
    async fn container_action(&self, _name: &str, _action: &str) -> anyhow::Result<()> {
        anyhow::bail!("this build drives no containers")
    }
    /// The complete update state (`GetUpdateState`): micad reads native deployment records and
    /// re-derives the lifecycle before answering, so this is never stale.
    async fn get_update_state(&self) -> anyhow::Result<Value>;
    /// Ask micad to run an update metadata check on a background task.
    async fn check_update(&self) -> anyhow::Result<()>;
    /// Ask micad to download the selected deployment objects on a background task.
    async fn fetch_update(&self) -> anyhow::Result<()>;
    /// Ask micad to import an offline archive already written to the device
    /// (`ImportUpdate`). The path is the file apid streamed the upload into;
    /// micad refuses one outside its own upload directory.
    async fn import_update(&self, _path: &str) -> anyhow::Result<()> {
        anyhow::bail!("this build imports no update archives")
    }
    /// Ask micad to install a verified deployment by its authenticated ID.
    /// Background progress lands in the update state.
    async fn install_update(&self, deployment_id: &str) -> anyhow::Result<()>;
    async fn confirm_deployment(&self, deployment_id: &str) -> anyhow::Result<()>;
    async fn reject_deployment(&self, deployment_id: &str) -> anyhow::Result<()>;
    async fn rollback_deployment(&self, deployment_id: &str) -> anyhow::Result<()>;
    /// Arm the bounded safe-to-reboot override for `seconds`; answers the
    /// recorded override.
    async fn set_reboot_override(&self, seconds: u32) -> anyhow::Result<Value>;

    /// Ask micad to merge `patch` into `/mica/config/updates.json` and write
    /// it; answers the document as saved.
    ///
    /// Deliberately not a file apid opens. micad owns that document and is its
    /// only writer, so one fact has one writer
    /// all the way down to the filesystem — and the validation that decides
    /// what may be written is the same code the reader runs, in the same
    /// process, rather than a second copy here that would eventually disagree
    /// with it.
    async fn set_update_config(&self, patch: &Value) -> anyhow::Result<Value>;
}

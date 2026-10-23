//! Network reconciler: renders systemd-networkd `.network` units and reloads
//! networkd.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use micad_settings::{IfaceKind, Settings};
use serde_json::json;

use super::Reconciler;
use crate::wgkeys::Keystore;

mod ifupdown;
mod render;
mod settle;
mod validate;
pub use ifupdown::IfupdownReconciler;
use render::*;
pub use settle::{LinkSettle, NetworkdSettle, NoSettle};
use validate::*;

/// Directory networkd reads runtime unit files from.
const DEFAULT_NETWORK_DIR: &str = "/run/systemd/network";
/// Environment variable overriding the networkd unit directory.
const NETWORK_DIR_ENV: &str = "MICAD_NETWORK_DIR";
/// Environment variable naming the settings file, whose directory is the
/// STATE-backed directory the WireGuard keys live under.
///
/// The same variable the access-point reconciler reads for the same reason: a
/// test that redirects it redirects the secrets with it, and never writes a key
/// into the host's `/var/lib/mica`.
const SETTINGS_PATH_ENV: &str = "MICAD_SETTINGS_PATH";

/// Asks the network stack to pick up freshly rendered unit files.
#[async_trait::async_trait]
pub trait NetworkReload: Send + Sync {
    /// Reload network configuration.
    async fn reload(&self) -> anyhow::Result<()>;
}

/// Production reloader calling `org.freedesktop.network1` `Manager.Reload` on
/// the system bus.
///
/// The bus connection is created lazily inside the call, so constructing this
/// reloader never touches the host.
pub struct Networkd;

#[async_trait::async_trait]
impl NetworkReload for Networkd {
    async fn reload(&self) -> anyhow::Result<()> {
        let connection = zbus::Connection::system().await?;
        connection
            .call_method(
                Some("org.freedesktop.network1"),
                "/org/freedesktop/network1",
                Some("org.freedesktop.network1.Manager"),
                "Reload",
                &(),
            )
            .await?;
        Ok(())
    }
}

/// The reload of a root without networkd: nothing to reload, because its
/// Wi-Fi services address their links themselves.
pub struct NoReload;

#[async_trait::async_trait]
impl NetworkReload for NoReload {
    async fn reload(&self) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Deletes a virtual network device from the kernel.
#[async_trait::async_trait]
pub trait LinkDelete: Send + Sync {
    /// Delete the kernel device named `iface`.
    async fn delete_link(&self, iface: &str) -> anyhow::Result<()>;
}

/// Production deleter running `networkctl delete <iface>`.
///
/// `networkctl` ships with the systemd the image already runs networkd from,
/// and its `delete` verb is one RTM_DELLINK: the image carries no iproute2,
/// and hand-rolled netlink would put a second engine next to the networkd this
/// reconciler otherwise speaks through.
pub struct NetworkctlDelete;

impl NetworkctlDelete {
    fn command(iface: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("networkctl");
        command.args(["delete", iface]);
        command
    }
}

#[async_trait::async_trait]
impl LinkDelete for NetworkctlDelete {
    async fn delete_link(&self, iface: &str) -> anyhow::Result<()> {
        let output = Self::command(iface).output().await?;
        if !output.status.success() {
            return Err(anyhow::anyhow!(
                "networkctl delete {iface} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    }
}

/// A deleter that deletes nothing, for tests that exercise this reconciler for
/// its file handling alone.
///
/// `cfg(test)` rather than an allow: it exists only for tests, and an allow
/// would also silence the day it stops being used at all (the argument
/// `Hostnamed::with_path` makes).
#[cfg(test)]
pub struct NoDelete;

#[cfg(test)]
#[async_trait::async_trait]
impl LinkDelete for NoDelete {
    async fn delete_link(&self, _iface: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

/// The features the running kernel claims to carry.
#[async_trait::async_trait]
pub trait KernelFeatures: Send + Sync {
    /// The kernel's signed features, or `None` when it claims none.
    async fn features(&self) -> Option<Vec<String>>;
}

/// A kernel that claims no features.
pub struct NoClaim;

#[async_trait::async_trait]
impl KernelFeatures for NoClaim {
    async fn features(&self) -> Option<Vec<String>> {
        None
    }
}

/// The running deployment's kernel features as `mica-deploy status` reports
/// them, read once per boot: the kernel cannot change under a running daemon.
pub struct DeployedKernel {
    client: crate::deployment::NativeClient,
    read: std::sync::Mutex<Option<Option<Vec<String>>>>,
}

impl DeployedKernel {
    /// Through `client`.
    pub fn new(client: Arc<dyn crate::update_lifecycle::UpdateClient>) -> Self {
        Self {
            client: crate::deployment::NativeClient::new(client),
            read: std::sync::Mutex::new(None),
        }
    }

    /// Through the device's `mica-deploy`.
    pub fn production() -> Self {
        Self::new(Arc::new(crate::update_lifecycle::SubprocessClient::new(
            PathBuf::from(crate::update_lifecycle::DEFAULT_CLIENT_PATH),
        )))
    }
}

#[async_trait::async_trait]
impl KernelFeatures for DeployedKernel {
    /// A status that cannot be read is no claim, and is read again next time.
    async fn features(&self) -> Option<Vec<String>> {
        if let Some(read) = self.read.lock().ok()?.clone() {
            return read;
        }
        use crate::deployment::DeploymentClient;
        match self.client.status().await {
            Ok(status) => {
                let features = status
                    .booted()
                    .and_then(|deployment| deployment.kernel_features.clone());
                if let Ok(mut read) = self.read.lock() {
                    *read = Some(features.clone());
                }
                features
            }
            Err(error) => {
                tracing::warn!(%error, "running kernel features unavailable");
                None
            }
        }
    }
}

/// An apply whose interfaces did not come up, so the files it replaced were
/// put back.
#[derive(Debug, thiserror::Error)]
#[error("{0}; the previous network configuration was restored")]
pub struct RolledBack(pub String);

/// Reconciler for the `network` settings subtree.
///
/// An apply replaces the unit files whole and reloads networkd. After the
/// first apply of this process, which has nothing of its own to go back to,
/// it waits for every interface it changed to come up, and on failure puts
/// the previous files back and answers [`RolledBack`].
pub struct NetworkReconciler<R: NetworkReload, D: LinkDelete> {
    target_dir: PathBuf,
    reloader: R,
    deleter: D,
    keys: Keystore,
    kernel: Arc<dyn KernelFeatures>,
    settle: Arc<dyn LinkSettle>,
    /// Where the kernel lists its links (`/sys/class/net`), for the wired
    /// ports nobody declared; `None` renders no unit for them.
    ports_dir: Option<PathBuf>,
    applied: AtomicBool,
}

impl<R: NetworkReload, D: LinkDelete> NetworkReconciler<R, D> {
    /// Create a network reconciler rendering units into `target_dir`,
    /// reloading through `reloader`, tearing devices down through `deleter`
    /// and taking WireGuard keys from `keys`.
    pub fn new(target_dir: PathBuf, reloader: R, deleter: D, keys: Keystore) -> Self {
        Self {
            target_dir,
            reloader,
            deleter,
            keys,
            kernel: Arc::new(NoClaim),
            settle: Arc::new(NoSettle),
            ports_dir: None,
            applied: AtomicBool::new(false),
        }
    }

    /// Give the wired ports listed in `dir` and not declared a DHCP unit of
    /// their own, with its route metric.
    #[must_use]
    pub fn with_ports_dir(mut self, dir: PathBuf) -> Self {
        self.ports_dir = Some(dir);
        self
    }

    /// The wired ports in [`Self::ports_dir`] no entry declares: the links
    /// the image's `80-dhcp.network` (`Name=eth*`) would configure, less the
    /// VLANs on them.
    fn undeclared_ports(&self, settings: &Settings) -> BTreeSet<String> {
        let Some(dir) = &self.ports_dir else {
            return BTreeSet::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return BTreeSet::new();
        };
        entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name.starts_with("eth") && !name.contains('.'))
            .filter(|name| !settings.network.contains_key(name))
            .collect()
    }

    /// Wait for changed interfaces through `settle`.
    #[must_use]
    pub fn with_settle(mut self, settle: Arc<dyn LinkSettle>) -> Self {
        self.settle = settle;
        self
    }

    /// The managed unit files in the target directory, by name.
    fn managed_files(&self) -> anyhow::Result<BTreeMap<String, String>> {
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(&self.target_dir)? {
            let entry = entry?;
            let Some(file_name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if is_mica_managed(&file_name) {
                files.insert(file_name, std::fs::read_to_string(entry.path())?);
            }
        }
        Ok(files)
    }

    /// Make the managed files `files`, from `current`: each changed one
    /// written beside its place and renamed over it, and the rest removed.
    fn replace_files(
        &self,
        files: &BTreeMap<String, String>,
        current: &BTreeMap<String, String>,
    ) -> anyhow::Result<()> {
        for (file_name, text) in files {
            if current.get(file_name) != Some(text) {
                let staged = self.target_dir.join(format!(".{file_name}.tmp"));
                std::fs::write(&staged, text)?;
                std::fs::rename(&staged, self.target_dir.join(file_name))?;
            }
        }
        for file_name in current.keys() {
            if !files.contains_key(file_name) {
                std::fs::remove_file(self.target_dir.join(file_name))?;
            }
        }
        Ok(())
    }

    /// Delete each of `devices`. A failure is logged and not returned: the
    /// usual cause is a device that was never created.
    async fn delete_devices<'a>(&self, devices: impl IntoIterator<Item = &'a String>) {
        for iface in devices {
            if let Err(error) = self.deleter.delete_link(iface).await {
                tracing::warn!(iface, %error, "could not delete network device");
            }
        }
    }

    /// Hold virtual devices to the features `kernel` claims.
    #[must_use]
    pub fn with_kernel(mut self, kernel: Arc<dyn KernelFeatures>) -> Self {
        self.kernel = kernel;
        self
    }
}

impl NetworkReconciler<Networkd, NetworkctlDelete> {
    /// Production reconciler: target directory from `MICAD_NETWORK_DIR` if
    /// set, else the networkd runtime directory; keys under the directory of
    /// [`SETTINGS_PATH_ENV`].
    pub fn production() -> Self {
        let dir = std::env::var(NETWORK_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_NETWORK_DIR));
        Self::new(dir, Networkd, NetworkctlDelete, production_keystore())
            .with_kernel(Arc::new(DeployedKernel::production()))
            .with_settle(Arc::new(NetworkdSettle))
            .with_ports_dir(Path::new("/").join(crate::network_state::NET_CLASS_DIR))
    }
}

/// The production key store: WireGuard keys under the directory holding the
/// settings file, which is the STATE-backed directory on a device.
///
/// Shared by the reconciler and by [`KeyRotation`], so the two cannot end up
/// reading and writing different key files.
fn production_keystore() -> Keystore {
    let state_dir = std::env::var(SETTINGS_PATH_ENV)
        .ok()
        .and_then(|path| {
            Path::new(&path)
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from(crate::identity::DEFAULT_STATE_DIR));
    Keystore::production(&state_dir)
}

/// Drawing a WireGuard interface a new private key.
///
/// A rotation is not a settings write — there is no key field in the settings
/// tree to write — so it is its own operation rather than a value the tree
/// carries, and this is the port the bus method calls through.
#[async_trait::async_trait]
pub trait WireguardRotate: Send + Sync {
    /// Replace `iface`'s private key and return the new public key.
    async fn rotate_key(&self, iface: &str) -> anyhow::Result<String>;
}

/// The rotation this daemon performs: a new key in the key store, and the
/// device holding the old one deleted.
pub struct KeyRotation<D: LinkDelete> {
    keys: Keystore,
    deleter: D,
}

impl<D: LinkDelete> KeyRotation<D> {
    /// Rotate keys in `keys`, deleting devices through `deleter`.
    pub fn new(keys: Keystore, deleter: D) -> Self {
        Self { keys, deleter }
    }
}

impl KeyRotation<NetworkctlDelete> {
    /// Production rotation, against the same key store the production
    /// reconciler renders from.
    pub fn production() -> Self {
        Self::new(production_keystore(), NetworkctlDelete)
    }
}

#[async_trait::async_trait]
impl<D: LinkDelete> WireguardRotate for KeyRotation<D> {
    /// Write a new private key, then delete the device carrying the old one.
    async fn rotate_key(&self, iface: &str) -> anyhow::Result<String> {
        // The name reaches a file path and a command argument, exactly as it
        // does on the render path.
        validate_iface_name(iface)?;
        let public_key = self.keys.rotate(iface)?;
        self.deleter.delete_link(iface).await?;
        Ok(public_key)
    }
}

#[async_trait::async_trait]
impl<R: NetworkReload, D: LinkDelete> Reconciler for NetworkReconciler<R, D> {
    fn name(&self) -> &'static str {
        "network"
    }

    fn subtree(&self) -> &'static str {
        "network"
    }

    async fn apply(&self, settings: &Settings) -> anyhow::Result<serde_json::Value> {
        let state = self.write_and_settle(settings).await?;
        // Only now: a rollback puts back netdevs that name these key files.
        let tunnels: BTreeSet<&str> = settings
            .network
            .iter()
            .filter(|(_, cfg)| matches!(cfg.kind, IfaceKind::Wireguard))
            .map(|(iface, _)| iface.as_str())
            .collect();
        for iface in self.keys.retain_keys(&tunnels)? {
            tracing::info!(%iface, "removed the private key of a deleted tunnel");
        }
        Ok(state)
    }
}

impl<R: NetworkReload, D: LinkDelete> NetworkReconciler<R, D> {
    /// Render and write the units, reload networkd and, past the first pass,
    /// wait for the changed interfaces, putting the previous files back when
    /// they do not come up.
    async fn write_and_settle(&self, settings: &Settings) -> anyhow::Result<serde_json::Value> {
        validate_network(&settings.network)?;
        validate_kernel_features(&settings.network, self.kernel.features().await.as_deref())?;
        // Before the netdevs, so networkd finds every key file a unit names.
        let psks = self.keys.sync_psks(&preshared_keys(&settings.network))?;
        let masters = bridge_ports(&settings.network);
        let children = vlan_children(&settings.network);
        let ports = self.undeclared_ports(settings);
        let metrics = route_metrics(&settings.network, &masters, &ports);
        std::fs::create_dir_all(&self.target_dir)?;
        let previous = self.managed_files()?;
        let mut files = BTreeMap::new();
        let mut state = serde_json::Map::new();
        // Devices whose netdev properties changed. They apply at creation
        // only, so the device has to go and be built again.
        let mut recreate = BTreeSet::new();
        for (iface, cfg) in &settings.network {
            let file_name = format!("50-mica-{iface}.network");
            let unit = render_unit(
                iface,
                cfg,
                masters.get(iface.as_str()).copied(),
                children.get(iface.as_str()).map_or(&[][..], Vec::as_slice),
                metrics.get(iface.as_str()).copied(),
            );
            files.insert(file_name.clone(), unit);
            let mut entry = json!({
                "file": file_name,
                "dhcp": cfg.dhcp,
                "kind": kind_name(cfg.kind),
            });
            if matches!(cfg.kind, IfaceKind::Wireguard) {
                // Lazily, on the first pass that sees the tunnel: a key file
                // that is already there is kept, so this generates exactly
                // once per interface. Only the public half is published — it
                // is what the far end needs, and it is public by definition.
                entry["publicKey"] = json!(self.keys.ensure(iface)?);
            }
            state.insert(iface.clone(), entry);
            if let Some(netdev) = render_netdev(iface, cfg, &self.keys) {
                let netdev_name = format!("50-mica-{iface}.netdev");
                if previous
                    .get(&netdev_name)
                    .is_some_and(|previous| *previous != netdev || psks.contains(iface))
                {
                    recreate.insert(iface.clone());
                }
                files.insert(netdev_name, netdev);
            }
        }
        for port in &ports {
            let dhcp = micad_settings::IfaceSettings {
                dhcp: true,
                ..micad_settings::IfaceSettings::default()
            };
            files.insert(
                auto_unit_name(port),
                render_unit(port, &dhcp, None, &[], metrics.get(port.as_str()).copied()),
            );
        }
        let torn_down: BTreeSet<String> = previous
            .keys()
            .filter(|file_name| !files.contains_key(*file_name))
            .filter_map(|file_name| mica_netdev_iface(file_name).map(str::to_string))
            .collect();
        self.replace_files(&files, &previous)?;
        // Before the reload, so networkd builds the recreated devices back on
        // the same pass that deleted them. Failing the apply on a delete would
        // report every interface that did converge as unconverged.
        self.delete_devices(torn_down.union(&recreate)).await;
        self.reloader.reload().await?;
        if !self.applied.swap(true, Ordering::AcqRel) {
            return Ok(serde_json::Value::Object(state));
        }
        let changed: BTreeMap<String, bool> = settings
            .network
            .keys()
            .filter(|iface| {
                [".network", ".netdev"].iter().any(|suffix| {
                    let file_name = format!("50-mica-{iface}{suffix}");
                    files.get(&file_name) != previous.get(&file_name)
                })
            })
            .map(|iface| {
                let netdev = format!("50-mica-{iface}.netdev");
                let created = files.contains_key(&netdev);
                (iface.clone(), created)
            })
            .collect();
        if changed.is_empty() {
            return Ok(serde_json::Value::Object(state));
        }
        if let Err(reason) = self.settle.settle(&changed).await {
            tracing::warn!(%reason, "network apply did not come up; restoring the previous files");
            self.replace_files(&previous, &files)?;
            let rebuilt: BTreeSet<String> = files
                .keys()
                .chain(previous.keys())
                .filter_map(|file_name| mica_netdev_iface(file_name))
                .filter(|iface| {
                    let netdev = format!("50-mica-{iface}.netdev");
                    files.get(&netdev) != previous.get(&netdev)
                })
                .map(str::to_string)
                .collect();
            self.delete_devices(&rebuilt).await;
            self.reloader.reload().await?;
            return Err(RolledBack(reason).into());
        }
        Ok(serde_json::Value::Object(state))
    }
}

#[cfg(test)]
mod tests;

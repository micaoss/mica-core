//! Network reconciler: renders systemd-networkd `.network` units and reloads
//! networkd.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use micad_settings::{IfaceKind, Settings};
use serde_json::json;

use super::Reconciler;
use crate::wgkeys::Keystore;

mod ifupdown;
mod render;
mod validate;
pub use ifupdown::IfupdownReconciler;
use render::*;
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

/// Reconciler for the `network` settings subtree.
pub struct NetworkReconciler<R: NetworkReload, D: LinkDelete> {
    target_dir: PathBuf,
    reloader: R,
    deleter: D,
    keys: Keystore,
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
        }
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
        validate_network(&settings.network)?;
        let masters = bridge_ports(&settings.network);
        let children = vlan_children(&settings.network);
        std::fs::create_dir_all(&self.target_dir)?;
        let mut rendered = BTreeSet::new();
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
            );
            std::fs::write(self.target_dir.join(&file_name), unit)?;
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
            rendered.insert(file_name);
            if let Some(netdev) = render_netdev(iface, cfg, &self.keys) {
                let netdev_name = format!("50-mica-{iface}.netdev");
                let path = self.target_dir.join(&netdev_name);
                if std::fs::read_to_string(&path).is_ok_and(|previous| previous != netdev) {
                    recreate.insert(iface.clone());
                }
                std::fs::write(&path, netdev)?;
                rendered.insert(netdev_name);
            }
        }
        let mut torn_down = BTreeSet::new();
        for entry in std::fs::read_dir(&self.target_dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if is_mica_managed(file_name) && !rendered.contains(file_name) {
                if let Some(iface) = mica_netdev_iface(file_name) {
                    torn_down.insert(iface.to_string());
                }
                std::fs::remove_file(entry.path())?;
            }
        }
        // Before the reload, so networkd builds the recreated devices back on
        // the same pass that deleted them. A failure is logged and not
        // returned: the unit file is already gone, the usual cause is a device
        // that was never created, and failing the apply here would report
        // every interface that did converge as unconverged.
        for iface in torn_down.union(&recreate) {
            if let Err(error) = self.deleter.delete_link(iface).await {
                tracing::warn!(iface, %error, "could not delete network device");
            }
        }
        self.reloader.reload().await?;
        Ok(serde_json::Value::Object(state))
    }
}

#[cfg(test)]
mod tests;

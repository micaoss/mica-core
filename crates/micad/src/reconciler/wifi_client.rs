//! WiFi station (uplink) reconciler: renders wpa_supplicant configuration from
//! `wifi.client`, drives `wpa_supplicant@<interface>.service`, and renders the
//! networkd unit that gives the associated link an address. Three system
//! effects, in this order.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use micad_settings::{Settings, WifiClientSettings, WifiNetwork};
use serde_json::json;

use super::Reconciler;
use super::network::{NetworkReload, Networkd, NoReload};
use super::systemd::{Systemd, UnitControl, is_active, is_enabled};
use crate::fswrite::write_config_if_changed;

/// Directory Debian's `wpa_supplicant@.service` template reads its
/// per-interface configuration from.
const DEFAULT_CONFIG_DIR: &str = "/etc/wpa_supplicant";
/// Environment variable overriding the wpa_supplicant configuration directory.
const CONFIG_DIR_ENV: &str = "MICAD_WPA_SUPPLICANT_DIR";
/// Directory networkd reads runtime unit files from.
///
/// Deliberately the same directory and the same override the network
/// reconciler uses: both render into networkd's runtime drop-in directory, and
/// a test that redirects one must redirect the other with it.
const DEFAULT_NETWORK_DIR: &str = "/run/systemd/network";
/// Environment variable overriding the networkd unit directory.
const NETWORK_DIR_ENV: &str = "MICAD_NETWORK_DIR";
/// Mode of the rendered configuration: owner-only, because it carries PSKs.
const CONFIG_MODE: u32 = 0o600;
/// Prefix of the networkd units this reconciler owns.
///
/// `90-` sorts after the network reconciler's `50-mica-…` and the image's
/// `80-dhcp.network`, so an interface the operator configured explicitly keeps
/// winning: networkd applies the first matching unit in lexical order.
const NETWORKD_PREFIX: &str = "90-wifi-client-";
/// Header of the rendered configuration.
const CONFIG_HEADER: &str = "# Managed by micad from wifi.client. Do not edit.\n";

/// What the reconciler did to the station role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Station {
    /// Something changed: the configuration, the networkd unit, or the unit's
    /// runtime state.
    Applied,
    /// The station was already exactly as configured; nothing was written and
    /// no unit call was made.
    Unchanged,
    /// `wifi.client.enabled` is true but no network is configured, so there is
    /// nothing to associate with and the supplicant is kept down.
    Idle,
    /// `wifi.client.enabled` is false.
    Disabled,
}

impl Station {
    /// Live-state spelling of this outcome.
    fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Unchanged => "unchanged",
            Self::Idle => "idle",
            Self::Disabled => "disabled",
        }
    }
}

/// Reconciler for the `wifi.client` settings subtree.
pub struct WifiClientReconciler<C: UnitControl, R: NetworkReload> {
    /// On an OpenRC root, where the station service reads its interface and
    /// configuration and addresses the link itself; `None` under networkd.
    openrc_env: Option<PathBuf>,
    config_dir: PathBuf,
    network_dir: PathBuf,
    control: C,
    reloader: R,
}

impl<C: UnitControl, R: NetworkReload> WifiClientReconciler<C, R> {
    /// Create a station reconciler writing wpa_supplicant configuration into
    /// `config_dir` and networkd units into `network_dir`, driving the
    /// supplicant unit through `control` and reloading networkd through
    /// `reloader`.
    pub fn new(config_dir: PathBuf, network_dir: PathBuf, control: C, reloader: R) -> Self {
        Self {
            openrc_env: None,
            config_dir,
            network_dir,
            control,
            reloader,
        }
    }
}

/// The station service of an OpenRC root, mica-wifi's (Base): one service,
/// told its interface by [`OPENRC_ENV`], because an instance per interface
/// would be a link in the read-only `/etc/init.d`.
const OPENRC_UNIT: &str = "mica-wifi-client.service";
/// What micad tells that service: `interface=` and `config=`.
const OPENRC_ENV: &str = "/run/mica/wifi-client.env";
/// The configuration on STATE, writable on an OpenRC root, where no mount
/// binds `/etc/wpa_supplicant`.
const OPENRC_CONFIG_DIR: &str = "/var/lib/mica/wpa_supplicant";

impl<C: UnitControl> WifiClientReconciler<C, NoReload> {
    /// The station on an OpenRC root: the configuration under `config_dir`,
    /// the service's environment at `env`, driven through `control`.
    pub fn openrc_at(config_dir: PathBuf, env: PathBuf, control: C) -> Self {
        Self {
            openrc_env: Some(env),
            ..Self::new(config_dir, PathBuf::new(), control, NoReload)
        }
    }

    /// Production station on an OpenRC root.
    pub fn openrc(control: C) -> Self {
        Self::openrc_at(
            PathBuf::from(OPENRC_CONFIG_DIR),
            PathBuf::from(OPENRC_ENV),
            control,
        )
    }
}

impl WifiClientReconciler<Systemd, Networkd> {
    /// Production reconciler: paths from [`CONFIG_DIR_ENV`] and
    /// [`NETWORK_DIR_ENV`] if set, else the system locations.
    pub fn production() -> Self {
        let config_dir = std::env::var(CONFIG_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_DIR));
        let network_dir = std::env::var(NETWORK_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_NETWORK_DIR));
        Self::new(config_dir, network_dir, Systemd::new(), Networkd)
    }
}

/// Name of the configuration file `wpa_supplicant@<interface>.service` reads.
fn config_file_name(interface: &str) -> String {
    format!("wpa_supplicant-{interface}.conf")
}

/// Name of the supplicant unit instance for `interface`.
fn unit_name(interface: &str) -> String {
    format!("wpa_supplicant@{interface}.service")
}

/// Name of the networkd unit this reconciler renders for `interface`.
fn networkd_file_name(interface: &str) -> String {
    format!("{NETWORKD_PREFIX}{interface}.network")
}

/// Whether `file_name` is a networkd unit owned by this reconciler.
fn is_wifi_client_managed(file_name: &str) -> bool {
    file_name.starts_with(NETWORKD_PREFIX) && file_name.ends_with(".network")
}

/// Check that `interface` is a name the kernel could actually carry.
fn validate_interface(interface: &str) -> Result<()> {
    micad_settings::check_iface_name("wifi.client.interface", interface).map_err(anyhow::Error::msg)
}

/// True when `value` can be carried inside a wpa_supplicant double-quoted
/// string with no way of ending the string early.
fn is_quotable(value: &str) -> bool {
    micad_settings::is_wpa_quotable(value)
}

/// Encode `ssid` as a wpa_supplicant `ssid=` value.
///
/// A plain SSID is quoted, which is what an operator reading the file expects.
/// Anything else — a quote, a backslash, a newline, a control character, any
/// non-ASCII byte — is emitted as wpa_supplicant's unquoted hex form, where the
/// alphabet is `0-9a-f` and so injection is not expressible at all.
fn encode_ssid(ssid: &str) -> String {
    if is_quotable(ssid) {
        format!("\"{ssid}\"")
    } else {
        hex::encode(ssid.as_bytes())
    }
}

/// Encode `psk` as a wpa_supplicant `psk=` value.
///
/// A 64-character hex string is a raw 256-bit PMK and is emitted unquoted;
/// quoting it would make wpa_supplicant read it as a 64-character passphrase,
/// which exceeds the 63-character maximum and makes it reject the whole file.
/// Anything else is a passphrase and is quoted.
fn encode_psk(psk: &str) -> Result<String> {
    if psk.len() == micad_settings::RAW_PMK_LEN && psk.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Ok(psk.to_string());
    }
    // IEEE 802.11i's passphrase bounds, from `micad-settings`, which states the
    // rule beside `WifiNetwork` so every write surface runs the same one. They
    // are checked because wpa_supplicant rejects an out-of-range passphrase by
    // refusing the WHOLE configuration file, which takes every other
    // configured network down with it while the reconcile still reports
    // `applied`. The message never names the length observed.
    micad_settings::validate_wifi_psk(psk).map_err(|message| anyhow!(message))?;
    if !is_quotable(psk) {
        return Err(anyhow!(
            "the pre-shared key contains a character wpa_supplicant configuration \
             cannot carry; use printable ASCII without a quote or a backslash"
        ));
    }
    Ok(format!("\"{psk}\""))
}

/// Render the wpa_supplicant configuration for `client`.
fn render_config(client: &WifiClientSettings) -> Result<String> {
    let mut ordered: Vec<&WifiNetwork> = client.networks.iter().collect();
    ordered.sort_by_key(|network| std::cmp::Reverse(network.priority));

    let mut out = String::from(CONFIG_HEADER);
    out.push_str("ctrl_interface=/run/wpa_supplicant\n");
    out.push_str("update_config=0\n");
    // SAE's password element by both hunting-and-pecking and hash-to-element.
    // The default is the first alone, and WPA3 on 6 GHz (Wi-Fi 6E/7) requires
    // the second: SAE without this still fails there.
    out.push_str("sae_pwe=2\n");
    for network in ordered {
        out.push_str("\nnetwork={\n");
        out.push_str(&format!("\tssid={}\n", encode_ssid(&network.ssid)));
        if network.hidden {
            out.push_str("\tscan_ssid=1\n");
        }
        out.push_str(&format!("\tpriority={}\n", network.priority));
        match &network.psk {
            Some(psk) => {
                let encoded = encode_psk(psk)
                    .with_context(|| format!("render the network {:?}", network.ssid.as_str()))?;
                if encoded.starts_with('"') {
                    // A passphrase: one block joins WPA2, WPA3 (SAE) and
                    // transition-mode networks, and PMF is offered, which
                    // SAE requires and WPA2 may use.
                    out.push_str("\tkey_mgmt=WPA-PSK SAE\n");
                    out.push_str("\tieee80211w=1\n");
                } else {
                    // A raw PMK: SAE derives its keys from the password
                    // itself, so a network stored as a PMK is WPA2 only.
                    out.push_str("\tkey_mgmt=WPA-PSK\n");
                }
                out.push_str(&format!("\tpsk={encoded}\n"));
            }
            None => out.push_str("\tkey_mgmt=NONE\n"),
        }
        out.push_str("}\n");
    }
    Ok(out)
}

/// Render the networkd unit that addresses an associated station link.
fn render_networkd(interface: &str) -> String {
    format!("[Match]\nName={interface}\n\n[Network]\nDHCP=yes\n")
}

impl<C: UnitControl, R: NetworkReload> WifiClientReconciler<C, R> {
    /// Render the networkd unit for `interface` when the station is meant to be
    /// up, remove every unit of this reconciler's that is not the wanted one,
    /// and report the wanted name plus whether anything changed.
    fn apply_networkd(&self, interface: &str, up: bool) -> Result<(Option<String>, bool)> {
        std::fs::create_dir_all(&self.network_dir)
            .with_context(|| format!("create {}", self.network_dir.display()))?;

        let wanted = up.then(|| networkd_file_name(interface));
        let mut changed = false;
        if let Some(name) = &wanted {
            let path = self.network_dir.join(name);
            let rendered = render_networkd(interface);
            if std::fs::read_to_string(&path).ok() != Some(rendered.clone()) {
                std::fs::write(&path, &rendered)
                    .with_context(|| format!("render {}", path.display()))?;
                changed = true;
            }
        }
        for entry in std::fs::read_dir(&self.network_dir)
            .with_context(|| format!("read {}", self.network_dir.display()))?
        {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            if is_wifi_client_managed(file_name) && Some(file_name) != wanted.as_deref() {
                std::fs::remove_file(entry.path())
                    .with_context(|| format!("remove {}", entry.path().display()))?;
                changed = true;
            }
        }
        Ok((wanted, changed))
    }

    /// Bring the supplicant unit to the state the settings ask for, and report
    /// whether any call was needed.
    async fn apply_unit(&self, unit: &str, up: bool, config_changed: bool) -> Result<bool> {
        let mut changed = false;
        if up {
            if !is_enabled(&self.control.unit_file_state(unit).await?) {
                self.control.enable(unit).await?;
                changed = true;
            }
            if is_active(&self.control.active_state(unit).await?) {
                if config_changed {
                    self.control.restart(unit).await?;
                    changed = true;
                }
            } else {
                self.control.start(unit).await?;
                changed = true;
            }
        } else {
            if is_active(&self.control.active_state(unit).await?) {
                self.control.stop(unit).await?;
                changed = true;
            }
            if is_enabled(&self.control.unit_file_state(unit).await?) {
                self.control.disable(unit).await?;
                changed = true;
            }
        }
        Ok(changed)
    }
}

#[async_trait::async_trait]
impl<C: UnitControl, R: NetworkReload> Reconciler for WifiClientReconciler<C, R> {
    fn name(&self) -> &'static str {
        "wifiClient"
    }

    fn subtree(&self) -> &'static str {
        "wifi.client"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let client = &settings.wifi.client;
        validate_interface(&client.interface)?;
        let unit = match self.openrc_env {
            Some(_) => OPENRC_UNIT.to_string(),
            None => unit_name(&client.interface),
        };
        let config_path = self.config_dir.join(config_file_name(&client.interface));

        // "Enabled with nothing to join" is not a reason to run a supplicant;
        // see `Station::Idle`.
        let up = client.enabled && !client.networks.is_empty();

        // The configuration is rendered even while the station is kept down, so
        // the file on disk always describes `wifi.client` and adding the first
        // network is an ordinary configuration change rather than a first
        // render.
        // Unchanged bytes are not rewritten: the configuration lives on STATE.
        let mut config_changed =
            write_config_if_changed(&config_path, &render_config(client)?, CONFIG_MODE)?;
        let (networkd_unit, networkd_changed) = match &self.openrc_env {
            // The service's parameters: a change is a restart, as a changed
            // configuration is.
            Some(env) => {
                let rendered = format!(
                    "# Managed by micad from wifi.client. Do not edit.\ninterface={}\nconfig={}\n",
                    client.interface,
                    config_path.display()
                );
                config_changed |= write_config_if_changed(env, &rendered, 0o644)?;
                (None, false)
            }
            None => self.apply_networkd(&client.interface, up)?,
        };
        if networkd_changed {
            self.reloader.reload().await?;
        }
        let unit_changed = self.apply_unit(&unit, up, config_changed).await?;

        let station = if !client.enabled {
            Station::Disabled
        } else if client.networks.is_empty() {
            Station::Idle
        } else if config_changed || networkd_changed || unit_changed {
            Station::Applied
        } else {
            Station::Unchanged
        };

        // Every value below is public: an SSID is broadcast over the air, and
        // `secured` reports only whether a key exists. The key itself never
        // leaves the 0600 configuration file — this tree is served over D-Bus.
        let networks: Vec<serde_json::Value> = client
            .networks
            .iter()
            .map(|network| {
                json!({
                    "ssid": network.ssid,
                    "hidden": network.hidden,
                    "priority": network.priority,
                    "secured": network.psk.is_some(),
                })
            })
            .collect();

        Ok(json!({
            "enabled": client.enabled,
            "interface": client.interface,
            "station": station.as_str(),
            "networks": networks,
            "config": config_path.display().to_string(),
            "networkdUnit": networkd_unit,
            "unit": unit,
            "activeState": self.control.active_state(&unit).await?,
            "unitFileState": self.control.unit_file_state(&unit).await?,
        }))
    }
}

#[cfg(test)]
mod tests;

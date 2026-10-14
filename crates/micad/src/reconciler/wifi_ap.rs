//! WiFi access-point reconciler: renders hostapd configuration from `wifi.ap`,
//! drives `hostapd@<interface>.service`, and renders the networkd unit that
//! gives the access point its address and its DHCP server. Three system
//! effects, in this order:
//!
//! - `/etc/hostapd/<interface>.conf` is rendered from `wifi.ap`. It carries the
//!   WPA2 pre-shared key, so it is written at 0600.
//! - a networkd `.network` unit for the interface is rendered, carrying the
//!   AP-side address and `DHCPServer=yes`. An access point clients can
//!   associate with but which hands out no address is an access point nothing
//!   can reach, the provisioning UI included.
//! - `hostapd@<interface>.service` is brought to the state `wifi.ap.mode` asks
//!   for.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use micad_settings::{ApMode, Settings, WifiApSettings};
use serde_json::json;

use super::Reconciler;
use super::network::{NetworkReload, Networkd, NoReload};
use super::systemd::{Systemd, UnitControl, is_active, is_enabled};
use crate::fswrite::write_config_if_changed;
use crate::identity;

mod render;
use render::*;

/// Directory hostapd opens its control socket in.
///
/// The socket is how the device can answer "who is connected": hostapd speaks
/// the same request/reply protocol wpa_supplicant does, and without this line
/// it opens no socket at all and the question has no answer.
///
/// Under `/run`, so it is a runtime path on a read-only root and disappears
/// with the boot that made it.
const CONTROL_DIR: &str = "/run/hostapd";

/// Directory the `hostapd@.service` template reads its per-interface
/// configuration from.
const DEFAULT_CONFIG_DIR: &str = "/etc/hostapd";
/// Environment variable overriding the hostapd configuration directory.
const CONFIG_DIR_ENV: &str = "MICAD_HOSTAPD_DIR";
/// Directory networkd reads runtime unit files from.
///
/// Deliberately the same directory and the same override the network and
/// station reconcilers use: all three render into networkd's runtime drop-in
/// directory, and a test that redirects one must redirect the others with it.
const DEFAULT_NETWORK_DIR: &str = "/run/systemd/network";
/// Environment variable overriding the networkd unit directory.
const NETWORK_DIR_ENV: &str = "MICAD_NETWORK_DIR";
/// Environment variable naming the settings file, whose directory is the
/// STATE-backed directory the per-device AP key lives under.
const SETTINGS_PATH_ENV: &str = "MICAD_SETTINGS_PATH";
/// Mode of the rendered configuration: owner-only, because it carries the
/// pre-shared key.
const CONFIG_MODE: u32 = 0o600;
/// Prefix of the networkd units this reconciler owns.
///
/// Two independent properties, both required.
const NETWORKD_PREFIX: &str = "90-wifi-ap-";
/// Header of the rendered configuration.
const CONFIG_HEADER: &str = "# Managed by micad from wifi.ap. Do not edit.\n";
/// Longest SSID IEEE 802.11 allows, in bytes.
const MAX_SSID_BYTES: usize = 32;
/// Prefix of a derived SSID, matching the derived hostname's family so a
/// device's access point is recognisably the same device.
const SSID_PREFIX: &str = "mica-";
/// Characters of the device identifier a derived SSID carries; the same count
/// the derived hostname uses.
const SSID_ID_CHARS: usize = 8;
/// Highest 2.4 GHz channel number; `wifi.ap.channel` is documented as 2.4 GHz.
const MAX_CHANNEL: u8 = 14;
/// Shortest prefix length that still leaves a usable subnet.
const MIN_PREFIX_LEN: u32 = 1;
/// Longest prefix length that can still hold a host and a one-address pool.
const MAX_PREFIX_LEN: u32 = 30;

/// What the reconciler did to the access-point role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccessPoint {
    /// Something changed: the configuration, the networkd unit, or the unit's
    /// runtime state.
    Applied,
    /// The access point was already exactly as configured; nothing was written
    /// and no unit call was made.
    Unchanged,
    /// `wifi.ap.mode` is `off`, so hostapd is stopped and disabled.
    Stopped,
    /// The access point and the station role were both asked for on the same
    /// interface, which one radio cannot do.
    Conflict,
}

impl AccessPoint {
    /// Live-state spelling of this outcome.
    fn as_str(self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Unchanged => "unchanged",
            Self::Stopped => "stopped",
            Self::Conflict => "conflict",
        }
    }
}

/// Live-state spelling of an [`ApMode`].
fn mode_str(mode: ApMode) -> &'static str {
    match mode {
        ApMode::Off => "off",
        ApMode::Provisioning => "provisioning",
        ApMode::Always => "always",
    }
}

/// Reconciler for the `wifi.ap` settings subtree.
pub struct WifiApReconciler<C: UnitControl, R: NetworkReload> {
    /// On an OpenRC root, where the access-point service reads its
    /// parameters and addresses the link and serves DHCP itself; `None` under
    /// networkd.
    openrc_env: Option<PathBuf>,
    config_dir: PathBuf,
    network_dir: PathBuf,
    state_dir: PathBuf,
    control: C,
    reloader: R,
}

impl<C: UnitControl, R: NetworkReload> WifiApReconciler<C, R> {
    /// Create an access-point reconciler writing hostapd configuration into
    /// `config_dir`, networkd units into `network_dir`, reading the per-device
    /// AP key from under `state_dir`, driving the hostapd unit through
    /// `control` and reloading networkd through `reloader`.
    pub fn new(
        config_dir: PathBuf,
        network_dir: PathBuf,
        state_dir: PathBuf,
        control: C,
        reloader: R,
    ) -> Self {
        Self {
            openrc_env: None,
            config_dir,
            network_dir,
            state_dir,
            control,
            reloader,
        }
    }
}

/// The access-point service of an OpenRC root, mica-wifi-ap's (Base): one
/// service told its interface by its environment file.
const OPENRC_UNIT: &str = "mica-wifi-ap.service";
/// What micad tells that service: `interface=`, `config=`, `address=` and
/// `udhcpd=`.
const OPENRC_ENV: &str = "/run/mica/wifi-ap.env";
/// busybox udhcpd's configuration, and the pidfile it names.
const OPENRC_UDHCPD: &str = "/run/mica/wifi-ap-udhcpd.conf";
const OPENRC_UDHCPD_PID: &str = "/run/mica-wifi-ap.udhcpd.pid";
/// The configuration on STATE, writable on an OpenRC root, where no mount
/// binds `/etc/hostapd`.
const OPENRC_CONFIG_DIR: &str = "/var/lib/mica/hostapd";

impl<C: UnitControl> WifiApReconciler<C, NoReload> {
    /// The access point on an OpenRC root: the configuration under
    /// `config_dir`, the service's environment at `env` and udhcpd's
    /// configuration at `udhcpd`, the key read from under `state_dir`.
    pub fn openrc_at(
        config_dir: PathBuf,
        env: PathBuf,
        udhcpd: PathBuf,
        state_dir: PathBuf,
        control: C,
    ) -> Self {
        Self {
            openrc_env: Some(env),
            ..Self::new(config_dir, udhcpd, state_dir, control, NoReload)
        }
    }

    /// Production access point on an OpenRC root.
    pub fn openrc(control: C) -> Self {
        Self::openrc_at(
            PathBuf::from(OPENRC_CONFIG_DIR),
            PathBuf::from(OPENRC_ENV),
            PathBuf::from(OPENRC_UDHCPD),
            production_state_dir(),
            control,
        )
    }
}

/// The directory of the settings file, where the per-device AP key lives.
fn production_state_dir() -> PathBuf {
    std::env::var(SETTINGS_PATH_ENV)
        .ok()
        .and_then(|path| {
            Path::new(&path)
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(Path::to_path_buf)
        })
        .unwrap_or_else(|| PathBuf::from(identity::DEFAULT_STATE_DIR))
}

impl WifiApReconciler<Systemd, Networkd> {
    /// Production reconciler: paths from [`CONFIG_DIR_ENV`],
    /// [`NETWORK_DIR_ENV`] and the directory of [`SETTINGS_PATH_ENV`] if set,
    /// else the system locations.
    pub fn production() -> Self {
        let config_dir = std::env::var(CONFIG_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_CONFIG_DIR));
        let network_dir = std::env::var(NETWORK_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_NETWORK_DIR));
        Self::new(
            config_dir,
            network_dir,
            production_state_dir(),
            Systemd::new(),
            Networkd,
        )
    }
}

/// Name of the configuration file `hostapd@<interface>.service` reads.
fn config_file_name(interface: &str) -> String {
    format!("{interface}.conf")
}

/// Name of the hostapd unit instance for `interface`.
fn unit_name(interface: &str) -> String {
    format!("hostapd@{interface}.service")
}

/// Name of the networkd unit this reconciler renders for `interface`.
fn networkd_file_name(interface: &str) -> String {
    format!("{NETWORKD_PREFIX}{interface}.network")
}

/// Whether `file_name` is a networkd unit owned by this reconciler.
fn is_wifi_ap_managed(file_name: &str) -> bool {
    file_name.starts_with(NETWORKD_PREFIX) && file_name.ends_with(".network")
}

impl<C: UnitControl, R: NetworkReload> WifiApReconciler<C, R> {
    /// The SSID this device advertises, if one can be known.
    ///
    /// `wifi.ap.ssid` when the operator set one, otherwise derived from
    /// `provisioning.deviceId`. `None` only on a device that has neither — an
    /// unprovisioned tree — which is a hard error when the access point is
    /// actually meant to run and merely unknown when it is not.
    fn resolve_ssid(&self, settings: &Settings) -> Option<String> {
        settings
            .wifi
            .ap
            .ssid
            .clone()
            .or_else(|| settings.provisioning.device_id.as_deref().map(derived_ssid))
    }

    /// The pre-shared key the access point should use.
    ///
    /// `wifi.ap.psk` when the operator set one, otherwise the per-device key
    /// generated on STATE at first boot. There is deliberately no constant
    /// fallback: the signed rootfs is byte-identical on every device, so a
    /// baked default would be one WiFi key for the entire fleet.
    fn resolve_psk(&self, ap: &WifiApSettings) -> Result<String> {
        if let Some(psk) = &ap.psk {
            return Ok(psk.clone());
        }
        identity::read_ap_psk(&self.state_dir)
            .context("read the per-device access-point key")?
            .ok_or_else(|| {
                anyhow!(
                    "wifi.ap.psk is unset and this device has no access-point key on STATE; \
                     a fleet-wide default is not an option"
                )
            })
    }

    /// Write the service's environment and udhcpd's configuration; report
    /// whether either changed.
    fn apply_openrc(&self, env: &Path, ap: &WifiApSettings, config: &Path) -> Result<bool> {
        let udhcpd = &self.network_dir;
        let mut changed = write_config_if_changed(
            udhcpd,
            &render_udhcpd(&ap.interface, &ap.address, OPENRC_UDHCPD_PID)?,
            0o644,
        )?;
        let (host, prefix) = parse_cidr(&ap.address)?;
        changed |= write_config_if_changed(
            env,
            &format!(
                "# Managed by micad from wifi.ap. Do not edit.\n\
                 interface={}\nconfig={}\naddress={host}/{prefix}\nudhcpd={}\n",
                ap.interface,
                config.display(),
                udhcpd.display()
            ),
            0o644,
        )?;
        Ok(changed)
    }

    /// Render the networkd unit for `interface` when the access point is meant
    /// to be up, remove every unit of this reconciler's that is not the wanted
    /// one, and report the wanted name plus whether anything changed.
    fn apply_networkd(
        &self,
        interface: &str,
        address: &str,
        up: bool,
    ) -> Result<(Option<String>, bool)> {
        std::fs::create_dir_all(&self.network_dir)
            .with_context(|| format!("create {}", self.network_dir.display()))?;

        let wanted = up.then(|| networkd_file_name(interface));
        let mut changed = false;
        if let Some(name) = &wanted {
            let path = self.network_dir.join(name);
            let rendered = render_networkd(interface, address)?;
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
            if is_wifi_ap_managed(file_name) && Some(file_name) != wanted.as_deref() {
                std::fs::remove_file(entry.path())
                    .with_context(|| format!("remove {}", entry.path().display()))?;
                changed = true;
            }
        }
        Ok((wanted, changed))
    }

    /// Bring the hostapd unit to the state the settings ask for, and report
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
impl<C: UnitControl, R: NetworkReload> Reconciler for WifiApReconciler<C, R> {
    fn name(&self) -> &'static str {
        "wifiAp"
    }

    fn subtree(&self) -> &'static str {
        // The whole `wifi` tree, not just `wifi.ap`: the conflict check below
        // reads `wifi.client`, so a write there must re-run this reconciler
        // too. Declaring only the narrower subtree left the cross-subtree
        // dependency invisible to the bus's overlap test — enabling the
        // station reported no conflict until the next full `apply_all`, and
        // disabling it left an access point parked in `conflict` until
        // something happened to touch `wifi.ap`. The station reconciler keeps
        // its narrow subtree: it reads nothing outside `wifi.client`.
        "wifi"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let ap = &settings.wifi.ap;
        validate_interface(&ap.interface)?;
        let unit = match self.openrc_env {
            Some(_) => OPENRC_UNIT.to_string(),
            None => unit_name(&ap.interface),
        };
        let config_path = self.config_dir.join(config_file_name(&ap.interface));
        let ssid = self.resolve_ssid(settings);
        let up = ap.mode != ApMode::Off;

        // One radio cannot be a station and an access point at once. Reported,
        // never fought over: see `AccessPoint::Conflict`. This returns before
        // any I/O and before any bus call, so a conflicting tree leaves the
        // radio exactly as the station reconciler set it.
        if up && settings.wifi.client.enabled && settings.wifi.client.interface == ap.interface {
            return Ok(json!({
                "mode": mode_str(ap.mode),
                "interface": ap.interface,
                "accessPoint": AccessPoint::Conflict.as_str(),
                "ssid": ssid,
                "unit": unit,
                "conflict": format!(
                    "wifi.client is enabled on {}, so the access point cannot use it: \
                     one radio cannot be a station and an access point at once",
                    ap.interface
                ),
            }));
        }

        // The configuration is rendered only while the access point is meant to
        // run. Unlike the station's, it cannot be rendered "for later": its key
        // comes from STATE, and a device that has not been provisioned yet has
        // none — rendering unconditionally would make the default tree
        // (`mode: off`) fail on every reconcile.
        let mut config_changed = false;
        if up {
            let ssid = ssid.as_deref().ok_or_else(|| {
                anyhow!("wifi.ap.ssid is unset and this device has no identity to derive one from")
            })?;
            let psk = self.resolve_psk(ap)?;
            let rendered = render_config(ap, ssid, &psk)?;
            config_changed = write_config_if_changed(&config_path, &rendered, CONFIG_MODE)?;
        }

        let (networkd_unit, networkd_changed) = match &self.openrc_env {
            Some(env) if up => {
                config_changed |= self.apply_openrc(env, ap, &config_path)?;
                (None, false)
            }
            Some(_) => (None, false),
            None => self.apply_networkd(&ap.interface, &ap.address, up)?,
        };
        if networkd_changed {
            self.reloader.reload().await?;
        }
        let unit_changed = self.apply_unit(&unit, up, config_changed).await?;

        let access_point = if !up {
            AccessPoint::Stopped
        } else if config_changed || networkd_changed || unit_changed {
            AccessPoint::Applied
        } else {
            AccessPoint::Unchanged
        };

        let ssid_source = if ap.ssid.is_some() {
            "settings"
        } else {
            "derived"
        };

        // Every value below is public: an SSID is broadcast over the air, and
        // `secured` reports only whether a key exists. The key itself never
        // leaves the 0600 configuration file — this tree is served over D-Bus.
        Ok(json!({
            "mode": mode_str(ap.mode),
            "interface": ap.interface,
            "accessPoint": access_point.as_str(),
            "ssid": ssid,
            "ssidSource": ssid_source,
            "channel": ap.channel,
            "countryCode": ap.country_code,
            "address": ap.address,
            "secured": true,
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

//! The board's physical network interfaces: `/usr/share/mica/board/interfaces.json`,
//! shipped by the board package (`docs/mica-core.md` section 3.12).
//!
//! A board that declares its ports names each one with its kind, its label,
//! its role and what it is known to do; a generic board says `discover`, and a
//! root without the file is one. A declaration adds to the observation, gives
//! a port's `internet` its default and keeps a hidden port unmanaged. It
//! changes no stored setting.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

use micad_settings::IfaceSettings;
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The declaration, relative to the root.
pub const BOARD_FILE: &str = "usr/share/mica/board/interfaces.json";
const VERSION: u32 = 1;
/// The longest label, in characters.
const MAX_LABEL: usize = 16;
const BANDS: [&str; 3] = ["2.4", "5", "6"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortKind {
    Ethernet,
    Wifi,
    Can,
}

impl PortKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ethernet => "ethernet",
            Self::Wifi => "wifi",
            Self::Can => "can",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Uplink,
    Internal,
}

/// One declared port. A capability left out is not known.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Port {
    pub name: String,
    pub kind: PortKind,
    pub label: String,
    #[serde(default)]
    pub role: Option<Role>,
    #[serde(default)]
    pub speed_mbps: Option<u32>,
    #[serde(default)]
    pub ap: Option<bool>,
    #[serde(default)]
    pub bands: Option<Vec<String>>,
    #[serde(default)]
    pub fd: Option<bool>,
    /// A port the product does not expose: neither listed nor managed unless
    /// the tree has an entry for it.
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    #[serde(default)]
    discover: bool,
    #[serde(default)]
    interfaces: Option<Vec<Port>>,
}

/// What the board says about its ports.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Board {
    /// Nothing: the ports are what the kernel lists, as without a file.
    #[default]
    Discover,
    /// These ports, in this order.
    Declared(Vec<Port>),
}

impl Board {
    /// Read a declaration. Unknown members and versions are refused.
    pub fn parse(text: &str) -> Result<Self, String> {
        let file: File = serde_json::from_str(text).map_err(|err| err.to_string())?;
        if file.version != VERSION {
            return Err(format!("version {} is not {VERSION}", file.version));
        }
        match (file.discover, file.interfaces) {
            (true, None) => Ok(Self::Discover),
            (true, Some(_)) => Err("a board that discovers declares no interfaces".to_string()),
            (false, Some(ports)) if !ports.is_empty() => {
                check(&ports)?;
                Ok(Self::Declared(ports))
            }
            (false, _) => Err("a board declares its interfaces or says discover: true".to_string()),
        }
    }

    /// The board under `root`: `discover` without a file, and with one this
    /// build refuses, which is logged.
    pub fn load(root: &Path) -> Self {
        let path = root.join(BOARD_FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Self::Discover,
            Err(err) => {
                tracing::error!(path = %path.display(), error = %err, "the board's interfaces could not be read; discovering them");
                return Self::Discover;
            }
        };
        Self::parse(&text).unwrap_or_else(|err| {
            tracing::error!(path = %path.display(), error = %err, "the board's interfaces are refused; discovering them");
            Self::Discover
        })
    }

    fn ports(&self) -> &[Port] {
        match self {
            Self::Discover => &[],
            Self::Declared(ports) => ports,
        }
    }

    fn port(&self, name: &str) -> Option<&Port> {
        self.ports().iter().find(|port| port.name == name)
    }

    /// Whether the board declares `name` hidden.
    #[must_use]
    pub fn hidden(&self, name: &str) -> bool {
        self.port(name).is_some_and(|port| port.hidden)
    }

    /// Whether `name` stays off the internet unless its entry says otherwise.
    #[must_use]
    pub fn internal(&self, name: &str) -> bool {
        self.port(name)
            .is_some_and(|port| port.role == Some(Role::Internal))
    }

    /// `network` with each internal port's absent `internet` made `false`.
    #[must_use]
    pub fn effective(
        &self,
        network: &BTreeMap<String, IfaceSettings>,
    ) -> BTreeMap<String, IfaceSettings> {
        network
            .iter()
            .map(|(name, cfg)| {
                let mut cfg = cfg.clone();
                if cfg.internet.is_none() && self.internal(name) {
                    cfg.internet = Some(false);
                }
                (name.clone(), cfg)
            })
            .collect()
    }

    /// Add the declaration to an observation: `board` at the top, each
    /// listed interface's `declared` (or `null`), and each declared port the
    /// device lacks as `{name, present: false, declared}`. A hidden port is
    /// taken out unless `configured` names it.
    pub fn annotate(&self, observed: &mut Value, configured: &BTreeSet<String>) {
        let Some(root) = observed.as_object_mut() else {
            return;
        };
        let word = match self {
            Self::Discover => "discover",
            Self::Declared(_) => "declared",
        };
        root.insert("board".to_string(), json!(word));
        let Some(entries) = observed
            .pointer_mut("/interfaces/entries")
            .and_then(Value::as_array_mut)
        else {
            return;
        };
        entries.retain(|entry| {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            !self.hidden(name) || configured.contains(name)
        });
        for entry in entries.iter_mut() {
            let name = entry
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let declared = self
                .ports()
                .iter()
                .position(|port| port.name == name)
                .map_or(Value::Null, |order| {
                    declared_json(&self.ports()[order], order)
                });
            if let Some(entry) = entry.as_object_mut() {
                entry.insert("declared".to_string(), declared);
            }
        }
        for (order, port) in self.ports().iter().enumerate() {
            if port.hidden && !configured.contains(&port.name) {
                continue;
            }
            if entries
                .iter()
                .any(|entry| entry.get("name").and_then(Value::as_str) == Some(port.name.as_str()))
            {
                continue;
            }
            entries.push(json!({
                "name": port.name,
                "present": false,
                "declared": declared_json(port, order),
            }));
        }
    }

    /// Log, once each, a declared port the kernel does not list and a
    /// physical link under `root`'s `/sys/class/net` the board does not
    /// declare.
    pub fn log_mismatches(&self, root: &Path) {
        let Self::Declared(ports) = self else {
            return;
        };
        let dir = root.join(crate::network_state::NET_CLASS_DIR);
        let links: BTreeSet<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .collect();
        for port in ports {
            if !links.contains(&port.name) {
                tracing::warn!(interface = %port.name, label = %port.label, "a declared interface is absent");
            }
        }
        for link in &links {
            if dir.join(link).join("device").exists()
                && !ports.iter().any(|port| &port.name == link)
            {
                tracing::warn!(interface = %link, "a physical interface the board does not declare");
            }
        }
    }
}

/// The board of this host, read once.
pub fn host() -> &'static Board {
    static HOST: OnceLock<Board> = OnceLock::new();
    HOST.get_or_init(|| Board::load(Path::new("/")))
}

fn check(ports: &[Port]) -> Result<(), String> {
    let mut names = BTreeSet::new();
    for port in ports {
        micad_settings::check_iface_name("interface name", &port.name)?;
        let name = &port.name;
        if !names.insert(name.as_str()) {
            return Err(format!("{name} is declared twice"));
        }
        let length = port.label.chars().count();
        if length == 0 || length > MAX_LABEL || port.label.trim() != port.label {
            return Err(format!(
                "{name}: the label is 1 to {MAX_LABEL} characters with no outer spaces"
            ));
        }
        let kind = port.kind;
        for (field, present, kinds) in [
            (
                "role",
                port.role.is_some(),
                &[PortKind::Ethernet, PortKind::Wifi][..],
            ),
            (
                "speedMbps",
                port.speed_mbps.is_some(),
                &[PortKind::Ethernet][..],
            ),
            ("ap", port.ap.is_some(), &[PortKind::Wifi][..]),
            ("bands", port.bands.is_some(), &[PortKind::Wifi][..]),
            ("fd", port.fd.is_some(), &[PortKind::Can][..]),
        ] {
            if present && !kinds.contains(&kind) {
                return Err(format!(
                    "{name}: {field} is not for a {} port",
                    kind.as_str()
                ));
            }
        }
        if port.speed_mbps == Some(0) {
            return Err(format!("{name}: speedMbps is above 0"));
        }
        if let Some(bands) = &port.bands {
            let unique: BTreeSet<&str> = bands.iter().map(String::as_str).collect();
            if bands.is_empty()
                || unique.len() != bands.len()
                || unique.iter().any(|band| !BANDS.contains(band))
            {
                return Err(format!("{name}: bands are distinct ones of 2.4, 5 and 6"));
            }
        }
    }
    Ok(())
}

fn declared_json(port: &Port, order: usize) -> Value {
    let mut capabilities = Map::new();
    if let Some(speed) = port.speed_mbps {
        capabilities.insert("speedMbps".to_string(), json!(speed));
    }
    if let Some(ap) = port.ap {
        capabilities.insert("ap".to_string(), json!(ap));
    }
    if let Some(bands) = &port.bands {
        capabilities.insert("bands".to_string(), json!(bands));
    }
    if let Some(fd) = port.fd {
        capabilities.insert("fd".to_string(), json!(fd));
    }
    let mut declared = json!({
        "kind": port.kind.as_str(),
        "label": port.label,
        "order": order,
        "capabilities": capabilities,
    });
    if port.kind != PortKind::Can {
        let role = match port.role.unwrap_or(Role::Uplink) {
            Role::Uplink => "uplink",
            Role::Internal => "internal",
        };
        declared["role"] = json!(role);
    }
    declared
}

/// `micad board check <file>`: whether this build takes `file`.
pub fn main(args: &[String]) -> anyhow::Result<()> {
    let [command, file] = args else {
        anyhow::bail!("usage: micad board check <file>");
    };
    if command != "check" {
        anyhow::bail!("usage: micad board check <file>");
    }
    let text = std::fs::read_to_string(file).map_err(|err| anyhow::anyhow!("{file}: {err}"))?;
    match Board::parse(&text).map_err(|err| anyhow::anyhow!("{file}: {err}"))? {
        Board::Discover => println!("{file}: discover"),
        Board::Declared(ports) => println!("{file}: {} interfaces", ports.len()),
    }
    Ok(())
}

#[cfg(test)]
mod tests;

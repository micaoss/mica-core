//! Declared containers and their rules.

use std::collections::BTreeMap;

/// Container engine policy, reconciled by `ContainerReconciler`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContainerSettings {
    /// Whether mica-containerd runs and the declared containers with it.
    pub enabled: bool,
    /// The containers this device runs, by name.
    ///
    /// Empty and skipped when empty, so a device that declares none writes the
    /// document it wrote before containers could be declared at all.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub units: BTreeMap<String, ContainerUnit>,
}

/// One container, as the fields mica-containerd's spec needs.
///
/// A deliberately small subset of that spec: enough to declare a container,
/// and no field micad would have to keep honest forever for nobody. Unknown
/// keys are refused like everywhere else.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContainerUnit {
    /// The image reference, tag included.
    pub image: String,
    /// The command to run instead of the image's own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// Environment variables passed into the container.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environment: BTreeMap<String, String>,
    /// Ports published from the host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<PublishedPort>,
    /// Host paths mounted into the container.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeMount>,
    /// What mica-containerd does when the container exits.
    #[serde(default)]
    pub restart: RestartPolicy,
    /// Whether the container starts at boot.
    #[serde(rename = "autoStart", default)]
    pub auto_start: bool,
    /// The most processes the container may run, 1 to 65536.
    ///
    /// **Absent is not unlimited**: it is podman's own ceiling, 2048, which
    /// every container has whether it asks or not. A value overrides it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pids: Option<u32>,
    /// The memory the container may use, `<n>k`, `<n>m` or `<n>g`, at least
    /// 6m (podman's floor). **Absent is unlimited**, unlike `pids`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    /// The CPUs the container may use, as a number of CPUs: `0.5` is half of
    /// one, `2` two, at most three decimals. **Absent is unlimited**, unlike
    /// `pids`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
}

/// The most processes a declared `pids` may allow.
pub const MAX_CONTAINER_PIDS: u32 = 65536;
/// podman's smallest memory ceiling, in bytes: a smaller one refuses to start.
const MIN_CONTAINER_MEMORY: u64 = 6 * 1024 * 1024;

/// Refuse limits podman would refuse or misread, naming the container.
fn validate_limits(name: &str, unit: &ContainerUnit) -> Result<(), String> {
    if let Some(pids) = unit.pids
        && !(1..=MAX_CONTAINER_PIDS).contains(&pids)
    {
        return Err(format!(
            "container {name:?} limits pids to {pids}; a limit is 1 to {MAX_CONTAINER_PIDS} (absent is podman's 2048)"
        ));
    }
    if let Some(memory) = &unit.memory {
        let bytes = memory
            .strip_suffix(['k', 'K'])
            .map(|n| (n, 1024))
            .or_else(|| memory.strip_suffix(['m', 'M']).map(|n| (n, 1024 * 1024)))
            .or_else(|| {
                memory
                    .strip_suffix(['g', 'G'])
                    .map(|n| (n, 1024 * 1024 * 1024))
            })
            .filter(|(n, _)| {
                !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0')
            })
            .and_then(|(n, unit)| n.parse::<u64>().ok()?.checked_mul(unit));
        match bytes {
            Some(bytes) if bytes >= MIN_CONTAINER_MEMORY => {}
            _ => {
                return Err(format!(
                    "container {name:?} limits memory to {memory:?}; a limit is a whole number with k, m or g, at least 6m"
                ));
            }
        }
    }
    if let Some(cpu) = &unit.cpu {
        let (whole, fraction) = cpu.split_once('.').unwrap_or((cpu, ""));
        let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
        let valid = !whole.is_empty()
            && whole.len() <= 4
            && digits(whole)
            && fraction.len() <= 3
            && digits(fraction)
            && (cpu.contains('.') == !fraction.is_empty())
            && cpu
                .parse::<f64>()
                .is_ok_and(|value| value > 0.0 && value <= 1024.0);
        if !valid {
            return Err(format!(
                "container {name:?} limits cpu to {cpu:?}; a limit is a number of CPUs above 0, such as 0.5 or 2, with at most three decimals"
            ));
        }
    }
    Ok(())
}

/// One published port: a host port, the container port behind it, and the
/// protocol.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishedPort {
    /// The port on the host.
    pub host: u16,
    /// The port inside the container.
    pub container: u16,
    /// `tcp` or `udp`.
    #[serde(default)]
    pub protocol: PortProtocol,
}

/// The transport a published port carries.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum PortProtocol {
    /// TCP.
    #[default]
    Tcp,
    /// UDP.
    Udp,
}

impl PortProtocol {
    /// The spelling podman takes in a `PublishPort=` line.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

/// One bind mount from the device into the container.
///
/// **The host path is bounded to `/mica/`** by [`validate_container_units`].
/// A bind of `/` or `/etc` hands the device's root filesystem to whatever the
/// image runs, and the settings file is writable without apid, so the bound is
/// stated here rather than only on the write path.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeMount {
    /// The path on the device.
    pub host: String,
    /// Where it appears inside the container.
    pub container: String,
    /// Whether the container sees it read-only.
    #[serde(rename = "readOnly", default)]
    pub read_only: bool,
}

/// What mica-containerd does when a container exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    /// Leave it stopped.
    #[default]
    No,
    /// Restart it when it fails.
    OnFailure,
    /// Restart it whenever it stops.
    Always,
}

impl RestartPolicy {
    /// The spelling mica-containerd takes as a restart `policy`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::No => "no",
            Self::OnFailure => "on-failure",
            Self::Always => "always",
        }
    }
}

/// The prefix every container volume's host path must sit under.
///
/// DATA, and nothing else. `/mica/` is the operator's half of the device;
/// everything outside it is either the signed read-only root or the management
/// plane's own state.
pub const CONTAINER_VOLUME_ROOT: &str = "/mica/";

/// The longest a container name may be.
///
/// It becomes the container's name in podman, and in the path of every verb.
pub(super) const MAX_CONTAINER_NAME_LEN: usize = 64;

/// Refuse a container map no device could run.
///
/// Four rules, each of them a configuration the device could not use:
///
/// - a name that is not letters, digits, `-` and `_`;
/// - an empty image reference;
/// - one host port published by two containers, which is two containers
///   racing for the same listener;
/// - a volume whose host path is not under [`CONTAINER_VOLUME_ROOT`], or that
///   climbs out of it with `..`.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_container_units(units: &BTreeMap<String, ContainerUnit>) -> Result<(), String> {
    let mut claimed: BTreeMap<(u16, PortProtocol), &str> = BTreeMap::new();
    for (name, unit) in units {
        if name.is_empty() || name.len() > MAX_CONTAINER_NAME_LEN {
            return Err(format!(
                "container name {name:?} is empty or longer than {MAX_CONTAINER_NAME_LEN} characters"
            ));
        }
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(format!(
                "container name {name:?} contains a character a container name cannot have; use letters, digits, `-` and `_`"
            ));
        }
        if unit.image.trim().is_empty() {
            return Err(format!("container {name:?} declares no image"));
        }
        validate_limits(name, unit)?;
        for port in &unit.publish {
            if let Some(other) = claimed.insert((port.host, port.protocol), name) {
                return Err(format!(
                    "containers {other:?} and {name:?} both publish host port {}/{}",
                    port.host,
                    port.protocol.as_str()
                ));
            }
        }
        for volume in &unit.volumes {
            if !volume.host.starts_with(CONTAINER_VOLUME_ROOT) || volume.host.contains("..") {
                return Err(format!(
                    "container {name:?} mounts {:?}; a container volume's host path is under {CONTAINER_VOLUME_ROOT} and may not climb out of it",
                    volume.host
                ));
            }
            if !volume.container.starts_with('/') {
                return Err(format!(
                    "container {name:?} mounts {:?} at {:?}, which is not an absolute path inside the container",
                    volume.host, volume.container
                ));
            }
        }
    }
    Ok(())
}

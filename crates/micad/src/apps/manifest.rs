//! `mica-app.json`, the manifest at the top of an app bundle.

use std::collections::BTreeMap;
use std::path::Path;

use micad_settings::Init;

/// The schema this build reads; any other is refused.
pub const SCHEMA_VERSION: u32 = 1;
/// The most units one app ships.
pub const MAX_UNITS: usize = 16;
/// The most variables in `env`.
const MAX_ENV: usize = 64;
/// The longest version string.
const MAX_VERSION_LEN: usize = 64;
/// The longest service name.
const MAX_NAME_LEN: usize = 32;
/// The longest app name: its account, `mica-app-<name>`, is at most 32.
const MAX_APP_NAME_LEN: usize = 23;
/// The architectures a bundle may name, and `all`.
const ARCHES: [&str; 4] = ["amd64", "arm64", "armhf", "all"];
/// The most services one app declares.
const MAX_SERVICES: usize = 16;
/// The most devices one app is given.
const MAX_DEVICES: usize = 16;
/// The most arguments of a service's command.
const MAX_ARGS: usize = 64;
/// The longest argument.
const MAX_ARG_LEN: usize = 4096;
/// The variables every app has, besides its `env`.
pub const APP_VARIABLES: [&str; 4] = ["APP_NAME", "APP_VERSION", "APP_DIR", "APP_DATA"];

/// Every capability Linux names.
const CAPABILITIES: [&str; 41] = [
    "CAP_CHOWN",
    "CAP_DAC_OVERRIDE",
    "CAP_DAC_READ_SEARCH",
    "CAP_FOWNER",
    "CAP_FSETID",
    "CAP_KILL",
    "CAP_SETGID",
    "CAP_SETUID",
    "CAP_SETPCAP",
    "CAP_LINUX_IMMUTABLE",
    "CAP_NET_BIND_SERVICE",
    "CAP_NET_BROADCAST",
    "CAP_NET_ADMIN",
    "CAP_NET_RAW",
    "CAP_IPC_LOCK",
    "CAP_IPC_OWNER",
    "CAP_SYS_MODULE",
    "CAP_SYS_RAWIO",
    "CAP_SYS_CHROOT",
    "CAP_SYS_PTRACE",
    "CAP_SYS_PACCT",
    "CAP_SYS_ADMIN",
    "CAP_SYS_BOOT",
    "CAP_SYS_NICE",
    "CAP_SYS_RESOURCE",
    "CAP_SYS_TIME",
    "CAP_SYS_TTY_CONFIG",
    "CAP_MKNOD",
    "CAP_LEASE",
    "CAP_AUDIT_WRITE",
    "CAP_AUDIT_CONTROL",
    "CAP_SETFCAP",
    "CAP_MAC_OVERRIDE",
    "CAP_MAC_ADMIN",
    "CAP_SYSLOG",
    "CAP_WAKE_ALARM",
    "CAP_BLOCK_SUSPEND",
    "CAP_AUDIT_READ",
    "CAP_PERFMON",
    "CAP_BPF",
    "CAP_CHECKPOINT_RESTORE",
];
/// The capabilities an app running as its own user may have: each stays
/// within the app's own sockets, memory and timers. An app that needs another
/// runs as root.
const ALLOWED_CAPABILITIES: [&str; 7] = [
    "CAP_NET_ADMIN",
    "CAP_NET_RAW",
    "CAP_NET_BIND_SERVICE",
    "CAP_NET_BROADCAST",
    "CAP_IPC_LOCK",
    "CAP_WAKE_ALARM",
    "CAP_BLOCK_SUSPEND",
];

/// What a bundle says it is.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    pub name: String,
    pub version: String,
    pub arch: Vec<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub units: Vec<UnitEntry>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_run: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<UpdateSpec>,
    #[serde(default)]
    pub seed_policy: SeedPolicy,
    #[serde(default)]
    pub privileges: Privileges,
    /// What an app running as its own user runs; micad writes their units.
    #[serde(default)]
    pub services: Vec<Service>,
}

/// Who an app runs as, and what it is given beyond its own files.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Privileges {
    #[serde(default)]
    pub user: RunAs,
    /// `CAP_*` names its services keep.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Device nodes its services may open.
    #[serde(default)]
    pub devices: Vec<String>,
}

/// An app's own user, or root: the whole device.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunAs {
    #[default]
    App,
    Root,
}

/// One service of an app that runs as its own user.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Service {
    pub name: String,
    /// A file in `files/`, then its arguments; `${VAR}` names an `APP_*` or
    /// `env` variable.
    pub exec: Vec<String>,
    /// Whether it starts at boot, and when the app is installed.
    #[serde(default)]
    pub enable: bool,
}

/// One unit, for each init it is shipped for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub systemd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrc: Option<String>,
    /// Whether it starts at boot, and when the app is installed.
    #[serde(default)]
    pub enable: bool,
}

/// Where the app's newer versions are announced.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSpec {
    pub url: String,
    /// Install a newer version on its own, rather than only report it.
    #[serde(default)]
    pub auto: bool,
}

/// Whether a later image's seed of this app is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeedPolicy {
    /// Upgraded to a newer seed while it is still the seeded version.
    #[default]
    FollowImage,
    /// Seeded once and never again.
    Once,
}

/// One rule a bundle breaks: its code and what broke it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Problem {
    pub rule: &'static str,
    pub detail: String,
}

impl Problem {
    pub fn new(rule: &'static str, detail: impl Into<String>) -> Self {
        Self {
            rule,
            detail: detail.into(),
        }
    }
}

/// The units of `manifest` this init runs: each entry's side for it, and
/// each service's, and whether it is enabled.
pub fn units_for(manifest: &Manifest, init: Init) -> Vec<(String, bool)> {
    manifest
        .units
        .iter()
        .filter_map(|unit| {
            match init {
                Init::Systemd => unit.systemd.clone(),
                Init::Openrc => unit.openrc.clone(),
            }
            .map(|name| (name, unit.enable))
        })
        .chain(
            manifest
                .services
                .iter()
                .map(|service| (service_unit(&service.name, init), service.enable)),
        )
        .collect()
}

/// The unit micad writes for service `name`.
pub fn service_unit(name: &str, init: Init) -> String {
    match init {
        Init::Systemd => format!("{name}.service"),
        Init::Openrc => name.to_string(),
    }
}

/// `arg` with each `${VAR}` replaced by `vars`' value; None when it names a
/// variable `vars` lacks or has a `$` that starts none.
pub fn expand(arg: &str, vars: &BTreeMap<String, String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = arg;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        let name_end = rest[at..].strip_prefix("${")?.find('}')?;
        let name = &rest[at + 2..at + 2 + name_end];
        out.push_str(vars.get(name)?);
        rest = &rest[at + 3 + name_end..];
    }
    out.push_str(rest);
    Some(out)
}

/// `path` relative to the bundle and inside `files/`.
fn is_bundle_file(path: &str) -> bool {
    path.strip_prefix("files/").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
    })
}

/// A device node's path: under `/dev`, plain characters, no `..`; or a
/// pattern, such a path ending with `*`, of a class of devices.
fn is_device(path: &str) -> bool {
    let node = path.strip_suffix('*').unwrap_or(path);
    (node == path || super::devices::class(path).is_some())
        && node.strip_prefix("/dev/").is_some_and(|rest| {
            rest.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part.bytes().all(|b| {
                        b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':')
                    })
            })
        })
}

/// Whether `name` can be an app's: 1 to 23 of `[a-z0-9-]`, not starting with
/// `-`.
pub fn is_app_name(name: &str) -> bool {
    name.len() <= MAX_APP_NAME_LEN && is_service_name(name)
}

/// Whether `name` can be a service's: 1 to 32 of `[a-z0-9-]`, not starting
/// with `-`.
fn is_service_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `version` is a version in dpkg's order, short enough.
pub fn is_version(version: &str) -> bool {
    version.len() <= MAX_VERSION_LEN
        && !version.contains('/')
        && mica_deploy::debian_version::compare(version, version).is_some()
}

/// Every rule `manifest`, unpacked at `dir`, breaks on a device running
/// `init` on `arch`.
pub fn check(manifest: &Manifest, dir: &Path, init: Init, arch: &str) -> Vec<Problem> {
    let mut problems = check_fields(manifest, init, arch);
    problems.extend(check_files(manifest, dir, init));
    problems
}

/// Every rule `manifest` breaks on its own, before its files are read.
pub fn check_fields(manifest: &Manifest, init: Init, arch: &str) -> Vec<Problem> {
    let mut problems = Vec::new();
    let mut fail = |rule, detail: String| problems.push(Problem::new(rule, detail));
    if manifest.schema_version != SCHEMA_VERSION {
        fail(
            "manifest",
            format!(
                "schemaVersion {} is not {SCHEMA_VERSION}",
                manifest.schema_version
            ),
        );
    }
    if !is_app_name(&manifest.name) {
        fail(
            "manifest",
            format!(
                "name {:?} is not 1 to {MAX_APP_NAME_LEN} of a-z, 0-9 and -",
                manifest.name
            ),
        );
    }
    if !is_version(&manifest.version) {
        fail(
            "manifest",
            format!("version {:?} is not a version", manifest.version),
        );
    }
    if manifest.arch.is_empty() || manifest.arch.iter().any(|a| !ARCHES.contains(&a.as_str())) {
        fail(
            "manifest",
            format!("arch is a list of {}", ARCHES.join(", ")),
        );
    } else if !manifest.arch.iter().any(|a| a == arch || a == "all") {
        fail(
            "arch",
            format!(
                "the bundle is for {}; this device is {arch}",
                manifest.arch.join(", ")
            ),
        );
    }
    if manifest.units.len() > MAX_UNITS {
        fail("too_many", format!("more than {MAX_UNITS} units"));
    }
    for unit in &manifest.units {
        if unit.systemd.is_none() && unit.openrc.is_none() {
            fail(
                "manifest",
                "a unit names neither a systemd nor an openrc file".into(),
            );
        }
        if let Some(name) = &unit.systemd
            && (!micad_settings::is_systemd_unit_name(name)
                || micad_settings::is_systemd_template(name))
        {
            fail("manifest", format!("{name:?} is not a systemd unit name"));
        }
        if let Some(name) = &unit.openrc
            && !micad_settings::is_openrc_script_name(name)
        {
            fail("manifest", format!("{name:?} is not an OpenRC script name"));
        }
    }
    let ours = manifest.units.iter().any(|unit| match init {
        Init::Systemd => unit.systemd.is_some(),
        Init::Openrc => unit.openrc.is_some(),
    });
    if !manifest.units.is_empty() && !ours {
        fail(
            "init",
            format!("the bundle has no unit for {}", init.as_str()),
        );
    }
    if manifest.env.len() > MAX_ENV {
        fail("too_many", format!("more than {MAX_ENV} env variables"));
    }
    for (key, value) in &manifest.env {
        let key_ok = key
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_uppercase() || b == b'_')
            && key
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
            && !key.starts_with("APP_");
        if !key_ok {
            fail(
                "manifest",
                format!("env {key:?} is not an upper-case name outside APP_"),
            );
        }
        if value
            .chars()
            .any(|c| matches!(c, '"' | '\'' | '\\' | '$' | '`') || c.is_control())
        {
            fail(
                "manifest",
                format!("env {key} carries a quote, a backslash, a $, a ` or a control character"),
            );
        }
    }
    if let Some(hook) = &manifest.first_run
        && !hook
            .strip_prefix("hooks/")
            .is_some_and(|rest| !rest.is_empty() && !rest.contains('/') && rest != "..")
    {
        fail(
            "manifest",
            format!("firstRun {hook:?} is not a file in hooks/"),
        );
    }
    if let Some(update) = &manifest.update
        && !update.url.starts_with("https://")
    {
        fail("manifest", "update.url is an https URL".into());
    }
    check_privileges(manifest, &mut fail);
    problems
}

/// The rules of `privileges` and `services`.
fn check_privileges(manifest: &Manifest, fail: &mut impl FnMut(&'static str, String)) {
    let privileges = &manifest.privileges;
    match privileges.user {
        RunAs::Root => {
            if !privileges.capabilities.is_empty() || !privileges.devices.is_empty() {
                fail(
                    "privileges",
                    "capabilities and devices are for an app that runs as its own user; root has them all".into(),
                );
            }
            if !manifest.services.is_empty() {
                fail(
                    "services_need_app",
                    "services are for an app that runs as its own user; a root app ships units/"
                        .into(),
                );
            }
        }
        RunAs::App => {
            if !manifest.units.is_empty() {
                fail(
                    "units_need_root",
                    "units are for an app that runs as root; an app that runs as its own user declares services".into(),
                );
            }
        }
    }
    for cap in &privileges.capabilities {
        if !CAPABILITIES.contains(&cap.as_str()) {
            fail("capability", format!("{cap:?} is not a capability"));
        } else if !ALLOWED_CAPABILITIES.contains(&cap.as_str()) {
            fail(
                "capability",
                format!("{cap} reaches past the app; an app that needs it runs as root"),
            );
        }
    }
    if privileges.devices.len() > MAX_DEVICES {
        fail("too_many", format!("more than {MAX_DEVICES} devices"));
    }
    for device in &privileges.devices {
        if !is_device(device) {
            fail(
                "device",
                format!(
                    "{device:?} is not a device under /dev, or a pattern of a serial port, a GPIO chip, an I2C or an SPI bus"
                ),
            );
        }
    }
    if manifest.services.len() > MAX_SERVICES {
        fail("too_many", format!("more than {MAX_SERVICES} services"));
    }
    let vars: BTreeMap<String, String> = APP_VARIABLES
        .iter()
        .map(|name| (*name).to_string())
        .chain(manifest.env.keys().cloned())
        .map(|name| (name, String::new()))
        .collect();
    let mut names = std::collections::BTreeSet::new();
    for service in &manifest.services {
        if !is_service_name(&service.name) {
            fail(
                "manifest",
                format!(
                    "service {:?} is not 1 to {MAX_NAME_LEN} of a-z, 0-9 and -",
                    service.name
                ),
            );
        } else if !names.insert(&service.name) {
            fail(
                "manifest",
                format!("service {} is there twice", service.name),
            );
        }
        match service.exec.first() {
            None => fail("manifest", format!("service {} runs nothing", service.name)),
            Some(file) if !is_bundle_file(file) => fail(
                "manifest",
                format!(
                    "service {} runs {file:?}, which is not a file in files/",
                    service.name
                ),
            ),
            Some(_) => {}
        }
        if service.exec.len() > MAX_ARGS {
            fail(
                "too_many",
                format!(
                    "service {} has more than {MAX_ARGS} arguments",
                    service.name
                ),
            );
        }
        for arg in &service.exec {
            let plain = arg.len() <= MAX_ARG_LEN
                && !arg
                    .chars()
                    .any(|c| matches!(c, '"' | '\'' | '\\' | '`') || c.is_control());
            if !plain || expand(arg, &vars).is_none() {
                fail(
                    "manifest",
                    format!(
                        "service {} argument {arg:?} carries a quote, a backslash, a ` or a control character, or a $ that is not ${{VAR}} of APP_* or env",
                        service.name
                    ),
                );
            }
        }
    }
}

/// Every file `manifest` names that the bundle unpacked at `dir` lacks.
pub fn check_files(manifest: &Manifest, dir: &Path, init: Init) -> Vec<Problem> {
    use std::os::unix::fs::PermissionsExt;
    let mut problems = Vec::new();
    let units = manifest.units.iter().filter_map(|unit| match init {
        Init::Systemd => unit.systemd.clone(),
        Init::Openrc => unit.openrc.clone(),
    });
    for name in units {
        let file = dir.join("units").join(init.as_str()).join(&name);
        if !file.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
            problems.push(Problem::new(
                "manifest",
                format!("units/{}/{name} is not in the bundle", init.as_str()),
            ));
        }
    }
    if let Some(hook) = &manifest.first_run
        && !dir
            .join(hook)
            .symlink_metadata()
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o100 != 0)
    {
        problems.push(Problem::new(
            "manifest",
            format!("firstRun {hook:?} is not an executable file in the bundle"),
        ));
    }
    for service in &manifest.services {
        if let Some(file) = service.exec.first()
            && is_bundle_file(file)
            && !dir
                .join(file)
                .symlink_metadata()
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o100 != 0)
        {
            problems.push(Problem::new(
                "manifest",
                format!(
                    "service {} runs {file:?}, which is not an executable file in the bundle",
                    service.name
                ),
            ));
        }
    }
    if manifest.privileges.user == RunAs::App
        && manifest.units.is_empty()
        && dir.join("units").exists()
    {
        problems.push(Problem::new(
            "units_need_root",
            "units/ is for an app that runs as root".to_string(),
        ));
    }
    problems
}

/// The architecture this build runs as, in Debian's words.
pub fn device_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        "arm" => "armhf",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "name": "tailscale",
            "version": "1.78.1",
            "arch": ["arm64"],
            "units": [{ "systemd": "tailscaled.service", "openrc": "tailscaled", "enable": true }],
            "privileges": { "user": "root" },
            "env": { "TS_SOCKET": "/run/tailscale/tailscaled.sock" },
        }))
        .unwrap()
    }

    fn bundle_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("units/systemd")).unwrap();
        std::fs::write(
            dir.path().join("units/systemd/tailscaled.service"),
            "[Unit]\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_whole_manifest_breaks_no_rule() {
        let dir = bundle_dir();
        assert_eq!(check(&manifest(), dir.path(), Init::Systemd, "arm64"), []);
    }

    #[test]
    fn every_broken_rule_is_named() {
        let dir = bundle_dir();
        let mut bad = manifest();
        bad.name = "Tail_scale".into();
        bad.version = "".into();
        bad.env.insert("APP_DIR".into(), "x".into());
        bad.env.insert("TOKEN".into(), "a\"b".into());
        bad.update = Some(UpdateSpec {
            url: "http://example.org/v.json".into(),
            auto: false,
        });
        let rules: Vec<&str> = check(&bad, dir.path(), Init::Systemd, "amd64")
            .iter()
            .map(|p| p.rule)
            .collect();
        assert_eq!(
            rules,
            [
                "manifest", "manifest", "arch", "manifest", "manifest", "manifest"
            ]
        );
        // A unit for the other init alone.
        let rules: Vec<&str> = check(&manifest(), dir.path(), Init::Openrc, "arm64")
            .iter()
            .map(|p| p.rule)
            .collect();
        assert_eq!(rules, ["manifest"], "the openrc file is not in the bundle");
        let mut systemd_only = manifest();
        systemd_only.units[0].openrc = None;
        let rules: Vec<&str> = check(&systemd_only, dir.path(), Init::Openrc, "arm64")
            .iter()
            .map(|p| p.rule)
            .collect();
        assert_eq!(rules, ["init"]);
    }

    #[test]
    fn an_app_of_its_own_user_keeps_only_the_allowed_capabilities() {
        let mut app = own_user();
        app.privileges.capabilities = ALLOWED_CAPABILITIES.map(String::from).to_vec();
        assert_eq!(rules_of(&app), Vec::<&str>::new());
        for cap in CAPABILITIES
            .iter()
            .filter(|cap| !ALLOWED_CAPABILITIES.contains(cap))
        {
            app.privileges.capabilities = vec![cap.to_string()];
            assert_eq!(rules_of(&app), ["capability"], "{cap}");
        }
    }

    #[test]
    fn an_app_name_leaves_room_for_its_account() {
        assert!(is_app_name(&"a".repeat(23)));
        assert!(!is_app_name(&"a".repeat(24)));
        assert!(is_service_name(&"a".repeat(32)));
        assert!(!is_service_name(&"a".repeat(33)));
    }

    fn own_user() -> Manifest {
        serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "name": "tailscale",
            "version": "1.104.2",
            "arch": ["arm64"],
            "env": { "TS_SOCKET": "/srv/apps/tailscale/tailscaled.sock" },
            "privileges": { "capabilities": ["CAP_NET_ADMIN", "CAP_NET_RAW"], "devices": ["/dev/net/tun"] },
            "services": [{ "name": "tailscaled", "exec": ["files/tailscaled", "--socket=${TS_SOCKET}", "--state=${APP_DATA}/s"], "enable": true }],
        }))
        .unwrap()
    }

    fn rules_of(manifest: &Manifest) -> Vec<&'static str> {
        check_fields(manifest, Init::Systemd, "arm64")
            .iter()
            .map(|p| p.rule)
            .collect()
    }

    #[test]
    fn an_app_of_its_own_user_declares_services_and_privileges() {
        let good = own_user();
        assert_eq!(good.privileges.user, RunAs::App);
        assert_eq!(rules_of(&good), Vec::<&str>::new());
        assert_eq!(
            units_for(&good, Init::Openrc),
            [("tailscaled".to_string(), true)]
        );

        let mut bad = own_user();
        bad.units = manifest().units;
        bad.privileges.capabilities = vec!["CAP_SYS_ADMIN".into(), "CAP_FLY".into()];
        bad.privileges.devices = vec![
            "/etc/shadow".into(),
            "/dev/../etc".into(),
            "/dev/net/tun*".into(),
        ];
        bad.services[0].exec[1] = "--socket=${NOPE}".into();
        bad.services.push(Service {
            name: "tailscaled".into(),
            exec: vec!["/bin/sh".into(), "-c".into(), "a'b".into()],
            enable: false,
        });
        assert_eq!(
            rules_of(&bad),
            [
                "units_need_root",
                "capability",
                "capability",
                "device",
                "device",
                "device",
                "manifest",
                "manifest",
                "manifest",
                "manifest"
            ]
        );
    }

    #[test]
    fn a_root_app_ships_units_and_declares_nothing_else() {
        let mut root = own_user();
        root.privileges.user = RunAs::Root;
        assert_eq!(rules_of(&root), ["privileges", "services_need_app"]);
        assert_eq!(rules_of(&manifest()), Vec::<&str>::new());
    }

    #[test]
    fn variables_are_expanded_or_refused() {
        let vars = BTreeMap::from([("A".to_string(), "x".to_string())]);
        assert_eq!(expand("--a=${A}/${A}", &vars).as_deref(), Some("--a=x/x"));
        assert_eq!(expand("plain", &vars).as_deref(), Some("plain"));
        assert_eq!(expand("$A", &vars), None);
        assert_eq!(expand("${B}", &vars), None);
        assert_eq!(expand("${A", &vars), None);
    }

    #[test]
    fn an_unknown_key_or_policy_is_refused() {
        assert!(
            serde_json::from_str::<Manifest>(
                r#"{"schemaVersion":1,"name":"a","version":"1","arch":["all"],"root":true}"#
            )
            .is_err()
        );
        let once: Manifest = serde_json::from_str(
            r#"{"schemaVersion":1,"name":"a","version":"1","arch":["all"],"seedPolicy":"once"}"#,
        )
        .unwrap();
        assert_eq!(once.seed_policy, SeedPolicy::Once);
    }
}

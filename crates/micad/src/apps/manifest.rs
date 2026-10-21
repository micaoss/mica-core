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
/// The longest name.
const MAX_NAME_LEN: usize = 32;
/// The architectures a bundle may name, and `all`.
const ARCHES: [&str; 4] = ["amd64", "arm64", "armhf", "all"];

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
/// whether it is enabled.
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
        .collect()
}

/// Whether `name` can be an app's: 1 to 32 of `[a-z0-9-]`, not starting with
/// `-`.
pub fn is_app_name(name: &str) -> bool {
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
                "name {:?} is not 1 to {MAX_NAME_LEN} of a-z, 0-9 and -",
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
    let ours = units_for(manifest, init);
    if !manifest.units.is_empty() && ours.is_empty() {
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
    problems
}

/// Every file `manifest` names that the bundle unpacked at `dir` lacks.
pub fn check_files(manifest: &Manifest, dir: &Path, init: Init) -> Vec<Problem> {
    use std::os::unix::fs::PermissionsExt;
    let mut problems = Vec::new();
    for (name, _) in units_for(manifest, init) {
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

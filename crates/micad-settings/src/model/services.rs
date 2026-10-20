//! The user's own services: unit files and OpenRC scripts micad keeps on DATA
//! state through Base's `mica-unit`, and their rules.

use std::collections::BTreeMap;

use crate::features::Init;

/// The user's services, reconciled by `ServicesReconciler`.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServicesSettings {
    /// Each unit file (systemd) or script (OpenRC) by its name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub units: BTreeMap<String, UserUnit>,
}

/// One of the user's services.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserUnit {
    /// The init the text is for; micad applies only the product's own.
    pub kind: Init,
    /// The unit file, or the openrc-run script.
    pub text: String,
    /// The script's settings, its `conf.d` file (OpenRC only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conf: Option<String>,
    /// Whether it is linked to start at boot, and started when it becomes so.
    #[serde(default)]
    pub enabled: bool,
}

/// The most services one device declares.
pub const MAX_USER_UNITS: usize = 32;
/// The largest one unit's text may be.
pub const MAX_USER_UNIT_BYTES: usize = 64 * 1024;
/// The largest one script's conf may be.
pub const MAX_USER_UNIT_CONF_BYTES: usize = 16 * 1024;
/// The largest every text and conf may be together.
pub const MAX_USER_UNITS_BYTES: usize = 256 * 1024;
/// The longest a systemd unit name may be (systemd's own limit).
const MAX_SYSTEMD_NAME_LEN: usize = 255;
/// The longest an OpenRC script name may be.
const MAX_OPENRC_NAME_LEN: usize = 64;
/// The unit types `mica-unit` installs.
const SYSTEMD_SUFFIXES: [&str; 6] = [
    ".service", ".socket", ".timer", ".path", ".target", ".mount",
];
/// The first lines `mica-unit` takes as an openrc-run script.
const OPENRC_SHEBANGS: [&str; 3] = [
    "#!/sbin/openrc-run",
    "#!/usr/sbin/openrc-run",
    "#! /sbin/openrc-run",
];

/// Whether `name` is a unit file `mica-unit` installs: a name with one of its
/// suffixes, of systemd's characters, and a template (`name@.service`) at
/// most; an instance (`name@x.service`) is not a file.
#[must_use]
pub fn is_systemd_unit_name(name: &str) -> bool {
    let Some(stem) = SYSTEMD_SUFFIXES
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
    else {
        return false;
    };
    let prefix = stem.strip_suffix('@').unwrap_or(stem);
    name.len() <= MAX_SYSTEMD_NAME_LEN
        && !prefix.is_empty()
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'_' | b'.' | b'-' | b'\\'))
}

/// Whether `name` is a template unit, `name@.<suffix>`.
#[must_use]
pub fn is_systemd_template(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(stem, _)| stem.ends_with('@'))
}

/// Whether `name` is a script name `mica-unit` installs under OpenRC.
#[must_use]
pub fn is_openrc_script_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_OPENRC_NAME_LEN
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Every value of `key=` in the `[Install]` section of a unit's `text`, read
/// as `mica-unit` reads it.
#[must_use]
pub fn unit_install_values(text: &str, key: &str) -> Vec<String> {
    let mut section = String::new();
    let mut values = Vec::new();
    for line in text.lines() {
        if line.trim_start().starts_with('[') {
            section = line.chars().filter(|c| !c.is_whitespace()).collect();
            continue;
        }
        if section != "[Install]" {
            continue;
        }
        if let Some(value) = line
            .strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
        {
            values.extend(value.split_whitespace().map(str::to_string));
        }
    }
    values
}

/// Refuse a service `mica-unit` would refuse: a name of the wrong form for
/// its kind, an empty or oversized text, a conf on systemd, an enabled
/// template or one whose `[Install]` starts it from nothing, a script that is
/// not openrc-run's, or more or larger services than a device declares.
/// Whether the kind is the product's init, and whether the root ships the
/// name, are the device's to say.
///
/// # Errors
///
/// Returns the sentence the refusal carries.
pub fn validate_services(services: &ServicesSettings) -> Result<(), String> {
    if services.units.len() > MAX_USER_UNITS {
        return Err(format!(
            "{} services are declared; a device declares at most {MAX_USER_UNITS}",
            services.units.len()
        ));
    }
    let mut total = 0;
    for (name, unit) in &services.units {
        validate_user_unit(name, unit)?;
        total += unit.text.len() + unit.conf.as_ref().map_or(0, String::len);
    }
    if total > MAX_USER_UNITS_BYTES {
        return Err(format!(
            "the services are {total} bytes together; together they are at most 256 KiB"
        ));
    }
    Ok(())
}

fn validate_user_unit(name: &str, unit: &UserUnit) -> Result<(), String> {
    if unit.text.trim().is_empty() {
        return Err(format!("service {name:?} is empty"));
    }
    if unit.text.len() > MAX_USER_UNIT_BYTES {
        return Err(format!(
            "service {name:?} is {} bytes; a service is at most 64 KiB",
            unit.text.len()
        ));
    }
    match unit.kind {
        Init::Systemd => {
            if !is_systemd_unit_name(name) {
                return Err(format!(
                    "{name:?} is not a unit file name: a name of a-z, A-Z, 0-9, `:`, `_`, `.`, `-` \
                     ending in .service, .socket, .timer, .path, .target or .mount, a template \
                     (name@.service) at most"
                ));
            }
            if unit.conf.is_some() {
                return Err(format!(
                    "service {name:?} is a systemd unit; only an OpenRC script has a conf"
                ));
            }
            if unit.enabled && is_systemd_template(name) {
                return Err(format!(
                    "{name:?} is a template; a template is not enabled itself"
                ));
            }
            if unit.enabled
                && ["WantedBy", "RequiredBy", "Alias"]
                    .iter()
                    .all(|key| unit_install_values(&unit.text, key).is_empty())
            {
                return Err(format!(
                    "{name:?} has no WantedBy=, RequiredBy= or Alias= in [Install]; nothing would start it"
                ));
            }
        }
        Init::Openrc => {
            if !is_openrc_script_name(name) {
                return Err(format!(
                    "{name:?} is not a script name: 1 to {MAX_OPENRC_NAME_LEN} of a-z, A-Z, 0-9, \
                     `.`, `_` and `-`, not starting with `.`"
                ));
            }
            let first = unit.text.lines().next().unwrap_or_default();
            if !OPENRC_SHEBANGS
                .iter()
                .any(|shebang| first.starts_with(shebang))
            {
                return Err(format!(
                    "service {name:?} is not an openrc-run script: its first line is not #!/sbin/openrc-run"
                ));
            }
            if let Some(conf) = &unit.conf
                && conf.len() > MAX_USER_UNIT_CONF_BYTES
            {
                return Err(format!(
                    "the conf of service {name:?} is {} bytes; a conf is at most 16 KiB",
                    conf.len()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(kind: Init, text: &str, enabled: bool) -> UserUnit {
        UserUnit {
            kind,
            text: text.to_string(),
            conf: None,
            enabled,
        }
    }

    const SERVICE: &str = "[Unit]\nDescription=x\n[Service]\nExecStart=/srv/x\n\n[Install]\nWantedBy=multi-user.target\n";
    const SCRIPT: &str = "#!/sbin/openrc-run\ncommand=/srv/x\n";

    fn check(name: &str, unit: UserUnit) -> Result<(), String> {
        validate_services(&ServicesSettings {
            units: BTreeMap::from([(name.to_string(), unit)]),
        })
    }

    #[test]
    fn names_follow_each_init() {
        for name in [
            "x.service",
            "a:b_c-d.e.timer",
            "x@.service",
            "x.socket",
            "x.mount",
        ] {
            assert!(is_systemd_unit_name(name), "{name}");
        }
        for name in [
            "x",
            "x.conf",
            "x@y.service",
            ".service",
            "@.service",
            "a/b.service",
            "x y.service",
        ] {
            assert!(!is_systemd_unit_name(name), "{name}");
        }
        for name in ["tailscaled", "x.y", "a_b-c"] {
            assert!(is_openrc_script_name(name), "{name}");
        }
        for name in ["", ".x", "a/b", "a b", &"x".repeat(65)] {
            assert!(!is_openrc_script_name(name), "{name}");
        }
    }

    #[test]
    fn install_values_are_read_from_the_install_section_alone() {
        let text = "[Service]\nWantedBy=no.target\n[ Install ]\nWantedBy=a.target  b.target\nAlias=y.service\n WantedBy=indented.target\n";
        assert_eq!(
            unit_install_values(text, "WantedBy"),
            ["a.target", "b.target"]
        );
        assert_eq!(unit_install_values(text, "Alias"), ["y.service"]);
        assert!(unit_install_values(text, "RequiredBy").is_empty());
    }

    #[test]
    fn a_service_mica_unit_takes_is_taken() {
        check("x.service", unit(Init::Systemd, SERVICE, true)).unwrap();
        check("x@.service", unit(Init::Systemd, SERVICE, false)).unwrap();
        check(
            "x.service",
            unit(Init::Systemd, "[Service]\nExecStart=/x\n", false),
        )
        .unwrap();
        let mut script = unit(Init::Openrc, SCRIPT, true);
        script.conf = Some("X=1\n".to_string());
        check("x", script).unwrap();
        check("x", unit(Init::Openrc, "#!/usr/sbin/openrc-run\n", false)).unwrap();
    }

    #[test]
    fn a_service_mica_unit_refuses_is_refused() {
        let refused = |name: &str, unit: UserUnit, says: &str| {
            let err = check(name, unit).unwrap_err();
            assert!(err.contains(says), "{name}: {err}");
        };
        refused(
            "x",
            unit(Init::Systemd, SERVICE, false),
            "not a unit file name",
        );
        refused("x.service", unit(Init::Systemd, " \n", false), "is empty");
        refused(
            "x.service",
            unit(Init::Systemd, "[Service]\nExecStart=/x\n", true),
            "nothing would start it",
        );
        refused(
            "x@.service",
            unit(Init::Systemd, SERVICE, true),
            "a template",
        );
        let mut with_conf = unit(Init::Systemd, SERVICE, false);
        with_conf.conf = Some(String::new());
        refused("x.service", with_conf, "only an OpenRC script");
        refused(
            "a b",
            unit(Init::Openrc, SCRIPT, false),
            "not a script name",
        );
        refused(
            "x",
            unit(Init::Openrc, "#!/bin/sh\n", false),
            "not an openrc-run script",
        );
        let mut big_conf = unit(Init::Openrc, SCRIPT, false);
        big_conf.conf = Some("x".repeat(MAX_USER_UNIT_CONF_BYTES + 1));
        refused("x", big_conf, "a conf is at most 16 KiB");
        let big = format!("{SCRIPT}{}", "#".repeat(MAX_USER_UNIT_BYTES));
        refused("x", unit(Init::Openrc, &big, false), "at most 64 KiB");
    }

    #[test]
    fn a_device_declares_a_bounded_set() {
        let many = ServicesSettings {
            units: (0..=MAX_USER_UNITS)
                .map(|n| (format!("s{n}"), unit(Init::Openrc, SCRIPT, false)))
                .collect(),
        };
        assert!(validate_services(&many).unwrap_err().contains("at most 32"));
        let text = format!("{SCRIPT}{}", "#".repeat(MAX_USER_UNIT_BYTES - SCRIPT.len()));
        let large = ServicesSettings {
            units: (0..5)
                .map(|n| (format!("s{n}"), unit(Init::Openrc, &text, false)))
                .collect(),
        };
        assert!(validate_services(&large).unwrap_err().contains("together"));
    }

    #[test]
    fn the_kind_is_spelled_as_init_is() {
        let unit: UserUnit = serde_json::from_value(serde_json::json!({
            "kind": "openrc", "text": SCRIPT, "enabled": true
        }))
        .unwrap();
        assert_eq!(unit.kind, Init::Openrc);
        assert!(
            serde_json::from_value::<UserUnit>(serde_json::json!({"kind": "runit", "text": "x"}))
                .is_err()
        );
    }
}

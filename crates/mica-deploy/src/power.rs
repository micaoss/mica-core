//! Rebooting and powering off through the root's init, as micad's power
//! control does: `systemctl reboot|poweroff` under systemd,
//! `openrc-shutdown --reboot|--poweroff now` under OpenRC, which has no
//! `reboot` or `poweroff` on the path. The init is the product file's
//! `INIT=` line, systemd when the line or the file is absent.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// What to do with the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Power {
    Reboot,
    PowerOff,
}

/// The init a product runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Init {
    Systemd,
    Openrc,
}

/// The init the product file `text` states; systemd when it states none.
///
/// # Errors
///
/// An `INIT=` line naming another init.
pub fn parse_init(text: &str) -> Result<Init> {
    let Some(value) = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("INIT="))
        .map(str::trim)
    else {
        return Ok(Init::Systemd);
    };
    match value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .unwrap_or(value)
    {
        "systemd" => Ok(Init::Systemd),
        "openrc" => Ok(Init::Openrc),
        other => bail!("INIT={other} names no init mica-deploy drives"),
    }
}

/// The init of the root at `root`.
///
/// # Errors
///
/// A product file that cannot be read or names an unknown init.
pub fn init(root: &Path) -> Result<Init> {
    let path = root.join(crate::components::PRODUCT_FILE.trim_start_matches('/'));
    match std::fs::read_to_string(&path) {
        Ok(text) => parse_init(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Init::Systemd),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// The program, under `root`, and the arguments that do `power` on `init`.
#[must_use]
pub fn command(root: &Path, init: Init, power: Power) -> (PathBuf, &'static [&'static str]) {
    match (init, power) {
        (Init::Systemd, Power::Reboot) => (root.join("usr/bin/systemctl"), &["reboot"]),
        (Init::Systemd, Power::PowerOff) => (root.join("usr/bin/systemctl"), &["poweroff"]),
        (Init::Openrc, Power::Reboot) => {
            (root.join("usr/sbin/openrc-shutdown"), &["--reboot", "now"])
        }
        (Init::Openrc, Power::PowerOff) => (
            root.join("usr/sbin/openrc-shutdown"),
            &["--poweroff", "now"],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A root with the product file `product` whose power programs record
    /// their arguments.
    fn fake_root(product: Option<&str>) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for program in ["usr/bin/systemctl", "usr/sbin/openrc-shutdown"] {
            let path = root.path().join(program);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let log = root.path().join("calls");
            std::fs::write(
                &path,
                format!("#!/bin/sh\necho \"${{0##*/}} $*\" >>{}\n", log.display()),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        if let Some(text) = product {
            let path = root.path().join("usr/lib/mica/product.conf");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    fn run(root: &Path, power: Power) -> String {
        let (program, args) = command(root, init(root).unwrap(), power);
        assert!(
            std::process::Command::new(program)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
        std::fs::read_to_string(root.join("calls")).unwrap()
    }

    #[test]
    fn systemd_roots_go_through_systemctl() {
        let root = fake_root(Some("FEATURES=\"\"\nINIT=systemd\n"));
        assert_eq!(run(root.path(), Power::Reboot), "systemctl reboot\n");
        let root = fake_root(Some("FEATURES=\"\"\nINIT=systemd\n"));
        assert_eq!(run(root.path(), Power::PowerOff), "systemctl poweroff\n");
    }

    #[test]
    fn a_root_without_an_init_line_or_product_file_is_systemd() {
        let root = fake_root(Some("FEATURES=\"\"\n"));
        assert_eq!(run(root.path(), Power::Reboot), "systemctl reboot\n");
        let root = fake_root(None);
        assert_eq!(run(root.path(), Power::Reboot), "systemctl reboot\n");
    }

    #[test]
    fn openrc_roots_go_through_openrc_shutdown() {
        let root = fake_root(Some("FEATURES=\"\"\nINIT=\"openrc\"\n"));
        assert_eq!(
            run(root.path(), Power::Reboot),
            "openrc-shutdown --reboot now\n"
        );
        let root = fake_root(Some("FEATURES=\"\"\nINIT=openrc\n"));
        assert_eq!(
            run(root.path(), Power::PowerOff),
            "openrc-shutdown --poweroff now\n"
        );
    }

    #[test]
    fn an_unknown_init_is_an_error() {
        let root = fake_root(Some("INIT=runit\n"));
        assert!(init(root.path()).is_err());
    }
}

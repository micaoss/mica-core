use super::super::systemd::mock::MockUnitControl;
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

mod accounts;
mod keys;
mod passwords;
mod render;
mod units;
use keys::*;

const GOLDEN_DEFAULTS: &str = "DROPBEAR_ARGS=\"-p 22\"\n";
/// What `apply` writes for default settings with **no** transient password
/// active: the same arguments with password logins turned off.
const GOLDEN_DEFAULTS_GATED: &str = "DROPBEAR_ARGS=\"-p 22 -s\"\n";
const GOLDEN_LISTEN: &str = "DROPBEAR_ARGS=\"-p 10.0.0.5:2222 -p [fd00::1]:2222 -s -w\"\n";

/// Three accounts, nine fields each, trailing newline — the shape of a
/// Debian `/etc/shadow`. `root` starts out locked (`!`).
const SHADOW: &str = "root:!:19000:0:99999:7:::\n\
    daemon:*:19000:0:99999:7:::\n\
    operator:$6$rounds=5000$abcd$efgh:19100:0:99999:7:::\n";
const SHADOW_MODE: u32 = 0o640;

fn ssh_settings(enabled: bool) -> SshSettings {
    SshSettings {
        enabled,
        ..SshSettings::default()
    }
}

fn settings_with(ssh: SshSettings) -> Settings {
    Settings {
        access: micad_settings::AccessSettings {
            ssh,
            ..micad_settings::AccessSettings::default()
        },
        ..Settings::default()
    }
}

/// Paths of a fixture, all of them under the tempdir.
struct Paths {
    environment: PathBuf,
    passwd: PathBuf,
    root_home: PathBuf,
    mica_home: PathBuf,
    /// The `root` account's key file — the path the goldens below are
    /// written against.
    keys: PathBuf,
    /// The `mica` account's key file, holding the same bytes as `keys`.
    mica_keys: PathBuf,
    shadow: PathBuf,
}

/// The uid and gid this test runs as. Both managed accounts are given
/// them, so the ownership the reconciler sets is one any test runner may
/// set, root or not.
fn own_ids() -> (u32, u32) {
    (
        rustix::process::geteuid().as_raw(),
        rustix::process::getegid().as_raw(),
    )
}

/// An `/etc/passwd` naming `root` and `mica` at the given homes, with a
/// system account between them.
fn passwd_for(root_home: &Path, mica_home: &Path) -> String {
    let (uid, gid) = own_ids();
    format!(
        "root:x:{uid}:{gid}:root:{}:/bin/bash\n\
         daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin\n\
         mica:x:{uid}:{gid}:mica operator:{}:/bin/bash\n",
        root_home.display(),
        mica_home.display()
    )
}

/// Fixture rooted entirely inside `dir`: an environment file that does
/// not exist yet, two 0755 homes with no `.ssh`, and a shadow file at
/// [`SHADOW_MODE`].
fn fixture(dir: &Path, active: &str, file_state: &str) -> (SshdReconciler<MockUnitControl>, Paths) {
    let root_home = dir.join("root");
    let mica_home = dir.join("home").join("mica");
    let paths = Paths {
        environment: dir.join("run").join("mica").join("dropbear.env"),
        passwd: dir.join("passwd"),
        keys: root_home.join(".ssh").join("authorized_keys"),
        mica_keys: mica_home.join(".ssh").join("authorized_keys"),
        root_home,
        mica_home,
        shadow: dir.join("shadow"),
    };
    for home in [&paths.root_home, &paths.mica_home] {
        std::fs::create_dir_all(home).unwrap();
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::write(
        &paths.passwd,
        passwd_for(&paths.root_home, &paths.mica_home),
    )
    .unwrap();
    std::fs::write(&paths.shadow, SHADOW).unwrap();
    std::fs::set_permissions(&paths.shadow, std::fs::Permissions::from_mode(SHADOW_MODE)).unwrap();
    let reconciler = SshdReconciler::new(
        paths.environment.clone(),
        paths.passwd.clone(),
        paths.shadow.clone(),
        MockUnitControl::new(active, file_state),
    );
    (reconciler, paths)
}

/// Write `contents` as the environment file, as a previous apply would
/// have.
fn preexisting_environment(paths: &Paths, contents: &str) {
    std::fs::create_dir_all(paths.environment.parent().unwrap()).unwrap();
    std::fs::write(&paths.environment, contents).unwrap();
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
}

fn calls(reconciler: &SshdReconciler<MockUnitControl>) -> Vec<String> {
    reconciler.control.calls()
}

// ---- rendering --------------------------------------------------------

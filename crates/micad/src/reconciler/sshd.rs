//! SSH access reconciler: renders the SSH server's environment and the
//! managed accounts' authorized keys from `access.ssh` and drives
//! `mica-sshd.service`. Three system effects, in this order.
//!
//! The server is Base's, dropbear or OpenSSH as the product chooses; both read
//! the same environment file, and nothing here depends on which one runs.
//! Either authenticates against `/etc/shadow` through `crypt(3)` and not
//! through PAM (`crate::transient`); the key files this module writes and the
//! transient password are the two ways into a device.

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use micad_settings::{AuthorizedKey, Settings, SshSettings};
use rustix::fs::{AtFlags, Mode, OFlags};
use serde_json::json;

use super::Reconciler;
use super::systemd::{UnitControl, is_active, is_enabled};
use crate::transient;

/// Unit implementing the SSH server, shipped by Base on both inits.
const SSH_UNIT: &str = "mica-sshd.service";
/// `ActiveState` of a unit that exited unsuccessfully, possibly inside its
/// start-limit window.
const FAILED_STATE: &str = "failed";
/// Environment file carrying the SSH server's arguments.
const DEFAULT_ENVIRONMENT_FILE: &str = "/run/mica/sshd.env";
/// Account database the managed accounts' ids and homes are read from.
const DEFAULT_PASSWD: &str = "/etc/passwd";
/// Accounts this reconciler renders authorized keys for, in render order.
///
/// **One key set, rendered for every managed login account.** Keys are not
/// per-user in the settings tree, so every entry of `access.ssh.authorizedKeys`
/// is a root key as much as it is a `mica` key, and the web UI says so.
const MANAGED_LOGIN_ACCOUNTS: [&str; 2] = ["root", "mica"];
/// Environment variable overriding the environment file path.
const ENVIRONMENT_FILE_ENV: &str = "MICAD_SSHD_ENV";
/// Environment variable overriding the account database path. Nothing in the
/// image sets it; the override exists for tests.
const PASSWD_ENV: &str = "MICAD_PASSWD_PATH";
/// Mode of the environment file: world-readable arguments, owner-writable.
const ENVIRONMENT_FILE_MODE: u32 = 0o644;
/// Mode of `~/.ssh`.
const SSH_DIR_MODE: u32 = 0o700;
/// Mode of `~/.ssh/authorized_keys`: owner-only. Nothing else has any business
/// enumerating which keys open the device.
const AUTHORIZED_KEYS_MODE: u32 = 0o600;
/// Name of the key file inside `~/.ssh`.
const AUTHORIZED_KEYS: &str = "authorized_keys";
/// Temporary sibling a key file is written to before the rename.
const AUTHORIZED_KEYS_TEMP: &str = ".authorized_keys.micad-tmp";
/// Most listeners `SSH_LISTEN` carries: dropbear binds ten
/// (`DROPBEAR_MAX_PORTS`) and ignores the rest without a word.
const MAX_LISTEN_ADDRESSES: usize = 10;

/// Reconciler for the `access.ssh` settings subtree.
pub struct SshdReconciler<C: UnitControl> {
    /// `mica-sshd.service`, or on an OpenRC root `mica-sshd`.
    unit: &'static str,
    environment_path: PathBuf,
    /// Account database the managed accounts' uid, gid and home come from.
    passwd_path: PathBuf,
    /// Shadow file this reconciler's device operates on.
    ///
    /// Nothing here writes it. It is read — through
    /// [`transient::transient_password_active`], which looks for the marker
    /// beside it — because whether password authentication may be offered at
    /// all is a question about this exact path.
    shadow_path: PathBuf,
    control: C,
}

impl<C: UnitControl> SshdReconciler<C> {
    /// Create a reconciler writing the server's environment to
    /// `environment_path` and each managed account's key file under the home
    /// `passwd_path` names, tracking the shadow file at `shadow_path`, and
    /// driving `mica-sshd.service` through `control`.
    pub fn new(
        environment_path: PathBuf,
        passwd_path: PathBuf,
        shadow_path: PathBuf,
        control: C,
    ) -> Self {
        Self {
            unit: SSH_UNIT,
            environment_path,
            passwd_path,
            shadow_path,
            control,
        }
    }
}

impl<C: UnitControl> SshdReconciler<C> {
    /// Production reconciler driving the service through `control`: paths
    /// from [`ENVIRONMENT_FILE_ENV`], [`PASSWD_ENV`] and
    /// [`transient::SHADOW_ENV`] if set, else the system locations.
    pub fn production(control: C) -> Self {
        let from_env = |name: &str, default: &str| {
            std::env::var(name)
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from(default))
        };
        Self::new(
            from_env(ENVIRONMENT_FILE_ENV, DEFAULT_ENVIRONMENT_FILE),
            from_env(PASSWD_ENV, DEFAULT_PASSWD),
            transient::production_shadow_path(),
            control,
        )
    }

    /// The production reconciler on an OpenRC root, driving `mica-sshd`.
    pub fn openrc(control: C) -> Self {
        Self {
            unit: "mica-sshd.service",
            ..Self::production(control)
        }
    }
}

/// Refuse a listen address that could not be one, and more of them than
/// the server binds.
fn validate_listen_addresses(addresses: &[String]) -> Result<()> {
    if addresses.len() > MAX_LISTEN_ADDRESSES {
        bail!(
            "access.ssh.listenAddresses holds {} entries; the SSH server binds at \
             most {MAX_LISTEN_ADDRESSES} and would silently ignore the rest",
            addresses.len()
        );
    }
    for address in addresses {
        let ok = address.parse::<std::net::IpAddr>().is_ok()
            || address.parse::<std::net::SocketAddr>().is_ok();
        if !ok {
            return Err(anyhow!(
                "access.ssh.listenAddresses entry {address:?} is not an IP address"
            ));
        }
    }
    Ok(())
}

/// One listener for `address`: `addr:port`, IPv6 in brackets, and an
/// address that carries its own port keeps it. Called only on a validated
/// address.
fn listener(address: &str, port: u16) -> String {
    match address.parse::<std::net::IpAddr>() {
        Ok(ip) => std::net::SocketAddr::new(ip, port).to_string(),
        Err(_) => address.to_string(),
    }
}

/// Render `/run/mica/sshd.env` for `ssh`.
///
/// Pure and deterministic: the same settings always produce the same bytes, so
/// a re-render can be compared against what is on disk to decide whether
/// anything changed. Three shell-sourceable lines, the contract with Base's
/// server packages:
///
/// - `SSH_LISTEN="..."`: the bare port, listening on every address, or one
///   `addr:port` per listen address, space-separated, IPv6 in brackets;
/// - `SSH_PASSWORD=yes|no`: `yes` only while password logins are on;
/// - `SSH_ROOT_LOGIN=yes|no`: `no` refuses root by every method.
fn render_environment(ssh: &SshSettings, password_authentication: bool) -> String {
    let listen = if ssh.listen_addresses.is_empty() {
        ssh.port.to_string()
    } else {
        ssh.listen_addresses
            .iter()
            .map(|address| listener(address, ssh.port))
            .collect::<Vec<_>>()
            .join(" ")
    };
    let yes_no = |on: bool| if on { "yes" } else { "no" };
    format!(
        "SSH_LISTEN=\"{listen}\"\nSSH_PASSWORD={}\nSSH_ROOT_LOGIN={}\n",
        yes_no(password_authentication),
        yes_no(ssh.permit_root_login)
    )
}

/// Render the authorized-keys file for `keys`.
///
/// One entry per line in settings order — the operator's order, which is stable
/// across a load/store round-trip, so the render is deterministic and can be
/// compared against what is on disk. Each line is `<key>` or `<key> <comment>`.
fn render_authorized_keys(keys: &[AuthorizedKey]) -> String {
    let mut out = String::new();
    for entry in keys {
        match &entry.comment {
            Some(comment) => out.push_str(&format!("{} {}\n", entry.key, comment)),
            None => out.push_str(&format!("{}\n", entry.key)),
        }
    }
    out
}

/// A managed account as the account database describes it.
#[derive(Debug, Clone, PartialEq)]
struct Account {
    name: String,
    uid: u32,
    gid: u32,
    home: PathBuf,
}

/// The `/etc/passwd` entry named `name`, if there is a well-formed one.
fn find_account(passwd: &str, name: &str) -> Option<Account> {
    passwd.lines().find_map(|line| {
        let fields: Vec<&str> = line.split(':').collect();
        if fields.len() != 7 || fields[0] != name {
            return None;
        }
        Some(Account {
            name: name.to_string(),
            uid: fields[2].parse().ok()?,
            gid: fields[3].parse().ok()?,
            home: PathBuf::from(fields[5]),
        })
    })
}

/// An account whose `~/.ssh` is open and ready for its key file.
struct KeyTarget {
    account: Account,
    ssh_dir: OwnedFd,
}

impl KeyTarget {
    fn path(&self) -> PathBuf {
        self.account.home.join(".ssh").join(AUTHORIZED_KEYS)
    }
}

fn errno(err: rustix::io::Errno) -> std::io::Error {
    std::io::Error::from(err)
}

/// Open `account`'s home and `~/.ssh` for a key file the server will honour.
fn open_key_target(account: Account) -> Result<Option<KeyTarget>> {
    let home = match rustix::fs::open(
        &account.home,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(home) => home,
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(err) => {
            return Err(errno(err)).with_context(|| format!("open {}", account.home.display()));
        }
    };
    let stat = rustix::fs::fstat(&home)
        .map_err(errno)
        .with_context(|| format!("stat {}", account.home.display()))?;
    if (stat.st_uid != account.uid && stat.st_uid != 0) || stat.st_mode & 0o022 != 0 {
        bail!(
            "{} (home of {}) must be owned by the account or root and not be group- or \
             world-writable, or the SSH server refuses every key under it",
            account.home.display(),
            account.name
        );
    }

    let ssh_path = account.home.join(".ssh");
    match rustix::fs::mkdirat(&home, ".ssh", Mode::from_raw_mode(SSH_DIR_MODE)) {
        Ok(()) | Err(rustix::io::Errno::EXIST) => {}
        Err(err) => {
            return Err(errno(err)).with_context(|| format!("create {}", ssh_path.display()));
        }
    }
    let ssh_dir = rustix::fs::openat(
        &home,
        ".ssh",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(errno)
    .with_context(|| format!("open {} (a symbolic link is refused)", ssh_path.display()))?;
    let stat = rustix::fs::fstat(&ssh_dir)
        .map_err(errno)
        .with_context(|| format!("stat {}", ssh_path.display()))?;
    if stat.st_uid != account.uid || stat.st_gid != account.gid {
        rustix::fs::fchown(
            &ssh_dir,
            Some(rustix::fs::Uid::from_raw(account.uid)),
            Some(rustix::fs::Gid::from_raw(account.gid)),
        )
        .map_err(errno)
        .with_context(|| format!("set owner on {}", ssh_path.display()))?;
    }
    if stat.st_mode & 0o7777 != SSH_DIR_MODE {
        rustix::fs::fchmod(&ssh_dir, Mode::from_raw_mode(SSH_DIR_MODE))
            .map_err(errno)
            .with_context(|| format!("set mode on {}", ssh_path.display()))?;
    }
    Ok(Some(KeyTarget { account, ssh_dir }))
}

/// Write `rendered` as `target`'s key file unless it already holds exactly
/// that.
fn write_key_file(target: &KeyTarget, rendered: &str) -> Result<()> {
    let path = target.path();
    // Non-blocking, and only a regular file of the rendered length is read:
    // the name is in a directory the account controls, and a FIFO there would
    // otherwise hold the open until somebody wrote to it.
    if let Ok(existing) = rustix::fs::openat(
        &target.ssh_dir,
        AUTHORIZED_KEYS,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    ) && rustix::fs::fstat(&existing).is_ok_and(|stat| {
        rustix::fs::FileType::from_raw_mode(stat.st_mode) == rustix::fs::FileType::RegularFile
            && u64::try_from(stat.st_size).ok() == Some(rendered.len() as u64)
    }) {
        let mut current = String::new();
        if std::fs::File::from(existing)
            .read_to_string(&mut current)
            .is_ok()
            && current == rendered
        {
            return Ok(());
        }
    }

    let temp_path = path.with_file_name(AUTHORIZED_KEYS_TEMP);
    // A leftover from an interrupted run, or anything planted under the name.
    let _ = rustix::fs::unlinkat(&target.ssh_dir, AUTHORIZED_KEYS_TEMP, AtFlags::empty());
    let temp = rustix::fs::openat(
        &target.ssh_dir,
        AUTHORIZED_KEYS_TEMP,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(AUTHORIZED_KEYS_MODE),
    )
    .map_err(errno)
    .with_context(|| format!("create {}", temp_path.display()))?;
    // Mode and owner explicitly and on the descriptor: the umask applied to
    // the create, and the name is not trusted between two calls.
    rustix::fs::fchmod(&temp, Mode::from_raw_mode(AUTHORIZED_KEYS_MODE))
        .map_err(errno)
        .with_context(|| format!("set mode on {}", temp_path.display()))?;
    rustix::fs::fchown(
        &temp,
        Some(rustix::fs::Uid::from_raw(target.account.uid)),
        Some(rustix::fs::Gid::from_raw(target.account.gid)),
    )
    .map_err(errno)
    .with_context(|| format!("set owner on {}", temp_path.display()))?;
    let mut file = std::fs::File::from(temp);
    file.write_all(rendered.as_bytes())
        .with_context(|| format!("write {}", temp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("flush {}", temp_path.display()))?;
    drop(file);
    rustix::fs::renameat(
        &target.ssh_dir,
        AUTHORIZED_KEYS_TEMP,
        &target.ssh_dir,
        AUTHORIZED_KEYS,
    )
    .map_err(errno)
    .with_context(|| format!("rename {} to {}", temp_path.display(), path.display()))?;
    Ok(())
}

impl<C: UnitControl> SshdReconciler<C> {
    /// Every managed account the key list can be rendered for, opened and
    /// checked before any key file is written.
    fn key_targets(&self, accounts: &[&str]) -> Result<Vec<KeyTarget>> {
        let passwd = std::fs::read_to_string(&self.passwd_path)
            .with_context(|| format!("read {}", self.passwd_path.display()))?;
        let mut targets = Vec::new();
        for name in accounts {
            let Some(account) = find_account(&passwd, name) else {
                tracing::warn!(
                    account = name,
                    passwd = %self.passwd_path.display(),
                    "managed SSH account is not in the account database; no keys rendered for it"
                );
                continue;
            };
            let home = account.home.clone();
            if let Some(target) = open_key_target(account)? {
                targets.push(target)
            } else {
                tracing::warn!(
                    account = name,
                    home = %home.display(),
                    "managed SSH account has no home directory; no keys rendered for it"
                )
            }
        }
        Ok(targets)
    }

    /// Bring `mica-sshd.service` to the state `ssh.enabled` asks for. Reads
    /// before it writes, so a system already in the target state gets no calls
    /// at all.
    async fn apply_unit(&self, ssh: &SshSettings, config_changed: bool) -> Result<()> {
        if ssh.enabled {
            if !is_enabled(&self.control.unit_file_state(self.unit).await?) {
                self.control.enable(self.unit).await?;
            }
            let active_state = self.control.active_state(self.unit).await?;
            if is_active(&active_state) {
                if config_changed {
                    self.control.restart(self.unit).await.with_context(|| {
                        format!(
                            "restart {} after its arguments changed: the running server \
                             still has the previous arguments",
                            self.unit
                        )
                    })?;
                }
            } else {
                // The mqtt.rs start-limit amendment: mica-sshd.service restarts
                // on failure, so a server that could not bind can be `failed`
                // inside its start-limit window, where systemd refuses start
                // jobs. Clear the failure first -- only when there is one, so
                // the call log still says which apply rescued it.
                if active_state == FAILED_STATE {
                    self.control.reset_failed(self.unit).await?;
                }
                self.control.start(self.unit).await?;
            }
        } else {
            if is_active(&self.control.active_state(self.unit).await?) {
                self.control.stop(self.unit).await?;
            }
            if is_enabled(&self.control.unit_file_state(self.unit).await?) {
                self.control.disable(self.unit).await?;
            }
        }
        Ok(())
    }
}

#[async_trait::async_trait]
impl<C: UnitControl> Reconciler for SshdReconciler<C> {
    fn name(&self) -> &'static str {
        // The live-state key and the apply queue's name for this reconciler,
        // which apid and the API harness read; it names the SSH channel, not
        // the daemon behind it.
        "sshd"
    }

    fn subtree(&self) -> &'static str {
        "access.ssh"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let ssh = &settings.access.ssh;

        // Before anything is written. The settings file lives on STATE and is
        // editable by anything that can write STATE, so the parser is the
        // security boundary and this is the second place it has to hold. A
        // failure here aborts the whole apply with every rendered file exactly
        // as it was: silently dropping the offending entry and rendering the
        // rest would leave the operator looking at a key in the UI that grants
        // nothing.
        micad_settings::validate_authorized_keys(&ssh.authorized_keys)?;
        // Same boundary, same reasoning: each listen address is interpolated
        // verbatim into the quoted `SSH_LISTEN` value, where whitespace
        // adds a listener and a quote or newline ends the value. Requiring
        // an actual address makes injection structurally impossible rather
        // than filtering for it.
        validate_listen_addresses(&ssh.listen_addresses)?;

        // Outside the settings tree, so it has to be read on every apply: the
        // operator setting a transient password changes no setting at all, and
        // the bus method that sets one calls back through `apply_all`.
        let transient_active = transient::transient_password_active(&self.shadow_path);
        // `password_authentication` below is the EFFECTIVE value — what the server
        // is actually told. `passwordAuthenticationRequested` in the published
        // state is the raw setting. Two similarly-named keys, so: effective =
        // requested AND a transient password is really active.
        let password_authentication = ssh.password_authentication && transient_active;

        // Every home is checked before any key file is written. No "did it
        // change" comes back from the keys, and none is wanted: the server opens
        // the key file on every authentication attempt, so a key added or
        // removed takes effect without touching the unit at all.
        let targets = self.key_targets(&MANAGED_LOGIN_ACCOUNTS)?;
        let rendered = render_authorized_keys(&ssh.authorized_keys);
        for target in &targets {
            write_key_file(target, &rendered)?;
        }
        // Unchanged bytes are not rewritten, so a reconcile that changes nothing
        // does not look like a change and restart the server.
        let config_changed = crate::fswrite::write_config_if_changed(
            &self.environment_path,
            &render_environment(ssh, password_authentication),
            ENVIRONMENT_FILE_MODE,
        )?;
        self.apply_unit(ssh, config_changed).await?;

        let authorized_keys: Vec<serde_json::Value> = ssh
            .authorized_keys
            .iter()
            .map(|entry| {
                json!({
                    // Never the key material itself: this tree is served over
                    // D-Bus and read by apid, and a fingerprint is what an
                    // operator needs in order to recognise a key.
                    "fingerprint": micad_settings::ssh_fingerprint(&entry.key),
                    "comment": entry.comment,
                })
            })
            .collect();
        let authorized_keys_paths: Vec<String> = targets
            .iter()
            .map(|target| target.path().display().to_string())
            .collect();

        Ok(json!({
            "enabled": ssh.enabled,
            "port": ssh.port,
            "permitRootLogin": ssh.permit_root_login,
            "passwordAuthentication": password_authentication,
            "passwordAuthenticationRequested": ssh.password_authentication,
            "transientPasswordActive": transient_active,
            "listenAddresses": ssh.listen_addresses,
            "environmentFile": self.environment_path.display().to_string(),
            // Plural: one key set is rendered to one file per managed account
            // that exists, in managed-account order.
            "authorizedKeysPaths": authorized_keys_paths,
            "authorizedKeys": authorized_keys,
            "unit": self.unit,
            "activeState": self.control.active_state(self.unit).await?,
            "unitFileState": self.control.unit_file_state(self.unit).await?,
        }))
    }
}

#[cfg(test)]
mod tests;

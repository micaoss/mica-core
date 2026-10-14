//! On-device identity and per-device credential generation.

use std::fs::{self, DirBuilder, Permissions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use micad_settings::Settings;

/// Default STATE-backed directory holding the settings file and the secrets.
///
/// `/var/lib/mica` is a bind mount whose source is `/mnt/data/state/mica`.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/mica";

/// Sub-directory of the state directory holding plaintext secrets.
const SECRETS_DIR: &str = "secrets";

/// File holding the plaintext device password.
const DEVICE_PASSWORD_FILE: &str = "device-password";

/// File holding the plaintext WPA2 pre-shared key for provisioning AP mode.
const AP_PSK_FILE: &str = "ap-psk";

/// Mode of the secrets directory: owner-only, so a non-root local process
/// cannot even list it.
const SECRETS_DIR_MODE: u32 = 0o700;

/// Mode of every secret file: owner read/write only.
const SECRET_FILE_MODE: u32 = 0o600;

/// Alphabet for generated secrets: digits and uppercase letters minus the
/// six characters an operator cannot reliably tell apart when reading a label
/// (`0`, `O`, `o`, `1`, `l`, `I`). Lowercase is excluded wholesale, which
/// removes `o` and `l`; `0`, `1`, `O` and `I` are removed by hand. Exactly 32
/// characters remain, so each one carries 5 bits.
const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

/// Length of a generated secret in characters.
///
/// 16 characters of a 32-symbol alphabet is 80 bits of entropy, and sits inside
/// the 8..=63 character range WPA2 requires of a pre-shared key.
const SECRET_LEN: usize = 16;

/// Length of the device identifier in random bytes; rendered as 32 hex chars.
const DEVICE_ID_BYTES: usize = 16;

/// Largest byte value plus one that may be folded into [`ALPHABET`] without
/// bias: the biggest multiple of the alphabet length that fits in a byte.
const UNBIASED_LIMIT: usize = 256 - (256 % ALPHABET.len());

/// What [`ensure_identity`] had to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// At least one part of the identity was missing and has been generated.
    Created,
    /// Everything was already present; nothing was generated or rewritten.
    AlreadyPresent,
}

/// Give this device an identity and its per-device secrets, if it has none.
pub fn ensure_identity(state_dir: &Path, settings: &mut Settings) -> Result<Outcome> {
    let rng = SystemRandom::new();
    let mut created = false;

    if settings.provisioning.device_id.is_none() {
        settings.provisioning.device_id = Some(generate_device_id(&rng)?);
        created = true;
    }

    if settings.access.device.password_hash.is_none() {
        let password = generate_secret(&rng, SECRET_LEN)?;
        let hash = hash_password(&password)?;
        // Plaintext first, hash second: the caller's settings save is the
        // commit point, so a crash in between orphans a plaintext that the next
        // boot overwrites, rather than stranding a hash with no way to learn
        // the password it stands for.
        write_secret(state_dir, DEVICE_PASSWORD_FILE, &password)?;
        settings.access.device.password_hash = Some(hash);
        settings.access.device.generation = settings.access.device.generation.saturating_add(1);
        created = true;
    }

    if read_secret(state_dir, AP_PSK_FILE)?.is_none() {
        let psk = generate_secret(&rng, SECRET_LEN)?;
        write_secret(state_dir, AP_PSK_FILE, &psk)?;
        created = true;
    }

    Ok(if created {
        Outcome::Created
    } else {
        Outcome::AlreadyPresent
    })
}

/// Read the plaintext device password from STATE, if it has been generated.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be read.
// Compiled only for tests: production code has no reader — the credential of
// record for shell access is an SSH key or a transient password, and nothing
// else consumes the file yet. The provisioning tests still need to assert the
// secret landed where a future reader will look, which is what this is for. A
// future access reconciler that wants it lifts the cfg rather than
// reintroducing an allow(dead_code) that outlives its truth.
#[cfg(test)]
pub fn read_device_password(state_dir: &Path) -> Result<Option<String>> {
    read_secret(state_dir, DEVICE_PASSWORD_FILE)
}

/// Read the plaintext AP PSK from STATE, if it has been generated.
///
/// # Errors
///
/// Returns an error when the file exists but cannot be read.
pub fn read_ap_psk(state_dir: &Path) -> Result<Option<String>> {
    read_secret(state_dir, AP_PSK_FILE)
}

/// Hash `password` with Argon2id default parameters into a PHC string, the
/// same hasher apid verifies with.
pub fn hash_password(password: &str) -> Result<String> {
    micad_settings::hash_password(password).map_err(|err| anyhow!("hash password: {err}"))
}

#[cfg(test)]
use micad_settings::verify_password;

/// Draw a fresh device identifier: [`DEVICE_ID_BYTES`] CSPRNG bytes as
/// lowercase hex. Independent of both secrets, so learning one tells nothing
/// about the others.
fn generate_device_id(rng: &SystemRandom) -> Result<String> {
    let mut bytes = [0u8; DEVICE_ID_BYTES];
    rng.fill(&mut bytes)
        .map_err(|_| anyhow!("system CSPRNG unavailable"))?;
    Ok(hex::encode(bytes))
}

/// Draw `len` characters uniformly from [`ALPHABET`] using the system CSPRNG.
fn generate_secret(rng: &SystemRandom, len: usize) -> Result<String> {
    let mut out = String::with_capacity(len);
    let mut buf = [0u8; 64];
    while out.len() < len {
        rng.fill(&mut buf)
            .map_err(|_| anyhow!("system CSPRNG unavailable"))?;
        for &byte in &buf {
            if usize::from(byte) >= UNBIASED_LIMIT {
                continue;
            }
            out.push(char::from(ALPHABET[usize::from(byte) % ALPHABET.len()]));
            if out.len() == len {
                break;
            }
        }
    }
    Ok(out)
}

/// Path of the secrets directory under `state_dir`.
fn secrets_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(SECRETS_DIR)
}

/// Create the secrets directory if needed and pin it to [`SECRETS_DIR_MODE`].
fn ensure_secrets_dir(state_dir: &Path) -> Result<PathBuf> {
    let dir = secrets_dir(state_dir);
    if !dir.is_dir() {
        if let Some(parent) = dir.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create state directory {}", parent.display()))?;
        }
        // mkdir(2) carries the mode, so the directory is never momentarily
        // group- or world-readable between creation and chmod.
        match DirBuilder::new().mode(SECRETS_DIR_MODE).create(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                return Err(err).with_context(|| format!("create {}", dir.display()));
            }
        }
    }
    // Unconditional, and not masked by the umask the way mkdir's mode is: a
    // directory left behind by an older or interrupted run gets tightened.
    fs::set_permissions(&dir, Permissions::from_mode(SECRETS_DIR_MODE))
        .with_context(|| format!("chmod {}", dir.display()))?;
    Ok(dir)
}

/// Write `value` to `<state_dir>/secrets/<name>` atomically and at
/// [`SECRET_FILE_MODE`].
fn write_secret(state_dir: &Path, name: &str, value: &str) -> Result<()> {
    let dir = ensure_secrets_dir(state_dir)?;
    let target = dir.join(name);
    mica_fs::Replace::new(&target, "tmp")?
        .mode(SECRET_FILE_MODE)
        .write(value)
        .with_context(|| format!("write {}", target.display()))
}

/// Read `<state_dir>/secrets/<name>`, or `None` when it does not exist.
///
/// Trailing whitespace is trimmed: the alphabet contains none, so trimming
/// cannot corrupt a generated secret, and it keeps a file an operator opened in
/// an editor (which appends a newline) from producing a PSK hostapd rejects.
fn read_secret(state_dir: &Path, name: &str) -> Result<Option<String>> {
    let path = secrets_dir(state_dir).join(name);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text.trim_end().to_string())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
    }
}

#[cfg(test)]
mod tests;

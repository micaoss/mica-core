//! What BLE provisioning keeps across reboots: the `password` verifier, the
//! failure count and lockout, and the last sessions.
//!
//! The verifier is a secret (`<state>/secrets/ble-verifier.json`, 0600); the
//! lockout and the history are not, and sit beside the settings.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::spake2plus::{self, POINT_LEN, Record};
use super::{ID_PROVER, ID_VERIFIER};

/// PBKDF2 iterations of a verifier this device makes: about two seconds on
/// an armhf board, measured.
pub const ITERATIONS: u32 = 300_000;
/// Bytes of a verifier's salt.
pub const SALT_LEN: usize = 16;
/// Consecutive failures before a lockout.
pub const MAX_FAILURES: u32 = 5;
/// How long each lockout lasts, in seconds; the last one repeats.
const LOCKOUTS: [u64; 3] = [10 * 60, 60 * 60, 24 * 60 * 60];
/// Sessions the history keeps.
pub const HISTORY_LEN: usize = 10;

const VERIFIER_FILE: &str = "ble-verifier.json";
const LOCKOUT_FILE: &str = "ble-lockout.json";
const HISTORY_FILE: &str = "ble-history.json";

/// The record of `password` under a fresh salt and `iterations`.
///
/// # Errors
/// The RNG failed, or `iterations` is 0.
pub fn make_record(password: &[u8], iterations: u32) -> Result<Record> {
    let mut salt = [0u8; SALT_LEN];
    aws_lc_rs::rand::fill(&mut salt).map_err(|_| anyhow::anyhow!("the system RNG failed"))?;
    let iterations = NonZeroU32::new(iterations).context("a verifier needs iterations")?;
    Ok(spake2plus::register(
        password,
        &salt,
        iterations,
        ID_PROVER,
        ID_VERIFIER,
    ))
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredRecord {
    salt: String,
    iterations: u32,
    w0: String,
    #[serde(rename = "L")]
    l: String,
}

/// The failure count and the lockout.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Lockout {
    /// Consecutive failures since the last success or lockout.
    pub failures: u32,
    /// Lockouts so far: which of the durations the next one takes.
    pub level: u32,
    /// Seconds since the epoch the current lockout ends at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked_until: Option<u64>,
}

impl Lockout {
    /// Seconds left of a lockout at `now`, if one holds.
    #[must_use]
    pub fn remaining(&self, now: u64) -> Option<u64> {
        self.locked_until
            .filter(|until| *until > now)
            .map(|until| until - now)
    }

    /// Attempts before the next lockout.
    #[must_use]
    pub fn attempts_left(&self) -> u32 {
        MAX_FAILURES.saturating_sub(self.failures)
    }

    /// Count one failure at `now`; the fifth locks.
    pub fn charge(&mut self, now: u64) {
        self.failures += 1;
        if self.failures >= MAX_FAILURES {
            let index = usize::try_from(self.level)
                .unwrap_or(usize::MAX)
                .min(LOCKOUTS.len() - 1);
            self.locked_until = Some(now + LOCKOUTS[index]);
            self.level = self.level.saturating_add(1);
            self.failures = 0;
        }
    }

    /// A successful exchange: `before` is the state the attempt was charged
    /// on, and the consecutive count starts again.
    pub fn succeed(&mut self, before: &Self) {
        *self = Self {
            failures: 0,
            ..before.clone()
        };
    }
}

/// One session, as the history keeps it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    /// Seconds since the epoch the session ended at.
    pub at: u64,
    /// The window's trigger: `unclaimed`, `boot`, `offline` or `manual`.
    pub trigger: String,
    /// How it ended: `claimed`, `wifi-connected`, `finished`, `failed`,
    /// `auth-failed`, `locked` or `abandoned`.
    pub outcome: String,
}

/// The files under one state directory.
#[derive(Debug, Clone)]
pub struct Store {
    state_dir: PathBuf,
}

impl Store {
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
        }
    }

    /// The `password` verifier, if one was made.
    ///
    /// # Errors
    /// The file exists and cannot be read or parsed.
    pub fn verifier(&self) -> Result<Option<Record>> {
        let Some(text) = crate::identity::read_secret(&self.state_dir, VERIFIER_FILE)? else {
            return Ok(None);
        };
        let stored: StoredRecord =
            serde_json::from_str(&text).context("the BLE verifier is not a record")?;
        let mut w0 = [0u8; 32];
        hex::decode_to_slice(&stored.w0, &mut w0).context("the verifier's w0")?;
        let mut l = [0u8; POINT_LEN];
        hex::decode_to_slice(&stored.l, &mut l).context("the verifier's L")?;
        Ok(Some(Record {
            salt: hex::decode(&stored.salt).context("the verifier's salt")?,
            iterations: stored.iterations,
            w0,
            l,
        }))
    }

    /// Replace the `password` verifier.
    ///
    /// # Errors
    /// The secret could not be written.
    pub fn set_verifier(&self, record: &Record) -> Result<()> {
        let stored = StoredRecord {
            salt: hex::encode(&record.salt),
            iterations: record.iterations,
            w0: hex::encode(record.w0),
            l: hex::encode(record.l),
        };
        crate::identity::write_secret(
            &self.state_dir,
            VERIFIER_FILE,
            &serde_json::to_string(&stored)?,
        )
    }

    fn read<T: serde::de::DeserializeOwned + Default>(&self, name: &str) -> T {
        let path = self.state_dir.join(name);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
                tracing::warn!(path = %path.display(), error = %err, "unreadable; starting empty");
                T::default()
            }),
            Err(_) => T::default(),
        }
    }

    fn write<T: serde::Serialize>(&self, name: &str, value: &T) -> Result<()> {
        crate::fswrite::write_config(
            &self.state_dir.join(name),
            &serde_json::to_string(value)?,
            0o600,
        )
    }

    /// The failure count and lockout; none when the file is missing.
    #[must_use]
    pub fn lockout(&self) -> Lockout {
        self.read(LOCKOUT_FILE)
    }

    /// # Errors
    /// The file could not be written.
    pub fn set_lockout(&self, lockout: &Lockout) -> Result<()> {
        self.write(LOCKOUT_FILE, lockout)
    }

    /// The last sessions, oldest first.
    #[must_use]
    pub fn history(&self) -> Vec<Entry> {
        self.read(HISTORY_FILE)
    }

    /// Add a session, dropping the oldest beyond [`HISTORY_LEN`].
    ///
    /// # Errors
    /// The file could not be written.
    pub fn push_history(&self, entry: Entry) -> Result<()> {
        let mut history = self.history();
        history.push(entry);
        let excess = history.len().saturating_sub(HISTORY_LEN);
        history.drain(..excess);
        self.write(HISTORY_FILE, &history)
    }
}

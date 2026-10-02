//! Password hashing/verification and login brute-force backoff.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The `backoffBase`. The first failure costs a
/// second; every consecutive one doubles it.
const BACKOFF_BASE: Duration = Duration::from_secs(1);
/// The `backoffMax`. The curve stops here and never becomes permanent —
/// see [`LoginGuard`] for why apid does not arm the `lockoutThreshold`.
const BACKOFF_MAX: Duration = Duration::from_secs(300);

/// Hash `password` with argon2id default parameters into a PHC string.
pub fn hash_password(password: &str) -> anyhow::Result<String> {
    micad_settings::hash_password(password).map_err(|err| anyhow::anyhow!("hash password: {err}"))
}

pub use micad_settings::verify_password;

/// The backoff a run of `failures` consecutive failures has earned:
/// `BACKOFF_BASE * 2^(failures - 1)`, capped at [`BACKOFF_MAX`].
///
/// Pure and total, so the curve is testable without a clock: the shift
/// saturates rather than overflowing, and the cap makes every count past the
/// ninth the same answer anyway.
fn backoff_for(failures: u32) -> Duration {
    if failures == 0 {
        return Duration::ZERO;
    }
    let factor = 1u64.checked_shl(failures - 1).unwrap_or(u64::MAX);
    let secs = BACKOFF_BASE.as_secs().saturating_mul(factor);
    Duration::from_secs(secs.min(BACKOFF_MAX.as_secs()))
}

/// Global (not per-client) login backoff on an exponential
/// curve: each consecutive failure doubles the wait before the next attempt is
/// accepted, from [`BACKOFF_BASE`] up to [`BACKOFF_MAX`]. A single shared
/// counter is deliberate — the appliance has one admin password, so per-client
/// tracking buys nothing against an online guesser, who would rotate source
/// addresses anyway.
///
/// The counter survives an expired window. Clearing `failures` when the window
/// lapses would make the rule flat: an attacker waits the window out, the next
/// run starts from zero, and the cost per guess never rises. Only
/// [`LoginGuard::record_success`] resets the run, so guessing gets
/// monotonically more expensive — under 300 guesses a day once the cap is
/// reached. The curve never becomes permanent: a `lockoutThreshold` pairs
/// with "releasable only with physical presence" and apid has no presence
/// check, so arming a threshold nothing can clear would let an attacker convert
/// a guessing attempt into a permanent denial of management. The cap is the
/// whole control — a locked-out administrator who knows the password waits at
/// most [`BACKOFF_MAX`].
///
/// Persistence is [`GuardStore`]'s job, not this type's: the guard stays a pure
/// counter-and-clock so the curve remains testable without a filesystem, and
/// the store wraps it to satisfy the "a power cycle must not reset the clock".
/// The state lives on STATE.
#[derive(Default)]
pub struct LoginGuard {
    failures: u32,
    locked_until: Option<Instant>,
}

impl LoginGuard {
    /// Gate one attempt: refuse while a window is armed, otherwise charge the
    /// attempt up front and admit it.
    ///
    /// Check and charge are one operation under one lock acquisition,
    /// deliberately. A handler that consulted the guard, verified the password
    /// and only then recorded the outcome would hold the lock for none of the
    /// middle, so N concurrent submissions would all pass the bare check before
    /// any recorded a failure, multiplying every window on the curve by the
    /// attacker's concurrency. Charging at admission arms the window before the
    /// lock is released, so a burst timed to a window's expiry buys one guess,
    /// not N. The charge is the pessimistic one: [`Self::record_success`]
    /// repays it by ending the run, and a failed attempt calls
    /// [`Self::confirm_failure`] to move the window's start to the outcome. An
    /// attempt that ends in neither — an infrastructure error mid-attempt —
    /// stays charged with the admission-time window, which errs closed.
    pub fn begin_attempt(&mut self) -> bool {
        if !self.check() {
            return false;
        }
        self.record_failure();
        true
    }

    /// Re-arm the window the run has earned, after a failed attempt reports
    /// its outcome.
    ///
    /// The attempt was already counted at admission; this only moves the
    /// window's start from admission time to outcome time. Without it the
    /// verification's own duration would eat into the wait — argon2 costs a
    /// meaningful fraction of the one-second base window by design — and the
    /// curve's early steps would be shorter than they claim.
    pub fn confirm_failure(&mut self) {
        self.locked_until = Some(Instant::now() + backoff_for(self.failures));
    }

    /// True when a login attempt may proceed; an elapsed window is cleared,
    /// but the failure run behind it is deliberately kept.
    pub fn check(&mut self) -> bool {
        match self.locked_until {
            Some(until) if until > Instant::now() => false,
            Some(_) => {
                self.locked_until = None;
                true
            }
            None => true,
        }
    }

    /// Record a failed login and arm the window this run has earned.
    pub fn record_failure(&mut self) {
        self.failures = self.failures.saturating_add(1);
        self.locked_until = Some(Instant::now() + backoff_for(self.failures));
    }

    /// Record a successful login, ending the failure run.
    pub fn record_success(&mut self) {
        self.failures = 0;
        self.locked_until = None;
    }

    /// The on-disk shape of the current state.
    ///
    /// `Instant` is monotonic and dies with the process, so the armed window
    /// crosses a restart as an **absolute UNIX timestamp** instead. That
    /// trades away precision against clock steps — an NTP jump or a dead RTC
    /// moves the window with the clock — which is accepted: the error is
    /// bounded by the cap on load, and the alternative (persisting a bare
    /// remaining-duration) would let a reboot restart the window from full,
    /// turning every power cycle into extra punishment.
    ///
    /// A sub-second remainder rounds **up** to one second rather than down to
    /// "no window": down would make a restart inside the first backoff step a
    /// free retry, the exact bypass the persistence exists to close.
    fn to_persisted(&self) -> PersistedGuard {
        let remaining = self
            .locked_until
            .map(|until| until.saturating_duration_since(Instant::now()))
            .unwrap_or(Duration::ZERO);
        PersistedGuard {
            failures: self.failures,
            locked_until_unix: if remaining.is_zero() {
                0
            } else {
                crate::persist::now_unix().saturating_add(remaining.as_secs().max(1))
            },
        }
    }

    /// Rebuild the guard from a persisted snapshot.
    ///
    /// The remaining window is capped at [`BACKOFF_MAX`] **on load**: a
    /// corrupt or far-future timestamp — a clock that stepped backwards after
    /// the save, a bit-flipped file that still parses — must not arm a window
    /// the curve itself refuses to. "Never permanent" has to survive bad
    /// data, not just good arithmetic. The failure count needs no such cap;
    /// `backoff_for` already saturates.
    fn from_persisted(persisted: &PersistedGuard) -> Self {
        let remaining_secs = persisted
            .locked_until_unix
            .saturating_sub(crate::persist::now_unix())
            .min(BACKOFF_MAX.as_secs());
        let remaining = Duration::from_secs(remaining_secs);
        Self {
            failures: persisted.failures,
            locked_until: (!remaining.is_zero()).then(|| Instant::now() + remaining),
        }
    }
}

/// What [`GuardStore`] writes to `login_guard.json`.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PersistedGuard {
    /// The consecutive-failure run, `LoginGuard::failures` verbatim.
    failures: u32,
    /// UNIX seconds at which the armed window ends; `0` when none is armed.
    locked_until_unix: u64,
}

/// [`LoginGuard`] behind a lock, persisted to disk on every mutation
/// (an apid restart or a power cycle must not reset the clock).
///
/// Lock acquisitions recover from poisoning for the same reason the previous
/// `Mutex<LoginGuard>` in `routes.rs` did: a panic while holding the counter
/// must not convert every later login into a panic of its own.
pub struct GuardStore {
    inner: Mutex<LoginGuard>,
    /// `None` = in-memory only, for tests built without persistence.
    path: Option<PathBuf>,
}

impl GuardStore {
    /// A store that never touches disk.
    pub fn ephemeral() -> Self {
        Self {
            inner: Mutex::new(LoginGuard::default()),
            path: None,
        }
    }

    /// Load the persisted state at `path`, or start clean.
    ///
    /// Infallible by design, and the failure direction is chosen per case: an
    /// absent file is first boot; an unreadable or unparsable one starts a
    /// clean slate with a loud log rather than refusing to start — corruption
    /// of a rate-limiter file must degrade to the in-RAM guard, never to a
    /// daemon that will not serve or a lock that will not lift.
    pub fn load(path: PathBuf) -> Self {
        let guard = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<PersistedGuard>(&bytes) {
                Ok(persisted) => LoginGuard::from_persisted(&persisted),
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "login-guard state is corrupt; starting a clean slate"
                    );
                    LoginGuard::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => LoginGuard::default(),
            Err(err) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "login-guard state is unreadable; starting a clean slate"
                );
                LoginGuard::default()
            }
        };
        Self {
            inner: Mutex::new(guard),
            path: Some(path),
        }
    }

    /// Run `mutate` under the lock and persist iff the state changed.
    ///
    /// The changed-check is load-bearing: a refused attempt mutates nothing,
    /// and without the check an attacker hammering the login endpoint while
    /// throttled would convert every refusal into an fsync — flash wear and
    /// I/O load bought for the price of an HTTP request. The write happens
    /// while the lock is held so the on-disk state can never lag an admitted
    /// attempt; it is a tiny file at human login rate, so blocking here is
    /// cheaper than any ordering bug letting a restart forget a charge.
    ///
    /// A failed write degrades to in-RAM behavior with a loud log rather
    /// than failing the login: see the corruption rationale on [`Self::load`].
    fn with<R>(&self, mutate: impl FnOnce(&mut LoginGuard) -> R) -> R {
        let mut guard = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = (guard.failures, guard.locked_until);
        let result = mutate(&mut guard);
        if let Some(path) = &self.path
            && (guard.failures, guard.locked_until) != before
        {
            let written = serde_json::to_string(&guard.to_persisted())
                .map_err(anyhow::Error::from)
                .and_then(|contents| crate::persist::write_atomically(path, &contents, 0o600));
            if let Err(err) = written {
                tracing::warn!(
                    path = %path.display(),
                    error = %err,
                    "persisting login-guard state failed; counters are RAM-only until it succeeds"
                );
            }
        }
        result
    }

    /// [`LoginGuard::begin_attempt`] with persistence.
    pub fn begin_attempt(&self) -> bool {
        self.with(LoginGuard::begin_attempt)
    }

    /// [`LoginGuard::confirm_failure`] with persistence.
    pub fn confirm_failure(&self) {
        self.with(LoginGuard::confirm_failure)
    }

    /// [`LoginGuard::record_success`] with persistence.
    pub fn record_success(&self) {
        self.with(LoginGuard::record_success)
    }
}

#[cfg(test)]
mod tests;

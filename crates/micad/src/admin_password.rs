//! The administrator password, asked again for a change that cannot be
//! taken back: the factory reset, an owner certificate, a driver extension.
//! One backoff counts the wrong passwords of all of them.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::factory_reset::Refusal;

/// Wrong passwords tolerated before micad's own backoff starts.
const FREE_FAILURES: u32 = 3;
/// The longest backoff.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// micad's backoff on wrong administrator passwords.
#[derive(Default)]
pub struct PasswordBackoff {
    state: Mutex<Backoff>,
}

#[derive(Default)]
struct Backoff {
    failures: u32,
    until: Option<Instant>,
}

impl PasswordBackoff {
    fn state(&self) -> std::sync::MutexGuard<'_, Backoff> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The refusal while the backoff runs.
    pub fn throttled(&self) -> Option<Refusal> {
        let left = self.state().until?.checked_duration_since(Instant::now())?;
        Some(Refusal::new(
            "login_throttled",
            format!(
                "too many wrong passwords; try again in {} s",
                left.as_secs().max(1)
            ),
        ))
    }

    /// Count a wrong password: after [`FREE_FAILURES`] each one doubles the
    /// wait, from 5 s up to [`MAX_BACKOFF`].
    pub fn failed(&self) {
        let mut backoff = self.state();
        backoff.failures = backoff.failures.saturating_add(1);
        if let Some(over) = backoff.failures.checked_sub(FREE_FAILURES + 1) {
            let wait = Duration::from_secs(5)
                .saturating_mul(1 << over.min(10))
                .min(MAX_BACKOFF);
            backoff.until = Some(Instant::now() + wait);
        }
    }

    /// A right password clears the backoff.
    pub fn succeeded(&self) {
        *self.state() = Backoff::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_passwords_back_off_and_a_right_one_clears_it() {
        let backoff = PasswordBackoff::default();
        for _ in 0..FREE_FAILURES {
            backoff.failed();
            assert_eq!(backoff.throttled(), None);
        }
        backoff.failed();
        let refusal = backoff.throttled().expect("backed off");
        assert_eq!(refusal.code, "login_throttled");
        backoff.succeeded();
        assert_eq!(backoff.throttled(), None);
    }
}

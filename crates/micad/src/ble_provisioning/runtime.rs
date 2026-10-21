//! BLE provisioning as the rest of micad sees it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::store::{ITERATIONS, Lockout, Store, make_record};

/// The audit source of everything BLE provisioning records.
pub const AUDIT_SOURCE: &str = "ble";

/// The verifier, the lockout and the audit trail, or nothing on a daemon
/// that has no state directory or no Bluetooth.
#[derive(Default)]
pub struct Provisioning {
    store: Option<Store>,
    audit_dir: Option<PathBuf>,
    /// Bumped by every password change; a verifier made for an older one is
    /// not written.
    generation: Mutex<u64>,
}

impl Provisioning {
    #[must_use]
    pub fn new(store: Store, audit_dir: Option<PathBuf>) -> Self {
        Self {
            store: Some(store),
            audit_dir,
            generation: Mutex::new(0),
        }
    }

    /// Append one line to the audit trail.
    pub fn audit(&self, event: &str, outcome: &str) {
        let Some(dir) = &self.audit_dir else {
            return;
        };
        let line =
            micad_settings::audit_line(event, outcome, AUDIT_SOURCE, micad_settings::ACTOR_DEVICE);
        if let Err(err) = micad_settings::append_audit_line(dir, &line) {
            tracing::warn!(error = %err, event, "the audit line could not be written");
        }
    }

    /// The administrator password is now `password`: the claim set it or the
    /// administrator changed it. The old verifier goes at once; the new one
    /// takes about two seconds and is made in the background.
    ///
    /// # Errors
    /// The old verifier could not be removed.
    pub fn password_set(self: &Arc<Self>, password: String) -> Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        let generation = {
            let mut generation = self.lock_generation();
            store.remove_verifier()?;
            *generation += 1;
            *generation
        };
        self.make_verifier(password, generation, "replaced");
        Ok(())
    }

    /// The administrator logged in to the console with `password`: the
    /// lockout is over, and a device claimed before this feature gets its
    /// verifier, in the background.
    ///
    /// # Errors
    /// The lockout or the verifier could not be read or written.
    pub fn logged_in(self: &Arc<Self>, password: String) -> Result<()> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        if store.lockout() != Lockout::default() {
            store.set_lockout(&Lockout::default())?;
            self.audit("ble-lockout", "reset");
        }
        let generation = *self.lock_generation();
        if store.verifier()?.is_none() {
            self.make_verifier(password, generation, "created");
        }
        Ok(())
    }

    fn lock_generation(&self) -> std::sync::MutexGuard<'_, u64> {
        self.generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Make the verifier of `password` and write it, unless the password
    /// changed meanwhile (`generation` moved on).
    fn make_verifier(self: &Arc<Self>, password: String, generation: u64, outcome: &'static str) {
        let this = Arc::clone(self);
        let made = tokio::task::spawn_blocking(move || {
            let Some(store) = &this.store else {
                return;
            };
            let result = make_record(password.as_bytes(), ITERATIONS).and_then(|record| {
                let current = this.lock_generation();
                if *current == generation {
                    store.set_verifier(&record)?;
                    this.audit("ble-verifier", outcome);
                }
                drop(current);
                Ok(())
            });
            if let Err(err) = result {
                tracing::warn!(error = %format!("{err:#}"), "the BLE verifier could not be made");
            }
        });
        drop(made);
    }

    /// Whether the `password` verifier exists.
    #[must_use]
    pub fn verifier_ready(&self) -> bool {
        self.store
            .as_ref()
            .is_some_and(|store| matches!(store.verifier(), Ok(Some(_))))
    }
}

//! BLE provisioning as the rest of micad sees it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use anyhow::Result;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use super::engine::Input;
use super::store::{ITERATIONS, Lockout, Store, make_record};
use super::window::MANUAL_SECONDS;

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
    /// The engine, once it runs.
    engine: OnceLock<mpsc::UnboundedSender<Input>>,
    /// Whether a session holds the radio: the console's Wi-Fi writes wait.
    wifi_locked: Arc<AtomicBool>,
}

impl Provisioning {
    #[must_use]
    pub fn new(store: Store, audit_dir: Option<PathBuf>) -> Self {
        Self {
            store: Some(store),
            audit_dir,
            ..Self::default()
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

    /// The store, when there is one.
    #[must_use]
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    /// The flag the engine sets while a session holds the radio.
    #[must_use]
    pub fn wifi_lock(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.wifi_locked)
    }

    /// Whether a session holds the radio.
    #[must_use]
    pub fn wifi_locked(&self) -> bool {
        self.wifi_locked.load(Ordering::SeqCst)
    }

    /// Hand the running engine's input to this facade.
    pub fn attach(&self, engine: mpsc::UnboundedSender<Input>) {
        let _ = self.engine.set(engine);
    }

    fn send(&self, input: Input) -> bool {
        self.engine
            .get()
            .is_some_and(|engine| engine.send(input).is_ok())
    }

    /// Another channel claimed the device.
    pub fn claimed_elsewhere(&self) {
        self.send(Input::ClaimedElsewhere);
    }

    /// The default route came or went.
    pub fn route(&self, present: bool) {
        self.send(Input::Route(present));
    }

    /// `GET /api/v1/bluetooth/provisioning`; none while no engine runs.
    pub async fn status(&self) -> Option<Value> {
        let (reply, answer) = oneshot::channel();
        if !self.send(Input::Status { reply }) {
            return None;
        }
        answer.await.ok()
    }

    /// Open a `manual` window for `seconds`: its temp code, or why not
    /// (`invalid_duration`, `not_claimed`, `locked`, `unavailable`).
    ///
    /// # Errors
    /// The reason, as above.
    pub async fn open_manual(&self, seconds: u64) -> Result<String, &'static str> {
        if !MANUAL_SECONDS.contains(&seconds) {
            return Err("invalid_duration");
        }
        if self.engine.get().is_none() {
            return Err("unavailable");
        }
        let code = temp_code().ok_or("unavailable")?;
        let password = code.clone();
        let record =
            tokio::task::spawn_blocking(move || make_record(password.as_bytes(), ITERATIONS))
                .await
                .map_err(|_| "unavailable")?
                .map_err(|_| "unavailable")?;
        let (reply, answer) = oneshot::channel();
        if !self.send(Input::OpenManual {
            seconds,
            code: code.clone(),
            record,
            reply,
        }) {
            return Err("unavailable");
        }
        answer.await.map_err(|_| "unavailable")??;
        Ok(code)
    }

    /// Close the `manual` window.
    pub async fn close_manual(&self) {
        let (reply, answer) = oneshot::channel();
        if self.send(Input::CloseManual { reply }) {
            let _ = answer.await;
        }
    }
}

/// Eight random digits.
fn temp_code() -> Option<String> {
    let mut bytes = [0u8; 8];
    aws_lc_rs::rand::fill(&mut bytes).ok()?;
    let value = u64::from_be_bytes(bytes) % 100_000_000;
    Some(format!("{value:08}"))
}

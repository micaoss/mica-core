//! The reset tiers and credential recovery: the authority each one needs, the
//! one write each one makes, and the secret that appears on exactly one
//! channel.
//!
//! What a tier DOES to the device is micad's contract and is tested in
//! `micad/src/reset.rs`, against the tier table cell for cell. What is here is
//! apid's half: who may ask, what gets committed, and what the trail records.

use super::*;
use crate::routes::{Assertion, NoPresence, Presence};
use serde_json::json;
use std::sync::Arc;

mod presence;
mod recovery;
mod rotation;
mod staging;

const RESET_PATH: &str = "/api/v1/reset";
const RECOVERY_PATH: &str = "/api/v1/recovery/credential";

/// The mechanism the fixtures assert presence under.
///
/// A mechanism is a BOARD fact and neither shipped board declares one,
/// so this is a fixture's name for a door no
/// mica device has — never a value read out of the tree, which would be a claim
/// the board table does not support.
const FIXTURE_MECHANISM: &str = "boot-menu";

/// What both shipped boards declare.
const DECLARES_NONE: &str = "BOARD_RECOVERY_ACTIONS=\"\"\n";

/// A board declaring one action under [`FIXTURE_MECHANISM`].
const DECLARES_ONE: &str = "\
BOARD_RECOVERY_ACTIONS=\"BOOT_MENU\"
RECOVERY_BOOT_MENU_INTENT=recovery
RECOVERY_BOOT_MENU_MECHANISM=boot-menu
RECOVERY_BOOT_MENU_CHANNEL=/dev/tty0
RECOVERY_BOOT_MENU_TIER=none
";

/// The password the fixtures claim the device with. A sentinel, so every
/// assertion that it reached nowhere is about a string nothing else produces.
const PW_SENTINEL: &str = "PW-SENTINEL-recovery-previous";

/// A presence seam a test can drive, which is the only way one is ever
/// established: an assertion is an action at the DEVICE, so no request and no
/// fixture tree can produce one.
struct FakePresence {
    /// Cleared by [`Presence::spend`], as the shipped reader's marker is
    /// unlinked: an assertion this seam has spent establishes nothing further.
    established: std::sync::atomic::AtomicBool,
    /// Everything [`Presence::publish`] was given, in order. The one place a
    /// minted credential is allowed to appear.
    published: std::sync::Mutex<Vec<String>>,
    /// When true, publication fails — the `aborted`.
    publish_fails: bool,
}

impl FakePresence {
    fn present() -> Arc<Self> {
        Arc::new(Self {
            established: std::sync::atomic::AtomicBool::new(true),
            published: std::sync::Mutex::new(Vec::new()),
            publish_fails: false,
        })
    }

    fn absent() -> Arc<Self> {
        Arc::new(Self {
            established: std::sync::atomic::AtomicBool::new(false),
            published: std::sync::Mutex::new(Vec::new()),
            publish_fails: false,
        })
    }

    fn present_but_unwritable() -> Arc<Self> {
        Arc::new(Self {
            established: std::sync::atomic::AtomicBool::new(true),
            published: std::sync::Mutex::new(Vec::new()),
            publish_fails: true,
        })
    }

    fn published(&self) -> Vec<String> {
        self.published.lock().unwrap().clone()
    }

    /// The one secret this flow published, which is the credential the
    /// operator standing at the console reads.
    fn secret(&self) -> String {
        let published = self.published();
        assert_eq!(published.len(), 1, "the credential was not published once");
        published[0].clone()
    }
}

impl Presence for FakePresence {
    fn assert(&self) -> Result<Assertion, NoPresence> {
        if self.established.load(std::sync::atomic::Ordering::SeqCst) {
            Ok(Assertion::for_test(FIXTURE_MECHANISM))
        } else {
            Err(NoPresence::Absent)
        }
    }

    fn publish(&self, _assertion: &Assertion, secret: &str) -> anyhow::Result<()> {
        if self.publish_fails {
            return Err(anyhow::anyhow!("the console could not be written"));
        }
        self.published.lock().unwrap().push(secret.to_string());
        Ok(())
    }

    fn spend(&self) -> anyhow::Result<()> {
        self.established
            .store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
}

/// A claimed device carrying an API token, an audit ring and a presence seam.
fn device(presence: Arc<dyn Presence>) -> (Router, Arc<FakeSettings>, TempDir, String) {
    let dir = TempDir::new().unwrap();
    let (tree, token) = with_token(claimed_tree());
    let fake = Arc::new(FakeSettings::new(tree));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY)
        .with_persistence(dir.path())
        .with_presence(presence));
    (router, fake, dir, token)
}

/// A device claimed through `POST /api/v1/setup`, with a generation to bump.
fn claimed_tree() -> serde_json::Value {
    let mut tree = configured_tree(PW_SENTINEL);
    tree["access"]["claim"] = json!({
        "via": "setup",
        "at": 1_700_000_000,
        "rotationRequired": false,
    });
    tree["access"]["device"] = json!({ "generation": 4 });
    tree["provisioning"] = json!({
        "state": "complete",
        "deviceId": "0123456789abcdef0123456789abcdef",
        "seededGeneration": 1,
    });
    tree
}

async fn access_of(fake: &FakeSettings) -> serde_json::Value {
    fake.get_settings("access").await.unwrap()
}

// --- The reset tiers -------------------------------------------------------

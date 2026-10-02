//! The claim lifecycle: the one-write commit, the retry after an interrupted
//! attempt, the already-claimed refusal and the forced rotation that bounds a
//! bootstrap credential.
//!
//! What a provisioning document MEANS — validation, idempotence, its own claim
//! gate — is micad's contract and is tested in `micad/src/provisioning_doc.rs`.
//! What is here is apid's half: the state a claim leaves behind, and the
//! bound apid enforces from it.

use super::*;
use serde_json::json;

mod bootstrap;
mod interruption;
mod route;

const CLAIM_PATH: &str = "/api/v1/claim";
const CHANGE_PASSWORD_PATH: &str = "/api/v1/actions/change-password";

/// The password a claim body carries. A sentinel, so every assertion that it
/// reached nowhere is an assertion about a string nothing else could produce.
const PW_SENTINEL: &str = "PW-SENTINEL-claim-bootstrap";

fn claim_body() -> String {
    json!({ "password": PW_SENTINEL }).to_string()
}

/// A device Layer 1 has provisioned and nothing has claimed.
fn seeded_tree() -> serde_json::Value {
    let mut tree = unconfigured_tree();
    tree["provisioning"] = json!({
        "state": "complete",
        "deviceId": "0123456789abcdef0123456789abcdef",
        "seededGeneration": 1,
    });
    tree
}

/// A device claimed by a provisioning document: an administrator credential,
/// an applied document, and NO claim record — which is the shape apid reads as
/// "claimed from a medium, still holding the password that was on it".
fn document_claimed_tree(password: &str) -> serde_json::Value {
    let mut tree = configured_tree(password);
    tree["provisioning"] = json!({
        "state": "complete",
        "deviceId": "0123456789abcdef0123456789abcdef",
        "seededGeneration": 1,
        "document": {
            "appliedVersion": 1,
            "appliedDigest": "9f2c1d0e5a7b4c3d2e1f00112233445566778899aabbccddeeff001122334455",
            "lastImport": {
                "source": "media",
                "outcome": "applied",
                "at": 1_700_000_000,
            },
        },
    });
    tree
}

/// The `access` subtree as the fake holds it.
async fn access_of(fake: &FakeSettings) -> serde_json::Value {
    fake.get_settings("access").await.unwrap()
}

// --- The one-write commit --------------------------------------------------

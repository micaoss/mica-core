//! Route tests for the provisioning-document status (`provisioning_api.rs`):
//! transport, authentication, the read-only surface and what it must never
//! serve. What a document MEANS — validation, idempotence, the claim gate — is
//! micad's contract, tested in `micad/src/provisioning_doc.rs`.

use super::*;

mod fleet;
mod status;
use status::*;

const STATUS_PATH: &str = "/api/v1/provisioning/status";

/// The baked manifest, in the shape `micad_settings::configuration` accepts.
///
/// Every field of that struct is required — it carries no serde defaults — so
/// a shortened fixture would not parse, the reader would fall back to the code
/// defaults, and the test would pass without anything having been read. This
/// is `meta.example/updates/manifest.json`.
const BAKED_MANIFEST: &str = r#"{
  "schema": "mica/meta/v1",
  "product": { "vendor": "example", "model": "mica-appliance" },
  "update": {
    "source": "https://baked.example/update/",
    "policy": "check",
    "checkIntervalMinutes": 1440
  },
  "http": { "credentialHosts": [] },
  "fleet": { "enabled": false, "url": null }
}
"#;

/// A device with the baked tree every device has, which is what this route
/// reads on every request. A build host has no `/usr/share/mica/meta`, so
/// without this the reading tests below never reach the handler at all.
///
/// The tree is the production shape: the manifest and nothing beside it.
/// `meta/GENERATED` is conditional on a device — staged only for
/// development-grade material — so one file is what a shipped image carries.
///
/// The `TempDir` comes back with the router because dropping it deletes the
/// tree the next request would read.
fn provisioning_app(tree: serde_json::Value) -> (Router, TempDir) {
    provisioning_app_with_updates(tree, None)
}

/// A route fixture carrying an optional isolated operator update document.
fn provisioning_app_with_updates(
    tree: serde_json::Value,
    updates_document: Option<&str>,
) -> (Router, TempDir) {
    provisioning_app_with_documents(tree, updates_document, None)
}

/// A route fixture carrying isolated operator update and fleet documents.
fn provisioning_app_with_documents(
    tree: serde_json::Value,
    updates_document: Option<&str>,
    fleet_document: Option<&str>,
) -> (Router, TempDir) {
    provisioning_app_with_manifest_and_documents(
        tree,
        BAKED_MANIFEST,
        updates_document,
        fleet_document,
    )
}

fn provisioning_app_with_manifest_and_documents(
    tree: serde_json::Value,
    manifest: &str,
    updates_document: Option<&str>,
    fleet_document: Option<&str>,
) -> (Router, TempDir) {
    let dir = TempDir::new().expect("temp baked metadata");
    let updates = dir.path().join("meta/updates");
    std::fs::create_dir_all(&updates).expect("meta/updates");
    std::fs::write(updates.join("manifest.json"), manifest).expect("baked manifest");
    let updates_path = dir.path().join("config/updates.json");
    if let Some(document) = updates_document {
        std::fs::create_dir(updates_path.parent().expect("operator config parent"))
            .expect("operator config directory");
        std::fs::write(&updates_path, document).expect("operator updates document");
    }
    let fleet_path = dir.path().join("config/fleet.json");
    if let Some(document) = fleet_document {
        std::fs::create_dir_all(fleet_path.parent().expect("fleet config parent"))
            .expect("fleet config directory");
        std::fs::write(&fleet_path, document).expect("operator fleet document");
    }
    let fake = Arc::new(FakeSettings::new(tree));
    let router = app(AppState::new(fake, SIGNING_KEY)
        .with_meta_manifest(updates.join("manifest.json"))
        .with_updates_path(updates_path)
        .with_fleet_path(fleet_path));
    (router, dir)
}

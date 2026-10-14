//! Refused configuration documents.

use super::super::{DocumentRefusal, MicadService, paths_overlap};
use crate::power::MockPower;
use std::sync::{Arc, Mutex};

use super::*;

pub(super) struct EmptyJournal;

#[async_trait::async_trait]
impl crate::diagnostics::JournalReader for EmptyJournal {
    async fn read(&self, _max_lines: usize) -> anyhow::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

pub(super) struct NoUnits;

#[async_trait::async_trait]
impl crate::diagnostics::UnitLister for NoUnits {
    async fn failed_units(&self) -> anyhow::Result<Vec<crate::diagnostics::FailedUnit>> {
        Ok(Vec::new())
    }
}

// --- The pour ----------------------------------------

/// A service with one recording reconciler per document-backed subtree,
/// over a store whose `/mica/config/` namespace is on disk and writable.
pub(super) fn pour(dir: &tempfile::TempDir, document: &str, text: &str) -> std::path::PathBuf {
    let config = dir.path().join("config");
    std::fs::create_dir_all(&config).expect("create the config namespace");
    let path = config.join(document);
    std::fs::write(&path, text).expect("pour the document");
    path
}

pub(super) async fn service_over(
    dir: &tempfile::TempDir,
) -> (MicadService, CallLog, Vec<DocumentRefusal>) {
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, SHADOW).expect("seed shadow");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let reconciler = |name, subtree| {
        Box::new(RecordingReconciler {
            name,
            subtree,
            calls: Arc::clone(&calls),
        }) as Box<dyn crate::reconciler::Reconciler>
    };
    let store = store_in(dir);
    // `load_with_refusals` and not `load`, which is the daemon's own choice
    // at `main.rs` and the whole subject here: `load` refuses the load, and
    // a test harness that used it could not reach a device that came up at
    // all with a broken document on it.
    let loaded = store
        .load_with_refusals()
        .expect("load the poured namespace");
    let refusals = loaded.refusals.clone();
    let service = MicadService::new(
        store,
        loaded.settings,
        vec![
            reconciler("hostname", "hostname"),
            reconciler("network", "network"),
            reconciler("wifiAp", "wifi"),
            reconciler("wifiClient", "wifi.client"),
            reconciler("sshd", "access.ssh"),
        ],
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        shadow_path,
        serde_json::json!({}),
    );
    service.set_config_refusals(loaded.refusals).await;
    (service, calls, refusals)
}

/// **Clause 2 of the pour gate, both directions, at the reconcile loop.**
#[tokio::test]
pub(super) async fn a_refused_document_skips_its_reconcilers_and_leaves_the_others_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    pour(
        &dir,
        micad_settings::WIFI_DOCUMENT,
        r#"{"schema_version": 1, "wifi": {"ap": {"mode": "alwys"}}}"#,
    );
    let (service, calls, refusals) = service_over(&dir).await;
    assert_eq!(refusals.len(), 1);
    let refusal = refusals[0].clone();
    service.apply_all().await;

    let mut ran = calls.lock().expect("call log").clone();
    ran.sort();
    assert_eq!(
        ran,
        vec![
            "hostname".to_string(),
            "network".to_string(),
            "sshd".to_string()
        ],
        "a refused wifi.json must cost the wifi reconcilers and nothing else"
    );

    let (_, state) = service.trees().await;
    for name in ["wifiAp", "wifiClient"] {
        let message = state[name]["refused"].as_str().unwrap_or_default();
        assert!(
            message.contains(&refusal.path.display().to_string()),
            "{name} must record a refusal naming the file, got {:?}",
            state[name]
        );
        assert!(
            state[name].get("applied").is_none(),
            "{name} must not report an applied state"
        );
    }
    assert_eq!(state["network"]["applied"], serde_json::json!(true));
    // The list form, for an operator who does not know which reconciler to
    // look under.
    assert_eq!(
        state["configuration"]["refused"][0]["document"],
        serde_json::json!(micad_settings::WIFI_DOCUMENT)
    );
}

/// **The write half.** An unrelated write must not overwrite the refused
/// document, and a write that reaches it must repair it.
#[tokio::test]
pub(super) async fn an_unrelated_write_preserves_a_refused_document_and_a_reaching_one_repairs_it()
{
    let dir = tempfile::tempdir().expect("tempdir");
    let poured = r#"{"schema_version": 1, "wifi": {"ap": {"mode": "alwys"}}}"#;
    let document = pour(&dir, micad_settings::WIFI_DOCUMENT, poured);
    let (service, calls, refusals) = service_over(&dir).await;
    assert_eq!(refusals.len(), 1);

    service
        .write_setting("hostname", serde_json::json!("edge-1"))
        .await
        .expect("write the hostname");
    assert_eq!(
        std::fs::read_to_string(&document).expect("read back"),
        poured,
        "an unrelated write must leave the refused document byte identical"
    );
    let (_, state) = service.trees().await;
    assert_eq!(
        state["configuration"]["refused"]
            .as_array()
            .map(Vec::len)
            .unwrap_or_default(),
        1,
        "the refusal stands until the document is repaired"
    );

    calls.lock().expect("call log").clear();
    service
        .write_setting("wifi.ap.ssid", serde_json::json!("repaired"))
        .await
        .expect("repair the document");

    let repaired: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&document).expect("read back"))
            .expect("the repaired document parses");
    assert_eq!(
        repaired["wifi"]["ap"]["ssid"],
        serde_json::json!("repaired")
    );
    let (_, state) = service.trees().await;
    assert!(
        state["configuration"]["refused"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "the repair clears the refusal: {:?}",
        state["configuration"]
    );
    // The write's own scope is `wifi.ap.ssid`, which the existing dot-path
    // scoping narrows to the AP reconciler; what matters is that it ran at
    // all, because a standing refusal would have skipped it.
    assert_eq!(*calls.lock().expect("call log"), vec!["wifiAp".to_string()]);
    // And the full reconcile that follows reaches both halves of the
    // document, which is the refusal being over rather than merely narrowed.
    calls.lock().expect("call log").clear();
    service.apply_all().await;
    let mut ran = calls.lock().expect("call log").clone();
    ran.sort();
    assert_eq!(
        ran,
        vec![
            "hostname".to_string(),
            "network".to_string(),
            "sshd".to_string(),
            "wifiAp".to_string(),
            "wifiClient".to_string()
        ],
        "after the repair every reconciler runs again"
    );
}

/// Every shipped reconciler is backed by exactly one `/mica/config/`
/// document.
#[test]
pub(super) fn every_reconciler_is_backed_by_exactly_one_config_document() {
    for reconciler in [micad_settings::Init::Systemd, micad_settings::Init::Openrc]
        .into_iter()
        .flat_map(crate::reconciler::all)
    {
        let backing: Vec<&str> = micad_settings::DOCUMENT_SUBTREES
            .iter()
            .filter(|(_, subtrees)| {
                subtrees
                    .iter()
                    .any(|subtree| paths_overlap(subtree, reconciler.subtree()))
            })
            .map(|(document, _)| *document)
            .collect();
        assert_eq!(
            backing.len(),
            1,
            "{} ({}) is backed by {backing:?}",
            reconciler.name(),
            reconciler.subtree()
        );
    }
}

// ---- Automatic and manual paths ------------------------------------

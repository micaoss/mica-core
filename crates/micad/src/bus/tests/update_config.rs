//! The update configuration and the native deployment state.

use super::super::record_policy_action;
use crate::update_lifecycle::Refusal;
use serde_json::Value;
use std::sync::Arc;

use super::*;

/// The write route's whole point: micad is the file's only writer, and a
/// patch changes what it names and nothing else.
#[tokio::test]
pub(super) async fn the_write_route_merges_a_patch_and_the_next_state_read_reports_it() {
    let (service, _calls, _deployment_calls, dir) =
        service_with_deployments(MockDeployments::default());
    let policy_path = dir.path().join("updates.json");
    std::fs::write(
        &policy_path,
        r#"{ "policy": "check", "source": { "url": "https://old.example/update/" } }"#,
    )
    .expect("seed policy");
    let service = service.with_update(
        Arc::new(crate::update_lifecycle::NoClient),
        crate::update_policy::PolicyStore::at(policy_path.clone()),
    );

    let saved = service
        .update_handle()
        .write_config(
            ":1.5",
            r#"{ "source": { "url": "https://updates.example/update/" } }"#,
        )
        .await
        .expect("a well-formed patch is written");

    assert_eq!(saved["source"]["url"], "https://updates.example/update/");
    assert_eq!(saved["policy"], "check", "an unnamed key is left alone");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&policy_path).unwrap()).unwrap();
    assert_eq!(on_disk["source"]["url"], "https://updates.example/update/");
    // The snapshot is retaken on the write, so an operator polling the
    // state reads what they just set rather than what the last action saw.
    let recorded = service.trees().await.1;
    assert_eq!(
        recorded["update"]["lifecycle"]["policy"]["sourceUrl"],
        "https://updates.example/update/"
    );
}

/// The rule fires at the API, which is the difference between telling an
/// operator now and a device failing closed some hours later.
#[tokio::test]
pub(super) async fn the_write_route_refuses_auto_without_a_window_and_replaces_nothing() {
    let (service, _calls, _deployment_calls, dir) =
        service_with_deployments(MockDeployments::default());
    let policy_path = dir.path().join("updates.json");
    std::fs::write(&policy_path, r#"{ "policy": "check" }"#).expect("seed policy");
    let before = std::fs::read_to_string(&policy_path).unwrap();
    let service = service.with_update(
        Arc::new(crate::update_lifecycle::NoClient),
        crate::update_policy::PolicyStore::at(policy_path.clone()),
    );

    let refused = service
        .update_handle()
        .write_config(":1.5", r#"{ "policy": "auto" }"#)
        .await
        .expect_err("`auto` with no window is refused on write");

    assert!(
        matches!(refused, Refusal::Invalid(_)),
        "an InvalidArgs, which apid answers 422: {}",
        refused.message()
    );
    assert!(
        refused.message().contains("maintenance window"),
        "the refusal names the rule: {}",
        refused.message()
    );
    assert_eq!(std::fs::read_to_string(&policy_path).unwrap(), before);
}

/// The address is the operator's; what the device will accept is not.
#[tokio::test]
pub(super) async fn the_write_route_refuses_a_trust_anchor_by_name() {
    let (service, _calls, _deployment_calls, dir) =
        service_with_deployments(MockDeployments::default());
    let policy_path = dir.path().join("updates.json");
    let service = service.with_update(
        Arc::new(crate::update_lifecycle::NoClient),
        crate::update_policy::PolicyStore::at(policy_path.clone()),
    );

    let refused = service
        .update_handle()
        .write_config(
            ":1.5",
            r#"{ "source": { "url": "https://x/", "keyring": "/k" } }"#,
        )
        .await
        .expect_err("an anchor-shaped key is not writable");

    assert!(
        matches!(refused, Refusal::Invalid(_)),
        "{}",
        refused.message()
    );
    assert!(
        refused.message().contains("keyring"),
        "the refusal names the key: {}",
        refused.message()
    );
    assert!(!policy_path.exists(), "nothing was written");
}

/// A document that does not load is refused the way every other action on
/// it is refused — 409, not 422 — because it is the file that is wrong.
#[tokio::test]
pub(super) async fn the_write_route_refuses_to_patch_over_a_document_that_does_not_load() {
    let (service, _calls, _deployment_calls, dir) =
        service_with_deployments(MockDeployments::default());
    let policy_path = dir.path().join("updates.json");
    std::fs::write(&policy_path, "{not json").expect("seed policy");
    let service = service.with_update(
        Arc::new(crate::update_lifecycle::NoClient),
        crate::update_policy::PolicyStore::at(policy_path.clone()),
    );

    let refused = service
        .update_handle()
        .write_config(":1.5", r#"{ "policy": "off" }"#)
        .await
        .expect_err("there is no base to merge over");

    assert!(
        matches!(refused, Refusal::Policy(_)),
        "{}",
        refused.message()
    );
    assert_eq!(std::fs::read_to_string(&policy_path).unwrap(), "{not json");
}

/// An action the policy took names `policy`, in the same ring and
/// under the same event names an operator's action lands in.
#[test]
pub(super) fn an_automatic_action_is_audited_under_the_policy_actor() {
    let dir = tempfile::tempdir().expect("tempdir");
    record_policy_action(dir.path(), micad_settings::UPDATE_CHECK_EVENT);

    let contents =
        std::fs::read_to_string(dir.path().join(micad_settings::AUDIT_LOG)).expect("a line");
    let line: Value = serde_json::from_str(contents.trim()).expect("JSONL");
    assert_eq!(line["event"], "update-check");
    assert_eq!(line["outcome"], "requested");
    assert_eq!(line["actor"], "policy");
    assert_eq!(
        line["source"], "auto-update",
        "the name the lifecycle already records this driver's requests under"
    );
}

#[tokio::test]
pub(super) async fn native_candidate_state_and_lifecycle_are_refreshed_together() {
    let (service, _, _, _) = service_with_deployments(MockDeployments {
        status: pending_status(),
        ..MockDeployments::default()
    });
    let value: Value =
        serde_json::from_str(&service.refresh_update_state().await.unwrap()).unwrap();
    assert_eq!(value["lifecycle"]["state"], "reboot-required");
    assert_eq!(value["state"]["candidate"], "e".repeat(64));
    assert_eq!(value["rollback"]["reason"], "candidate_pending");
    assert!(value.get("slots").is_none());
}

#[tokio::test]
pub(super) async fn update_state_is_queried_recorded_and_returned() {
    let (service, _, _, _) = service_with_deployments(MockDeployments::default());
    let value: Value =
        serde_json::from_str(&service.refresh_update_state().await.unwrap()).unwrap();
    assert_eq!(value["state"]["current"], "a".repeat(64));
    assert_eq!(value["state"]["fallback"], "b".repeat(64));
    assert_eq!(value["state"]["highestGeneration"], 2);
    assert_eq!(value["boot"]["kernelId"], "c".repeat(64));
    assert_eq!(value["rollback"]["permitted"], true);
    assert_eq!(value["lifecycle"]["state"], "succeeded");
    let recorded: Value =
        serde_json::from_str(&service.get_state("update").await.unwrap()).unwrap();
    assert_eq!(recorded, value);
}

#[tokio::test]
pub(super) async fn an_unreachable_native_fails_the_query_and_records_nothing() {
    let (service, _calls, _deployment_calls, _dir) = service_with_deployments(MockDeployments {
        queries_fail: true,
        ..MockDeployments::default()
    });

    service
        .refresh_update_state()
        .await
        .expect_err("an unreachable installer is the caller's error, not a silent {}");
    assert!(
        service.get_state("update").await.is_err(),
        "a failed query must leave no half-recorded entry"
    );
}

#[tokio::test]
pub(super) async fn native_actions_validate_identity_and_record_the_operator() {
    let (service, _, calls, _) = service_with_deployments(MockDeployments::default());
    for (action, id) in [
        ("confirm", "a".repeat(63)),
        ("activate", "a".repeat(64)),
        ("confirm", "b".repeat(64)),
    ] {
        assert!(
            service
                .request_deployment_action(":1.2", action, &id)
                .await
                .is_err()
        );
    }
    assert!(calls.lock().unwrap().is_empty());
    service
        .request_deployment_action(":1.2", "confirm", &"a".repeat(64))
        .await
        .unwrap();
    service
        .request_deployment_action(":1.3", "reject", &"e".repeat(64))
        .await
        .unwrap();
    service
        .request_deployment_action(":1.4", "rollback", &"a".repeat(64))
        .await
        .unwrap();
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            "confirm".to_owned(),
            format!("reject {}", "e".repeat(64)),
            "rollback".into()
        ]
    );
    let status: Value = serde_json::from_str(&service.get_state("update").await.unwrap()).unwrap();
    assert_eq!(status["last_action"]["requested_by"], ":1.4");
    assert_eq!(status["last_action"]["action"], "rollback");
}

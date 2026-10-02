//! Installing an update and the gates in front of it.

use std::sync::Arc;

use super::*;

#[tokio::test]
pub(super) async fn an_install_runs_in_the_background_and_records_its_lifecycle() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let (service, _calls, deployment_calls, dir) = service_with_deployments(MockDeployments {
        install_gate: Some(Arc::clone(&gate)),
        ..MockDeployments::default()
    });
    let descriptor = verified_descriptor(&dir, &format!("{}.json", "a".repeat(64)));
    let descriptor = descriptor.as_str();

    // Returns while the install is still gated: the bus call cannot be
    // blocked by a slow installer.
    service
        .request_install(":1.6", descriptor)
        .await
        .expect("install");
    wait_for_install_status(&service, "running").await;

    // A second install while one runs is refused, and the refusal names
    // the reason rather than queueing silently.
    let busy = service
        .request_install(":1.7", descriptor)
        .await
        .expect_err("concurrent install must be refused");
    assert!(busy.to_string().contains("already running"), "{busy}");

    gate.notify_one();
    wait_for_install_status(&service, "done").await;

    let install = service.get_state("update.install").await.expect("state");
    let install: serde_json::Value = serde_json::from_str(&install).expect("json");
    assert_eq!(install["requested_by"], ":1.6");
    assert_eq!(install["deploymentId"], "a".repeat(64));
    assert_eq!(
        *deployment_calls.lock().expect("lock"),
        vec![format!("install {descriptor}")],
        "exactly the admitted install reached the installer"
    );
    // The completed install refreshed the whole update entry.
    let update = service.get_state("update").await.expect("state");
    let update: serde_json::Value = serde_json::from_str(&update).expect("json");
    assert_eq!(update["state"]["current"], "a".repeat(64));

    // The in-flight flag is released: a new install is admitted again.
    gate.notify_one();
    service
        .request_install(":1.8", descriptor)
        .await
        .expect("install");
    wait_for_install_status(&service, "done").await;
}

#[tokio::test]
pub(super) async fn a_failed_install_records_the_error_and_releases_the_flag() {
    let (service, _calls, _deployment_calls, dir) = service_with_deployments(MockDeployments {
        install_error: Some("signature verification failed".to_string()),
        ..MockDeployments::default()
    });
    let descriptor = verified_descriptor(&dir, &format!("{}.json", "a".repeat(64)));
    let descriptor = descriptor.as_str();

    service
        .request_install(":1.9", descriptor)
        .await
        .expect("admitted");
    wait_for_install_status(&service, "failed").await;

    let install = service.get_state("update.install").await.expect("state");
    let install: serde_json::Value = serde_json::from_str(&install).expect("json");
    assert!(
        install["error"]
            .as_str()
            .is_some_and(|err| err.contains("signature verification failed")),
        "the failure reason must be recorded, got {install}"
    );
    // The failure code, driven through the real install path rather than
    // asserted at the classifier: the native backend's sentence stays in `error` and its
    // class is beside it, so a fleet groups signature refusals without
    // matching on the native backend's words.
    assert_eq!(
        install["error_code"],
        crate::update_codes::CLIENT_EXIT_FAILURE,
        "got {install}"
    );
    service
        .request_install(":1.9", descriptor)
        .await
        .expect("flag released");
}

// The same path with a failure this repository has NOT measured: the code
// is `unknown` and it is NOT the sentence. This is the half of the gate
// that a mapping with a pass-through fallback would silently fail —
// there, `error_code` would read `Compatible mismatch: …` and a consumer
// matching on codes would be back to matching on text without being told.

#[tokio::test]
pub(super) async fn an_install_mid_flight_refuses_a_reboot_until_it_finishes() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let (service, power_calls, _deployment_calls, dir) =
        service_with_deployments(MockDeployments {
            install_gate: Some(Arc::clone(&gate)),
            ..MockDeployments::default()
        });
    let descriptor = std::path::PathBuf::from(verified_descriptor(
        &dir,
        &format!("{}.json", "a".repeat(64)),
    ));
    service
        .request_install(":1.6", descriptor.to_str().expect("utf-8"))
        .await
        .expect("install");
    wait_for_install_status(&service, "running").await;

    let refused = service
        .request_reboot(":1.7")
        .await
        .expect_err("the gate must refuse a reboot mid-install");
    assert!(refused.to_string().contains("install"), "{refused}");
    assert!(
        power_calls.lock().expect("lock").is_empty(),
        "a refused reboot must not reach the power control"
    );
    // No override lifts the install block.
    service
        .update_handle()
        .set_reboot_override(":1.7", 60)
        .await
        .expect("override armed");
    assert!(service.request_reboot(":1.7").await.is_err());

    gate.notify_one();
    wait_for_install_status(&service, "done").await;
    service.request_reboot(":1.7").await.expect("gate reopened");
    assert_eq!(*power_calls.lock().expect("lock"), vec!["reboot"]);
}

#[tokio::test]
pub(super) async fn a_blocking_health_report_refuses_a_reboot_until_overridden() {
    let (service, power_calls, _dir) = service_with_mock();
    service
        .report_health("exporter", "blocking", "mid-transaction")
        .await
        .expect("report");
    // `degraded` — mica-health's disk-pressure report — must NOT block.
    service
        .report_health("var", "degraded", "/var at 91% of capacity")
        .await
        .expect("report");

    let refused = service
        .request_reboot(":1.4")
        .await
        .expect_err("a blocking report closes the gate");
    assert!(refused.to_string().contains("exporter"), "{refused}");
    assert!(power_calls.lock().expect("lock").is_empty());

    // The reporter clearing its status reopens the gate without any
    // override — the ordinary path.
    service
        .report_health("exporter", "ok", "flushed")
        .await
        .expect("report");
    service.request_reboot(":1.4").await.expect("reopened");

    // And the bounded override lifts a standing block, audited.
    service
        .report_health("exporter", "blocking", "mid-transaction")
        .await
        .expect("report");
    assert!(service.request_reboot(":1.4").await.is_err());
    service
        .update_handle()
        .set_reboot_override(":1.4", 120)
        .await
        .expect("override armed");
    service.request_reboot(":1.4").await.expect("overridden");
    assert_eq!(*power_calls.lock().expect("lock"), vec!["reboot", "reboot"]);
}

#[tokio::test]
pub(super) async fn an_invalid_policy_file_fails_installs_closed() {
    let (service, _calls, deployment_calls, dir) =
        service_with_deployments(MockDeployments::default());
    let policy_path = dir.path().join("updates.json");
    std::fs::write(&policy_path, "{not json").expect("seed policy");
    let service = service.with_update(
        Arc::new(crate::update_lifecycle::NoClient),
        crate::update_policy::PolicyStore::at(policy_path),
    );
    let descriptor = std::path::PathBuf::from(verified_descriptor(
        &dir,
        &format!("{}.json", "a".repeat(64)),
    ));

    let refused = service
        .request_install(":1.5", descriptor.to_str().expect("utf-8"))
        .await
        .expect_err("an unreadable policy file must fail closed");
    assert!(refused.to_string().contains("invalid"), "{refused}");
    assert!(
        deployment_calls.lock().expect("lock").is_empty(),
        "the refused install must not reach the installer"
    );
}

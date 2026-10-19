//! Reboot, power-off and install admission.

use super::super::{MicadService, fdo};
use std::time::Duration;

use super::*;

/// Poll `update.install.status` until it reads `want` or ~2s elapse.
pub(super) async fn wait_for_install_status(service: &MicadService, want: &str) {
    for _ in 0..200 {
        let state = service
            .get_state("update.install.status")
            .await
            .unwrap_or_default();
        if state == format!("\"{want}\"") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "update.install.status never became \"{want}\"; state: {}",
        service.get_state("").await.unwrap_or_default()
    );
}

#[tokio::test]
pub(super) async fn reboot_reaches_the_power_control_and_is_recorded_first() {
    let (service, calls, _dir) = service_with_mock();

    service.request_reboot(":1.7").await.expect("reboot");

    assert_eq!(*calls.lock().expect("lock"), vec!["reboot".to_string()]);
    let state = service.get_state("power").await.expect("power state");
    let state: serde_json::Value = serde_json::from_str(&state).expect("json");
    assert_eq!(state["last_action"], "reboot");
    assert_eq!(state["requested_by"], ":1.7");
}

#[tokio::test]
pub(super) async fn power_off_reaches_the_power_control_and_is_recorded_first() {
    let (service, calls, _dir) = service_with_mock();

    service.request_power_off(":1.9").await.expect("power off");

    assert_eq!(*calls.lock().expect("lock"), vec!["power_off".to_string()]);
    let state = service.get_state("power").await.expect("power state");
    let state: serde_json::Value = serde_json::from_str(&state).expect("json");
    assert_eq!(state["last_action"], "power_off");
    assert_eq!(state["requested_by"], ":1.9");
}

#[tokio::test]
pub(super) async fn a_reboot_into_a_pending_deployment_records_the_warning_first() {
    let (service, calls, _deployment_calls, _dir) = service_with_deployments(MockDeployments {
        status: pending_status(),
        ..MockDeployments::default()
    });

    service.request_reboot(":1.4").await.expect("reboot");

    assert_eq!(*calls.lock().expect("lock"), vec!["reboot".to_string()]);
    let power = service.get_state("power").await.expect("power state");
    let power: serde_json::Value = serde_json::from_str(&power).expect("json");
    assert_eq!(power["last_action"], "reboot");
    let warning = power["update_warning"]
        .as_str()
        .expect("a pending deployment must put update_warning beside the action");
    assert!(warning.contains(&"e".repeat(64)), "warning: {warning}");
    assert!(warning.contains("awaits reboot"), "warning: {warning}");
}

#[tokio::test]
pub(super) async fn a_converged_system_reboots_without_an_update_warning() {
    let (service, calls, _deployment_calls, _dir) = service_with_deployments(MockDeployments {
        ..MockDeployments::default()
    });

    service.request_reboot(":1.4").await.expect("reboot");

    assert_eq!(*calls.lock().expect("lock"), vec!["reboot".to_string()]);
    let power = service.get_state("power").await.expect("power state");
    let power: serde_json::Value = serde_json::from_str(&power).expect("json");
    assert!(
        power.get("update_warning").is_none(),
        "no warning means no key, not an empty one: {power}"
    );
}

#[tokio::test]
pub(super) async fn an_unreachable_native_does_not_block_the_reboot() {
    // An unavailable native backend: the deployment query fails, the reboot
    // still goes through, and no warning is invented.
    let (service, calls, _deployment_calls, _dir) = service_with_deployments(MockDeployments {
        queries_fail: true,
        ..MockDeployments::default()
    });

    service.request_reboot(":1.4").await.expect("reboot");

    assert_eq!(*calls.lock().expect("lock"), vec!["reboot".to_string()]);
    let power = service.get_state("power").await.expect("power state");
    let power: serde_json::Value = serde_json::from_str(&power).expect("json");
    assert!(power.get("update_warning").is_none(), "got: {power}");
}

#[tokio::test]
pub(super) async fn install_admission_rejects_paths_outside_the_verified_descriptor_namespace() {
    let (service, _, calls, dir) = service_with_deployments(MockDeployments::default());
    let good = verified_descriptor(&dir, &format!("{}.json", "a".repeat(64)));
    let verified = std::path::Path::new(&good).parent().unwrap();
    let link = verified.join(format!("{}.json", "b".repeat(64)));
    std::os::unix::fs::symlink(&good, &link).unwrap();
    let outside = dir.path().join(format!("{}.json", "c".repeat(64)));
    std::fs::write(&outside, b"unverified").unwrap();
    let missing = verified.join(format!("{}.json", "d".repeat(64)));
    let partial = verified_descriptor(&dir, "partial.json.partial");
    for path in [
        "relative.json",
        dir.path().to_str().unwrap(),
        link.to_str().unwrap(),
        outside.to_str().unwrap(),
        missing.to_str().unwrap(),
        &partial,
    ] {
        assert!(
            matches!(
                service.request_install(":1.5", path).await,
                Err(fdo::Error::InvalidArgs(_))
            ),
            "{path}"
        );
    }
    assert!(calls.lock().unwrap().is_empty());
}

//! The factory reset: its refusals, the password and its backoff, and the
//! request it stages before the reboot.

use super::super::{ResetFault, fdo};
use super::*;
use std::time::Duration;

const PASSWORD: &str = "correct horse battery";

/// A claimed device whose runkit carries out a reset, with `native` as its
/// deployments.
fn claimed(native: MockDeployments) -> (MicadService, CallLog, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = micad_settings::Settings::default();
    settings.access.web_admin = Some(micad_settings::WebAdminSettings {
        password_hash: micad_settings::hash_password(PASSWORD).unwrap(),
    });
    let calls = Arc::new(Mutex::new(Vec::new()));
    std::fs::create_dir_all(dir.path().join("data/meta")).unwrap();
    std::fs::create_dir_all(dir.path().join("run")).unwrap();
    std::fs::write(
        dir.path().join("run/runkit.json"),
        r#"{"factoryReset":true}"#,
    )
    .unwrap();
    let service = MicadService::new(
        store_in(&dir),
        settings,
        Vec::new(),
        Box::new(MockPower {
            calls: Arc::clone(&calls),
        }),
        dir.path().join("shadow"),
        serde_json::json!({}),
    )
    .with_deployments(Arc::new(native))
    .with_factory_reset(Arc::new(crate::factory_reset::at(dir.path())));
    (service, calls, dir)
}

fn code(fault: ResetFault) -> String {
    match fault {
        ResetFault::Refused(json) => {
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["code"]
                .as_str()
                .unwrap()
                .to_string()
        }
        ResetFault::Fdo(error) => panic!("not a refusal: {error}"),
    }
}

fn staged(dir: &tempfile::TempDir) -> bool {
    mica_deploy::factory_reset::pending(&dir.path().join("data/meta"))
}

#[tokio::test]
pub(super) async fn the_administrator_password_stages_the_reset_and_the_reboot_follows() {
    let (service, calls, dir) = claimed(MockDeployments::default());

    service
        .request_factory_reset(":1.5", PASSWORD.into(), "console", "192.0.2.7")
        .await
        .unwrap();

    assert!(staged(&dir));
    let request: serde_json::Value = serde_json::from_slice(
        &std::fs::read(dir.path().join("data/meta/factory-reset.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(request["by"], "console");
    assert_eq!(request["source"], "192.0.2.7");
    let power: serde_json::Value =
        serde_json::from_str(&service.get_state("power").await.unwrap()).unwrap();
    assert_eq!(power["last_action"], "factory_reset");
    // The answer goes out before the reboot.
    assert!(calls.lock().unwrap().is_empty());
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(*calls.lock().unwrap(), vec!["reboot".to_string()]);
}

#[tokio::test]
pub(super) async fn a_wrong_password_stages_nothing_and_repeated_ones_back_off() {
    let (service, calls, dir) = claimed(MockDeployments::default());

    for _ in 0..3 {
        let fault = service
            .request_factory_reset(":1.5", "guess".into(), "api", "192.0.2.9")
            .await
            .unwrap_err();
        assert_eq!(code(fault), "password_incorrect");
    }
    let fault = service
        .request_factory_reset(":1.5", "guess".into(), "api", "192.0.2.9")
        .await
        .unwrap_err();
    assert_eq!(code(fault), "password_incorrect");
    // Backed off: even the right password waits.
    let fault = service
        .request_factory_reset(":1.5", PASSWORD.into(), "api", "192.0.2.9")
        .await
        .unwrap_err();
    assert_eq!(code(fault), "login_throttled");
    assert!(!staged(&dir));
    assert!(calls.lock().unwrap().is_empty());
    // The wait over.
    service.factory_reset.as_ref().unwrap().succeeded();

    service
        .request_factory_reset(":1.5", PASSWORD.into(), "api", "192.0.2.9")
        .await
        .unwrap();
    assert!(staged(&dir));
}

#[tokio::test]
pub(super) async fn an_unclaimed_device_has_nothing_to_reset() {
    let (service, _calls, dir) = claimed(MockDeployments::default());
    service.inner.write().await.settings.access.web_admin = None;

    let fault = service
        .request_factory_reset(":1.5", PASSWORD.into(), "cli", "uid 0")
        .await
        .unwrap_err();
    assert_eq!(code(fault), "not_claimed");
    assert!(!staged(&dir));
}

#[tokio::test]
pub(super) async fn a_runkit_without_the_reset_refuses_it() {
    let (service, _calls, dir) = claimed(MockDeployments::default());
    std::fs::remove_file(dir.path().join("run/runkit.json")).unwrap();

    let fault = service
        .request_factory_reset(":1.5", PASSWORD.into(), "cli", "uid 0")
        .await
        .unwrap_err();
    assert_eq!(code(fault), "factory_reset_unsupported");
    assert!(!staged(&dir));
}

#[tokio::test]
pub(super) async fn an_update_or_a_trial_comes_first() {
    // An installed deployment awaiting its reboot.
    let (service, _calls, dir) = claimed(MockDeployments {
        status: pending_status(),
        ..MockDeployments::default()
    });
    let fault = service
        .request_factory_reset(":1.5", PASSWORD.into(), "cli", "uid 0")
        .await
        .unwrap_err();
    assert_eq!(code(fault), "update_in_progress");
    assert!(!staged(&dir));

    // A running deployment awaiting its confirmation.
    let mut trial = pending_status();
    trial.boot.deployment_id = trial.state.candidate.clone().unwrap();
    let (service, _calls, dir) = claimed(MockDeployments {
        status: trial,
        ..MockDeployments::default()
    });
    let fault = service
        .request_factory_reset(":1.5", PASSWORD.into(), "cli", "uid 0")
        .await
        .unwrap_err();
    assert_eq!(code(fault), "trial_pending");
    assert!(!staged(&dir));

    // An update transaction holding DATA's lock.
    let (service, _calls, dir) = claimed(MockDeployments::default());
    let lock = std::fs::File::create(dir.path().join("data/meta/transaction.lock")).unwrap();
    lock.try_lock().unwrap();
    let fault = service
        .request_factory_reset(":1.5", PASSWORD.into(), "cli", "uid 0")
        .await
        .unwrap_err();
    assert_eq!(code(fault), "update_in_progress");
    drop(lock);
    service
        .request_factory_reset(":1.5", PASSWORD.into(), "cli", "uid 0")
        .await
        .unwrap();
    assert!(staged(&dir));
}

#[tokio::test]
pub(super) async fn the_status_says_why_a_reset_cannot_run() {
    let (service, _calls, _dir) = claimed(MockDeployments::default());
    let status: serde_json::Value =
        serde_json::from_str(&service.get_factory_reset().await.unwrap()).unwrap();
    assert_eq!(
        status,
        serde_json::json!({ "supported": true, "refusal": null, "last": null })
    );

    service.inner.write().await.settings.access.web_admin = None;
    let status: serde_json::Value =
        serde_json::from_str(&service.get_factory_reset().await.unwrap()).unwrap();
    assert_eq!(status["refusal"]["code"], "not_claimed");
}

#[tokio::test]
pub(super) async fn a_requester_outside_the_three_is_refused_as_an_argument() {
    let (service, _calls, dir) = claimed(MockDeployments::default());
    let fault = service
        .request_factory_reset(":1.5", PASSWORD.into(), "ble", "")
        .await
        .unwrap_err();
    assert!(matches!(fault, ResetFault::Fdo(fdo::Error::InvalidArgs(_))));
    assert!(!staged(&dir));
}

#[tokio::test]
pub(super) async fn the_storage_status_says_the_factory_reset_is_supported() {
    let (service, _calls, dir) = claimed(MockDeployments::default());
    let service = service.with_storage_status(Arc::new(FixedStorage(data_evidence(25_000_000))));
    let lifecycle = |status: String| {
        serde_json::from_str::<serde_json::Value>(&status).unwrap()["lifecycle"].clone()
    };
    let status = lifecycle(service.get_storage_status().await.unwrap());
    assert_eq!(status["factoryReset"], "supported");
    assert_eq!(status["secureErase"], "unsupported");

    std::fs::remove_file(dir.path().join("run/runkit.json")).unwrap();
    let status = lifecycle(service.get_storage_status().await.unwrap());
    assert_eq!(status["factoryReset"], "unsupported");
}

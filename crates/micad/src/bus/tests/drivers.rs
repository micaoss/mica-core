//! The owner certificate and driver extension changes: who may ask, and the
//! uploads they take.

use super::super::{DriversFault, fdo};
use super::*;

const PASSWORD: &str = "correct horse battery";

/// A device claimed when `claimed`, whose drivers are a mica-deploy that is
/// never run here.
fn device(claimed: bool) -> (MicadService, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = micad_settings::Settings::default();
    if claimed {
        settings.access.web_admin = Some(micad_settings::WebAdminSettings {
            password_hash: micad_settings::hash_password(PASSWORD).unwrap(),
        });
    }
    let service = MicadService::new(
        store_in(&dir),
        settings,
        Vec::new(),
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        dir.path().join("shadow"),
        serde_json::json!({}),
    )
    .with_update_workspace(dir.path().join("updates"))
    .with_drivers(Arc::new(crate::drivers::Drivers::new(
        dir.path().join("absent"),
    )));
    (service, dir)
}

fn code(fault: DriversFault) -> String {
    match fault {
        DriversFault::Refused(json) => {
            serde_json::from_str::<serde_json::Value>(&json).unwrap()["code"]
                .as_str()
                .unwrap()
                .to_string()
        }
        DriversFault::Fdo(error) => panic!("not a refusal: {error}"),
    }
}

#[tokio::test]
pub(super) async fn an_unclaimed_device_changes_nothing() {
    let (service, _dir) = device(false);
    let fault = service
        .admit_drivers_change(":1.5", "api", "192.0.2.9", "test", PASSWORD.into())
        .await
        .unwrap_err();
    assert_eq!(code(fault), "not_claimed");
}

#[tokio::test]
pub(super) async fn the_password_is_asked_again_and_wrong_ones_back_off_with_the_reset_s() {
    let (service, _dir) = device(true);
    service
        .admit_drivers_change(":1.5", "console", "192.0.2.9", "test", PASSWORD.into())
        .await
        .unwrap();
    for _ in 0..4 {
        let fault = service
            .admit_drivers_change(":1.5", "api", "192.0.2.9", "test", "guess".into())
            .await
            .unwrap_err();
        assert_eq!(code(fault), "password_incorrect");
    }
    // One counter: the factory reset is backed off too.
    assert_eq!(
        service
            .password_backoff
            .throttled()
            .map(|refusal| refusal.code),
        Some("login_throttled")
    );
    let fault = service
        .admit_drivers_change(":1.5", "api", "192.0.2.9", "test", PASSWORD.into())
        .await
        .unwrap_err();
    assert_eq!(code(fault), "login_throttled");
    assert!(matches!(
        service
            .admit_drivers_change(":1.5", "robot", "192.0.2.9", "test", PASSWORD.into())
            .await,
        Err(DriversFault::Fdo(fdo::Error::InvalidArgs(_)))
    ));
}

#[tokio::test]
pub(super) async fn an_upload_is_a_file_directly_in_the_update_uploads() {
    let (service, dir) = device(true);
    let uploads = service.update.uploads_dir();
    std::fs::create_dir_all(&uploads).unwrap();
    let upload = uploads.join("1.micaupd");
    std::fs::write(&upload, b"MICAUPD1").unwrap();
    assert_eq!(service.upload(&upload.to_string_lossy()).unwrap(), upload);
    let elsewhere = dir.path().join("1.micaupd");
    std::fs::write(&elsewhere, b"MICAUPD1").unwrap();
    for path in [
        elsewhere.to_string_lossy().into_owned(),
        uploads.join("absent").to_string_lossy().into_owned(),
        uploads.to_string_lossy().into_owned(),
    ] {
        assert!(service.upload(&path).is_err(), "{path}");
    }
}

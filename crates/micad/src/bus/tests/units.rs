//! The user's services on the bus.

use super::*;
use crate::openrc::{Commands, FakeCommands};
use crate::reconciler::systemd::UnitControl;
use crate::user_units::{UserUnits, tests::FakeControl};

const SERVICE: &str = "[Service]\nExecStart=/srv/x\n[Install]\nWantedBy=multi-user.target\n";

fn unit() -> serde_json::Value {
    serde_json::json!({ "kind": "systemd", "text": SERVICE, "enabled": true })
}

/// A daemon with no `mica-unit` has nowhere to keep a service: a write of one
/// is refused before it is stored.
#[tokio::test]
pub(super) async fn a_service_is_refused_where_there_is_no_mica_unit() {
    let (service, _calls, _dir) = service_with_mock();

    let err = service
        .persist_setting("services.units.\"x.service\"", unit())
        .await
        .unwrap_err();

    assert!(err.to_string().contains("no /usr/sbin/mica-unit"), "{err}");
    assert!(
        service
            .inner
            .read()
            .await
            .settings
            .services
            .units
            .is_empty()
    );
}

/// On a root with `mica-unit` the write lands, the read names the unit and
/// what is there by hand, and a verb reaches only an installed unit.
#[tokio::test]
pub(super) async fn a_service_is_written_listed_and_driven() {
    let (service, _calls, dir) = service_with_mock();
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("usr/sbin")).unwrap();
    std::fs::write(root.join("usr/sbin/mica-unit"), "").unwrap();
    let commands = Arc::new(FakeCommands::default());
    commands.answer(
        "/usr/sbin/mica-unit list",
        0,
        "x.service\tenabled\nhand.service\tdisabled\n",
    );
    let control = Arc::new(FakeControl::default());
    let service = service.with_user_units(Arc::new(UserUnits::new(
        micad_settings::Init::Systemd,
        &root,
        commands as Arc<dyn Commands>,
        Arc::clone(&control) as Arc<dyn UnitControl>,
    )));

    service
        .persist_setting("services.units.\"x.service\"", unit())
        .await
        .unwrap();
    let units: serde_json::Value =
        serde_json::from_str(&service.get_units().await.unwrap()).unwrap();
    let names: Vec<(&str, bool)> = units["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| (unit["name"].as_str().unwrap(), unit["managed"] == true))
        .collect();
    assert_eq!(names, [("hand.service", false), ("x.service", true)]);

    service.restart_unit("hand.service").await.unwrap();
    let err = service.stop_unit("dbus.service").await.unwrap_err();
    assert!(matches!(err, zbus::fdo::Error::InvalidArgs(_)), "{err}");
    assert_eq!(control.calls(), ["restart hand.service"]);
}

/// An app's unit is listed with its app, and a service of its name is
/// refused.
#[tokio::test]
pub(super) async fn an_apps_unit_is_its_own() {
    let (service, _calls, dir) = service_with_mock();
    let root = dir.path().join("root");
    std::fs::create_dir_all(root.join("usr/sbin")).unwrap();
    std::fs::write(root.join("usr/sbin/mica-unit"), "").unwrap();
    std::fs::create_dir_all(root.join("mica/apps/demo/1.0")).unwrap();
    std::fs::write(
        root.join("mica/apps/demo/1.0/mica-app.json"),
        r#"{"schemaVersion":1,"name":"demo","version":"1.0","arch":["all"],
            "units":[{"systemd":"demo.service","enable":true}]}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("mica/apps/apps.json"),
        r#"{"apps":{"demo":{"current":{"version":"1.0","sha256":"0","source":"upload",
            "installedAt":"2026-10-21T00:00:00Z"}}}}"#,
    )
    .unwrap();
    let commands = Arc::new(FakeCommands::default());
    commands.answer("/usr/sbin/mica-unit list", 0, "demo.service\tenabled\n");
    let units = Arc::new(UserUnits::new(
        micad_settings::Init::Systemd,
        &root,
        commands as Arc<dyn Commands>,
        Arc::new(FakeControl::default()) as Arc<dyn UnitControl>,
    ));
    let apps = Arc::new(crate::apps::Apps::new(
        &root,
        Arc::clone(&units),
        Arc::new(crate::apps::HttpFetch),
    ));
    let service = service.with_user_units(units).with_apps(apps);

    let listed: serde_json::Value =
        serde_json::from_str(&service.get_units().await.unwrap()).unwrap();
    assert_eq!(listed["units"][0]["app"], serde_json::json!("demo"));
    let err = service
        .persist_setting("services.units.\"demo.service\"", unit())
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("unit_taken: demo.service is app demo's"),
        "{err}"
    );
}

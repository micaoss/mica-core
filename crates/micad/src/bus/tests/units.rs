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

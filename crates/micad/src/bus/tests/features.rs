//! Containers and the product's features.

use super::*;

/// The name is checked against the declared projects first, so this is not a
/// way to drive an arbitrary container through a container-shaped argument.
#[tokio::test]
pub(super) async fn an_undeclared_container_is_refused_before_any_daemon_call() {
    let (service, calls, _dir) = service_with_container(true);

    let error = service
        .start_container("dbus.service")
        .await
        .expect_err("an undeclared name must be refused");

    assert!(format!("{error}").contains("dbus.service"), "{error}");
    assert!(calls.lock().expect("call log").is_empty());
}

/// With the switch off the daemon runs nothing and may not be running; saying
/// so beats an error about a socket that is not there.
#[tokio::test]
pub(super) async fn a_container_action_is_refused_while_containers_are_switched_off() {
    let (service, calls, _dir) = service_with_container(false);

    let error = service
        .start_container("app/web")
        .await
        .expect_err("a switched-off device must refuse");

    assert!(format!("{error}").contains("container.enabled"), "{error}");
    assert!(calls.lock().expect("call log").is_empty());
}

/// A product without a feature: its members are refused by name before
/// they touch a unit, an adapter or a radio.
#[tokio::test]
pub(super) async fn the_members_of_a_feature_the_product_does_not_carry_are_refused_by_name() {
    let (service, calls, _dir) = service_with_container(true);
    let service = service.with_features(micad_settings::Features::only(&[
        micad_settings::Feature::Mqtt,
    ]));

    let error = service
        .start_container("app/web")
        .await
        .expect_err("containers are not in this product");
    assert!(
        format!("{error}").contains("feature not in this product: containers"),
        "{error}"
    );
    assert!(calls.lock().expect("call log").is_empty());
    for error in [
        format!(
            "{:?}",
            service.get_containers().await.expect_err("containers")
        ),
        format!("{:?}", service.scan_wifi().await.expect_err("wifi")),
        format!(
            "{:?}",
            service.get_bluetooth().await.expect_err("bluetooth")
        ),
        format!(
            "{:?}",
            service
                .confirm_bluetooth_pairing("AA:BB:CC:DD:EE:FF", true)
                .await
                .expect_err("bluetooth")
        ),
    ] {
        assert!(error.contains("feature not in this product"), "{error}");
    }
}

/// A settings write that would change a feature the product does not
/// carry is refused, whether it names the subtree or a tree around it, and
/// leaves the tree as it was; a write elsewhere lands.
#[tokio::test]
pub(super) async fn a_write_into_a_feature_the_product_does_not_carry_is_refused() {
    let (service, _calls, _dir) = service_with_container(true);
    let service = service.with_features(micad_settings::Features::only(&[
        micad_settings::Feature::Containers,
    ]));

    let error = service
        .persist_setting("wifi.ap.enabled", serde_json::json!(true))
        .await
        .expect_err("wifi is not in this product");
    assert!(
        matches!(
            error,
            micad_settings::SettingsError::NotServed {
                feature: micad_settings::Feature::Wifi,
                ..
            }
        ),
        "{error}"
    );
    assert!(matches!(
        crate::bus::to_bus_error(error),
        crate::bus::SettingsFault::NotFound(_)
    ));

    let mut whole = serde_json::to_value(&service.inner.read().await.settings).unwrap();
    whole["mqtt"]["enabled"] = serde_json::json!(true);
    let error = service
        .persist_setting("", whole)
        .await
        .expect_err("a whole-tree write that changes mqtt");
    assert!(
        format!("{error}").contains("feature not in this product: mqtt"),
        "{error}"
    );
    assert!(!service.inner.read().await.settings.mqtt.enabled);

    service
        .persist_setting("hostname", serde_json::json!("edge-7"))
        .await
        .expect("a write outside every feature lands");
    service
        .persist_setting("container.enabled", serde_json::json!(false))
        .await
        .expect("a write into a feature the product carries lands");
}

/// The read names both sides and merges neither: the declared projects, and
/// mica-containerd's phase and health of each of their containers.
#[tokio::test]
pub(super) async fn the_container_read_names_the_declared_projects_and_the_daemon_apart() {
    let (service, _calls, _dir) = service_with_container(true);

    let value: serde_json::Value =
        serde_json::from_str(&service.get_containers().await.expect("read"))
            .expect("container JSON");

    assert_eq!(value["enabled"], serde_json::json!(true));
    assert!(value["projects"]["app"].is_string(), "{value}");
    assert_eq!(value["engine"]["available"], serde_json::json!(true));
    assert_eq!(
        value["engine"]["projects"][0]["services"][0]["phase"],
        serde_json::json!("running")
    );

    // An absent daemon is unavailable, never an empty list.
    let service = service.with_containers(
        Arc::new(crate::containers::NoEngine),
        Arc::new(crate::containerd::NoContainerd),
    );
    let value: serde_json::Value =
        serde_json::from_str(&service.get_containers().await.expect("read"))
            .expect("container JSON");
    assert_eq!(value["engine"]["available"], serde_json::json!(false));
    assert!(value["engine"]["detail"].is_string(), "{value}");
    assert_eq!(value["containerEngine"], serde_json::json!("podman"));

    // The read names the engine the product carries.
    let service = service.with_container_engine(micad_settings::ContainerEngine::Docker);
    let value: serde_json::Value =
        serde_json::from_str(&service.get_containers().await.expect("read"))
            .expect("container JSON");
    assert_eq!(value["containerEngine"], serde_json::json!("docker"));
}

/// A service whose `wifi` tree is `wifi`, nothing else configured.
fn service_with_wifi(wifi: micad_settings::WifiSettings) -> (MicadService, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, SHADOW).expect("seed shadow");
    let settings = micad_settings::Settings {
        wifi,
        ..micad_settings::Settings::default()
    };
    let service = MicadService::new(
        store_in(&dir),
        settings,
        Vec::new(),
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        shadow_path,
        serde_json::json!({}),
    );
    (service, dir)
}

/// A scan that cannot run says why in the operator's terms, before any
/// socket is looked for: the radio is the access point's, or the station is
/// switched off.
#[tokio::test]
pub(super) async fn a_scan_without_a_station_names_what_holds_the_radio() {
    let mut wifi = micad_settings::WifiSettings::default();
    wifi.client.enabled = false;
    wifi.ap.mode = micad_settings::ApMode::Always;
    let (service, _dir) = service_with_wifi(wifi.clone());
    let scan: serde_json::Value =
        serde_json::from_str(&service.scan_wifi().await.expect("scan")).expect("json");
    assert_eq!(scan["available"], false);
    assert_eq!(scan["reason"], "access-point");
    assert!(
        scan["detail"]
            .as_str()
            .expect("detail")
            .contains("access point"),
        "{scan}"
    );

    wifi.ap.mode = micad_settings::ApMode::Off;
    let (service, _dir) = service_with_wifi(wifi);
    let scan: serde_json::Value =
        serde_json::from_str(&service.scan_wifi().await.expect("scan")).expect("json");
    assert_eq!(scan["available"], false);
    assert_eq!(scan["reason"], "client-off");
    assert!(
        scan["detail"]
            .as_str()
            .expect("detail")
            .contains("switched off"),
        "{scan}"
    );
}

/// A project's verb reaches every container of it; `<project>/<service>`
/// reaches the one mica-containerd names for the service, `db`'s being its
/// `container_name`; a service the project cannot have is refused before a
/// call.
#[tokio::test]
pub(super) async fn a_project_verb_reaches_its_containers() {
    let (service, calls, _dir) = service_with_container(true);

    service
        .stop_container("app")
        .await
        .expect("stop the project");
    service
        .restart_container("app/web")
        .await
        .expect("restart one service");
    service
        .start_container("app/db")
        .await
        .expect("start the service with a container_name");
    assert!(service.start_container("app/cache").await.is_err());
    for refused in ["other", "other/web", "app/Web", "app/../x", "app-web-1"] {
        assert!(service.start_container(refused).await.is_err(), "{refused}");
    }

    assert_eq!(
        *calls.lock().expect("call log"),
        vec![
            "stop app-web-1".to_string(),
            "stop core".to_string(),
            "restart app-web-1".to_string(),
            "start core".to_string(),
        ]
    );
}

/// A write of a project is held to mica-containerd's check, and a refused
/// one changes nothing.
#[tokio::test]
pub(super) async fn a_project_write_is_checked_and_a_refused_one_changes_nothing() {
    let (service, _calls, _dir) = service_with_container(true);

    let error = service
        .persist_setting(
            "container.projects.bad",
            serde_json::json!("services:\n  web:\n    privileged: true\n"),
        )
        .await
        .expect_err("a refused file is refused");
    assert!(
        matches!(&error, micad_settings::SettingsError::Validation { message, .. }
            if message.contains("\"bad\"") && message.contains("privileged")),
        "{error}"
    );
    assert!(
        !service
            .inner
            .read()
            .await
            .settings
            .container
            .projects
            .contains_key("bad")
    );

    service
        .persist_setting(
            "container.projects.api",
            serde_json::json!("services:\n  api:\n    image: a:1\n"),
        )
        .await
        .expect("a file that passes is stored");
    assert!(
        service
            .inner
            .read()
            .await
            .settings
            .container
            .projects
            .contains_key("api")
    );
}

/// The check answers its problems whole, for a caller to show field by field.
#[tokio::test]
pub(super) async fn the_check_answers_the_problems_whole() {
    let (service, _calls, _dir) = service_with_container(true);
    let refused: serde_json::Value = serde_json::from_str(
        &service
            .check_container_project("app", "services:\n  web:\n    privileged: true\n")
            .await
            .expect("a refusal is an answer"),
    )
    .unwrap();
    assert_eq!(refused["ok"], serde_json::json!(false));
    assert_eq!(
        refused["problems"][0]["field"],
        serde_json::json!("privileged")
    );
    assert!(service.check_container_project("App", "x").await.is_err());
}

/// A write that changes a project's directory alone is checked in it, and a
/// check runs in the directory given or else in the stored one.
#[tokio::test]
pub(super) async fn a_project_directory_is_where_its_file_is_checked() {
    let (service, _calls, _dir) = service_with_container(true);
    let text = service.inner.read().await.settings.container.projects["app"].clone();
    let error = service
        .persist_setting(
            "container.projectDirs",
            serde_json::json!({ "app": "/refused" }),
        )
        .await
        .expect_err("the file is refused in that directory");
    assert!(error.to_string().contains("/refused"), "{error}");
    service
        .persist_setting(
            "container.projectDirs",
            serde_json::json!({ "app": "/srv/app" }),
        )
        .await
        .expect("a directory the file passes in is stored");

    let check = |answer: String| serde_json::from_str::<serde_json::Value>(&answer).unwrap();
    let stored = check(service.check_container_project("app", &text).await.unwrap());
    assert_eq!(
        stored["containers"][0]["dir"],
        serde_json::json!("/srv/app")
    );
    let given = check(
        service
            .check_container_project_in("app", &text, "/srv/other")
            .await
            .unwrap(),
    );
    assert_eq!(
        given["containers"][0]["dir"],
        serde_json::json!("/srv/other")
    );
    assert!(
        service
            .check_container_project_in("app", &text, "srv/other")
            .await
            .is_err()
    );
    let read = check(service.get_containers().await.unwrap());
    assert_eq!(
        read["projectDirs"],
        serde_json::json!({ "app": "/srv/app" })
    );
}

/// A log is one declared service's, bounded in lines and bytes, the newest
/// kept.
#[tokio::test]
pub(super) async fn a_log_is_one_declared_service_bounded() {
    let (service, calls, _dir) = service_with_container(true);
    let log = service
        .get_container_log("app/web", 50)
        .await
        .expect("a declared service's log");
    assert!(log.len() <= 256 * 1024 && log.ends_with("\nlast\n"));
    assert!(service.get_container_log("app/web", 0).await.is_err());
    assert!(service.get_container_log("app/web", 1001).await.is_err());
    assert!(service.get_container_log("app", 10).await.is_err());
    assert_eq!(
        *calls.lock().expect("call log"),
        vec!["logs app-web-1 50".to_string()]
    );
}

/// On docker nothing is declared: the verbs and logs reach whatever dockerd
/// holds by its project and service, and a write or a check of a project is
/// refused with how containers are made there.
#[tokio::test]
pub(super) async fn on_docker_the_verbs_reach_dockerds_containers_and_projects_are_refused() {
    let (service, calls, _dir) = service_with_container(true);
    let service = service.with_container_engine(micad_settings::ContainerEngine::Docker);
    // Nothing declared: drop the fixture's project, as a docker product has none.
    service
        .persist_setting("container.projects", serde_json::json!({}))
        .await
        .expect("taking projects away is a write");

    service
        .restart_container("app/web")
        .await
        .expect("a compose service dockerd holds");
    service
        .stop_container("app")
        .await
        .expect("a compose project dockerd holds");
    service.container_log("app/db", 5).await.expect("its log");
    for refused in [
        "other",
        "app/cache",
        "app/../x",
        "/app",
        "a b/web",
        "-app/web",
    ] {
        assert!(service.start_container(refused).await.is_err(), "{refused}");
    }
    assert_eq!(
        *calls.lock().expect("call log"),
        vec![
            "restart app-web-1".to_string(),
            "stop app-web-1".to_string(),
            "stop core".to_string(),
            "logs core 5".to_string(),
        ]
    );

    let refused = service
        .persist_setting(
            "container.projects",
            serde_json::json!({ "app": "services:\n  web:\n    image: nginx:1\n" }),
        )
        .await
        .expect_err("a project is refused on docker");
    assert!(refused.to_string().contains("docker compose"), "{refused}");
    let refused = service
        .check_container_project("app", "services: {}\n")
        .await
        .expect_err("a check is refused on docker");
    assert!(refused.to_string().contains("docker compose"), "{refused}");
}

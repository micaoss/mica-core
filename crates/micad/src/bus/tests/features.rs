//! Containers and the product's features.

use super::*;

/// The three verbs are mica-containerd's, never podman's or the init's.
#[tokio::test]
pub(super) async fn a_container_action_is_the_daemons_verb() {
    let (service, calls, _dir) = service_with_container(true);

    service.start_container("node-red").await.expect("start");
    service
        .restart_container("node-red")
        .await
        .expect("restart");
    service.stop_container("node-red").await.expect("stop");

    assert_eq!(
        *calls.lock().expect("call log"),
        vec![
            "start node-red".to_string(),
            "restart node-red".to_string(),
            "stop node-red".to_string(),
        ]
    );
}

/// The name is checked against the declared map first, so this is not a
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
        .start_container("node-red")
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
        .start_container("node-red")
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

/// The read names both sides and merges neither: the declared map, and
/// mica-containerd's phase and health of each container.
#[tokio::test]
pub(super) async fn the_container_read_names_the_declared_map_and_the_daemon_apart() {
    let (service, _calls, _dir) = service_with_container(true);

    let value: serde_json::Value =
        serde_json::from_str(&service.get_containers().await.expect("read"))
            .expect("container JSON");

    assert_eq!(value["enabled"], serde_json::json!(true));
    assert_eq!(
        value["declared"]["node-red"]["image"],
        serde_json::json!("docker.io/nodered/node-red:4.0.9")
    );
    assert_eq!(value["engine"]["available"], serde_json::json!(true));
    assert_eq!(
        value["engine"]["entries"][0]["phase"],
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
}

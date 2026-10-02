//! The MQTT routes.

use crate::auth;
use crate::settings_api::SettingsApi;
use axum::Router;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

// A settings tree with the mqtt subtree, authenticated as `ssh_tree`.
pub(super) fn mqtt_tree(enabled: bool) -> serde_json::Value {
    let hash = auth::hash_password("hunter2secret").unwrap();
    json!({
        "hostname": "mica",
        "network": {},
        "access": { "webAdmin": { "password_hash": hash } },
        "mqtt": {
            "enabled": enabled,
            "listen": { "address": "127.0.0.1", "port": 1883 },
            "auth": { "enabled": false },
        },
    })
}

// The live-state subtree micad's mqtt reconciler publishes, copied **verbatim**
// from the reconciler's own expectation of it.
pub(super) const MQTT_PUBLISHED_STATE: &str = r#"{
    "enabled": true,
    "listen": { "address": "127.0.0.1", "port": 1883 },
    "auth": { "enabled": false },
    "configPath": "/run/mica/mqtt-broker.toml",
    "units": [
        {
            "unit": "mica-mqtt-broker.service",
            "activeState": "active",
            "unitFileState": "enabled-runtime"
        },
        {
            "unit": "mica-mqttd.service",
            "activeState": "active",
            "unitFileState": "enabled-runtime"
        }
    ]
}"#;

// The published state with both units in the state a working switch produces.
pub(super) fn mqtt_state(
    enabled: bool,
    address: &str,
    port: u64,
    auth_enabled: bool,
) -> serde_json::Value {
    let active_state = if enabled { "active" } else { "inactive" };
    mqtt_state_with_units(
        enabled,
        address,
        port,
        auth_enabled,
        active_state,
        active_state,
    )
}

// The same with both units' `activeState` chosen, each written into the entry
// that carries its own name.
pub(super) fn mqtt_state_with_units(
    enabled: bool,
    address: &str,
    port: u64,
    auth_enabled: bool,
    broker_state: &str,
    bridge_state: &str,
) -> serde_json::Value {
    let mut state: serde_json::Value =
        serde_json::from_str(MQTT_PUBLISHED_STATE).expect("the golden published state parses");
    state["enabled"] = json!(enabled);
    state["listen"]["address"] = json!(address);
    state["listen"]["port"] = json!(port);
    state["auth"]["enabled"] = json!(auth_enabled);
    set_unit_state(&mut state, "mica-mqtt-broker.service", broker_state);
    set_unit_state(&mut state, "mica-mqttd.service", bridge_state);
    state
}

// Write one `units` entry's `activeState`, found by its `unit` field.
pub(super) fn set_unit_state(state: &mut serde_json::Value, unit: &str, active_state: &str) {
    let entry = state["units"]
        .as_array_mut()
        .expect("the golden `units` is an array")
        .iter_mut()
        .find(|entry| entry["unit"] == json!(unit))
        .unwrap_or_else(|| panic!("the golden state publishes no unit named {unit}"));
    entry["activeState"] = json!(active_state);
}

// --- Bluetooth ---------------------------------------------------------------

pub(super) const MQTT_PATH: &str = "/api/v1/mqtt";

// A browser session and its CSRF proof, which every write below needs.
pub(super) async fn mqtt_session(router: &Router) -> (String, String) {
    let response = json_request(
        router,
        "POST",
        "/api/v1/session",
        json!({ "password": "hunter2secret" }),
        None,
        None,
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&response);
    let csrf = body_json(response).await["csrfToken"]
        .as_str()
        .expect("an authenticated session carries a CSRF token")
        .to_string();
    (cookie, csrf)
}

// The read answers the stored subtree and the reconciler's own document, and
// it does not merge them: the console has to be able to show a listener that
// was configured but is not the one the broker is bound to.
#[tokio::test]
pub(super) async fn the_mqtt_read_answers_the_declared_subtree_and_the_published_state() {
    let (router, fake) = test_app(mqtt_tree(true));
    fake.set_state_entry("mqtt", mqtt_state(true, "0.0.0.0", 8883, true));
    let cookie = login(&router, "hunter2secret").await;

    let body = body_json(get(&router, MQTT_PATH, Some(&cookie)).await).await;

    assert_eq!(body["configured"]["enabled"], json!(true));
    assert_eq!(body["configured"]["listen"]["address"], json!("127.0.0.1"));
    assert_eq!(body["configured"]["listen"]["port"], json!(1883));
    assert_eq!(body["configured"]["auth"]["enabled"], json!(false));
    assert_eq!(body["observed"]["available"], json!(true));
    assert_eq!(
        body["observed"]["state"]["listen"]["address"],
        json!("0.0.0.0")
    );
    assert_eq!(
        body["observed"]["state"]["units"][0]["unit"],
        json!("mica-mqtt-broker.service")
    );
}

// A reconciler that has published nothing is reported as absent, not as a
// broker at its defaults.
#[tokio::test]
pub(super) async fn the_mqtt_read_reports_an_absent_observer_rather_than_a_default_one() {
    let (router, _fake) = test_app(mqtt_tree(false));
    let cookie = login(&router, "hunter2secret").await;

    let body = body_json(get(&router, MQTT_PATH, Some(&cookie)).await).await;

    assert_eq!(body["observed"]["available"], json!(false));
    assert!(body["observed"].get("state").is_none(), "{body}");
    assert!(body["observed"]["error"].is_string(), "{body}");
}

// The listener is one decision, so the write is one write: address, port, the
// switch and the auth flag reach micad as a single `mqtt` subtree.
#[tokio::test]
pub(super) async fn the_mqtt_write_commits_the_whole_subtree_in_one_write() {
    let (router, fake) = test_app(mqtt_tree(false));
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        MQTT_PATH,
        json!({
            "enabled": true,
            "listen": { "address": "0.0.0.0", "port": 8883 },
            "auth": { "enabled": true },
        }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(fake.set_paths(), vec!["mqtt".to_string()]);
    let stored = fake.get_settings("mqtt").await.unwrap();
    assert_eq!(
        stored,
        json!({
            "enabled": true,
            "listen": { "address": "0.0.0.0", "port": 8883 },
            "auth": { "enabled": true },
        })
    );
}

// An address the broker cannot bind is refused here rather than on the device,
// where it is a unit that will not start and a console that reported success.
#[tokio::test]
pub(super) async fn the_mqtt_write_refuses_a_listener_the_broker_could_not_bind() {
    for listen in [
        json!({ "address": "not-an-address", "port": 1883 }),
        json!({ "address": "127.0.0.1", "port": 0 }),
    ] {
        let (router, fake) = test_app(mqtt_tree(false));
        let (cookie, csrf) = mqtt_session(&router).await;

        let response = json_request(
            &router,
            "PUT",
            MQTT_PATH,
            json!({ "enabled": true, "listen": listen, "auth": { "enabled": false } }),
            Some(&cookie),
            Some(&csrf),
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let error = envelope(response).await;
        assert_eq!(error["code"], "validation_failed");
        assert_eq!(error["path"], "mqtt");
        assert!(fake.set_paths().is_empty(), "a refused write reached micad");
    }
}

// A body missing a field is refused rather than merged: a `PUT` replaces the
// subtree, and the only value a missing field could take is one the client
// never sent.
#[tokio::test]
pub(super) async fn the_mqtt_write_refuses_a_partial_document() {
    let (router, fake) = test_app(mqtt_tree(false));
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "PUT",
        MQTT_PATH,
        json!({ "enabled": true }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(fake.set_paths().is_empty());
}

#[tokio::test]
pub(super) async fn the_mqtt_open_listener_warning_tracks_the_configuration_not_the_page() {
    // Three configurations that must NOT warn, so the warning means something
    // when it does appear. A pane that warned on everything would train an
    // operator to ignore it.
    let quiet = [
        // Loopback with auth off: the default, and unreachable from off-host.
        (true, "127.0.0.1", false),
        // Loopback v6, same reasoning -- the broker treats both as loopback.
        (true, "::1", false),
        // Off-host WITH auth: a deliberate, defended configuration.
        (true, "0.0.0.0", true),
    ];
    for (enabled, address, auth_enabled) in quiet {
        let (router, fake) = test_app(mqtt_tree(enabled));
        fake.set_state_entry("mqtt", mqtt_state(enabled, address, 1883, auth_enabled));
        let cookie = login(&router, "hunter2secret").await;
        let body = body_string(get(&router, "/mqtt", Some(&cookie)).await).await;
        assert!(
            !body.contains("accepts unauthenticated connections"),
            "{address} with auth={auth_enabled} must not warn: {body}"
        );
    }

    // And with the switch off there is no listener to warn about: the broker
    // is not running, so an open bind in the last published state describes
    // something that has already stopped.
    let (router, fake) = test_app(mqtt_tree(false));
    fake.set_state_entry("mqtt", mqtt_state(false, "0.0.0.0", 1883, false));
    let cookie = login(&router, "hunter2secret").await;
    let body = body_string(get(&router, "/mqtt", Some(&cookie)).await).await;
    assert!(
        !body.contains("accepts unauthenticated connections"),
        "a stopped broker must not be reported as accepting connections: {body}"
    );
}

// Every `post(...)` route registered in `routes.rs` appears in
// [`ALL_MUTATIONS`], which is what the authentication tests iterate.

// The two read-only resource roots.

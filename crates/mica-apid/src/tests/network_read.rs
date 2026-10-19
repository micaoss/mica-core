//! The observed network and the dotted interface path.

use crate::auth;
use axum::http::StatusCode;
use serde_json::json;

use super::*;

#[tokio::test]
pub(super) async fn network_api_reports_configured_and_observed_interfaces() {
    let (router, fake) = test_app(json!({
        "hostname": "mica",
        "network": { "eth0": { "dhcp": true } },
        "access": {
            "webAdmin": { "password_hash": auth::hash_password("hunter2secret").unwrap() }
        }
    }));
    fake.set_state_entry(
        "network",
        json!({
            "interfaceCount": 2,
            "interfaces": [
                {
                    "index": 1,
                    "name": "lo",
                    "operationalState": "carrier",
                    "carrierState": "carrier"
                },
                {
                    "index": 2,
                    "name": "eth0",
                    "operationalState": "routable",
                    "carrierState": "carrier",
                    "addresses": [{ "Address": [192, 0, 2, 10], "PrefixLength": 24 }]
                }
            ]
        }),
    );
    let login = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": "hunter2secret" }),
        None,
        None,
    )
    .await;
    let cookie = session_cookie_value(&login);

    let response = get(&router, "/api/v1/network", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_str(&body_string(response).await).expect("network JSON");
    assert_eq!(body["configuredCount"], 1);
    assert_eq!(body["configured"]["eth0"]["dhcp"], true);
    assert_eq!(body["observed"]["available"], true);
    assert_eq!(body["observed"]["interfaceCount"], 2);
    assert_eq!(body["observed"]["interfaces"][1]["name"], "eth0");
    assert_eq!(
        body["observed"]["interfaces"][1]["operationalState"],
        "routable"
    );
}

// Uptime reaches the pane from micad's live-state tree and from nowhere else:
// a backend with no `uptime` state renders the unavailable notice, where a
// handler that still read `/proc/uptime` for itself would render a real
// number on any Linux host.

// The reproduction, end to end: the form accepts `eth0.100`, the
// write reaches the single key `eth0.100`, and the pane reads it back.

// The path apid builds for a dotted name is a path the real settings model
// accepts — the half that lived below the fake backend.
#[test]
pub(super) fn the_dotted_iface_path_apid_builds_is_accepted_by_the_settings_model() {
    let value = json!({ "dhcp": true });

    let mut settings = micad_settings::Settings::default();
    settings
        .set(r#"network."eth0.100""#, value.clone())
        .unwrap();
    assert_eq!(
        settings.network.keys().collect::<Vec<_>>(),
        vec!["eth0.100"]
    );
    assert!(settings.network["eth0.100"].dhcp);

    // The recorded spelling: three segments, so `100` is offered as a
    // field of `IfaceSettings` and refused.
    let err = micad_settings::Settings::default().set("network.eth0.100", value);
    assert!(
        matches!(&err, Err(micad_settings::SettingsError::Validation { message, .. }) if message.contains("unknown field `100`")),
        "{err:?}"
    );
}

// SSH pane

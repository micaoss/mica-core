//! The BLE provisioning routes.

use axum::http::StatusCode;
use serde_json::json;

use super::bluetooth::{BLUETOOTH_API, bluetooth_tree};
use super::*;

// The console opens a `manual` window, reads its code, and closes it.
#[tokio::test]
pub(super) async fn a_manual_window_opens_with_its_code_and_closes() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;
    let provisioning = format!("{BLUETOOTH_API}/provisioning");

    let before = body_json(get(&router, &provisioning, Some(&cookie)).await).await;
    assert_eq!(before["state"], json!("off"));
    assert!(before.get("code").is_none());

    let opened = json_request(
        &router,
        "POST",
        &provisioning,
        json!({ "durationSeconds": 600 }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(opened.status(), StatusCode::OK);
    let opened = body_json(opened).await;
    assert_eq!(opened["state"], json!("waiting"));
    assert_eq!(opened["trigger"], json!("manual"));
    assert_eq!(opened["code"], json!("20481337"));

    let closed = json_request(
        &router,
        "DELETE",
        &provisioning,
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(closed.status(), StatusCode::NO_CONTENT);
    let after = body_json(get(&router, &provisioning, Some(&cookie)).await).await;
    assert!(after.get("code").is_none());
    assert_eq!(
        fake.update_calls(),
        ["ble provisioning open 600", "ble provisioning close"]
    );
}

// A window lasts 5, 10 or 30 minutes; anything else never reaches micad.
#[tokio::test]
pub(super) async fn a_window_of_another_length_is_refused() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let refused = json_request(
        &router,
        "POST",
        &format!("{BLUETOOTH_API}/provisioning"),
        json!({ "durationSeconds": 60 }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(refused.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body_json(refused).await["error"]["code"],
        json!("invalid_duration")
    );
    assert!(fake.update_calls().is_empty());
}

// micad's refusals of a window are 409s named by their reason.
#[tokio::test]
pub(super) async fn an_unclaimed_or_locked_device_refuses_a_window() {
    for reason in ["not_claimed", "locked"] {
        let (router, fake) = test_app(bluetooth_tree());
        let (cookie, csrf) = mqtt_session(&router).await;
        fake.refuse_updates(
            "org.freedesktop.DBus.Error.AccessDenied",
            &format!("{reason}: refused"),
        );

        let refused = json_request(
            &router,
            "POST",
            &format!("{BLUETOOTH_API}/provisioning"),
            json!({ "durationSeconds": 300 }),
            Some(&cookie),
            Some(&csrf),
        )
        .await;

        assert_eq!(refused.status(), StatusCode::CONFLICT, "{reason}");
        let error = body_json(refused).await["error"].clone();
        assert_eq!(error["code"], json!(reason));
        assert_eq!(error["message"], json!("refused"));
    }
}

// The offline trigger's switch is one settings write.
#[tokio::test]
pub(super) async fn the_offline_switch_is_one_write() {
    let (router, fake) = test_app(bluetooth_tree());
    let (cookie, csrf) = mqtt_session(&router).await;

    let written = json_request(
        &router,
        "PUT",
        &format!("{BLUETOOTH_API}/provisioning/settings"),
        json!({ "autoWhenOffline": false }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;

    assert_eq!(written.status(), StatusCode::ACCEPTED);
    assert_eq!(
        fake.set_paths(),
        vec!["bluetooth.provisioning.autoWhenOffline".to_string()]
    );
    let status = body_json(
        get(
            &router,
            &format!("{BLUETOOTH_API}/provisioning"),
            Some(&cookie),
        )
        .await,
    )
    .await;
    assert_eq!(status["autoWhenOffline"], json!(false));
}

//! `GET /api/v1/time/zones`: the zones this device's tzdata offers, and
//! whether the stored zone is one of them.

use axum::http::StatusCode;
use axum::http::header::{ETAG, IF_NONE_MATCH};
use serde_json::json;

use super::*;

const ZONES_PATH: &str = "/api/v1/time/zones";

/// A tzdata tree with `Europe/Berlin` in its table and the alias
/// `Arctic/Longyearbyen` installed beside it.
fn tzdata() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for zone in ["Europe/Berlin", "Arctic/Longyearbyen", "UTC"] {
        let path = dir.path().join(zone);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "TZif2").unwrap();
    }
    std::fs::write(
        dir.path().join("zone1970.tab"),
        "#\nDE\t+5230+01322\tEurope/Berlin\tmost of Germany\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("tzdata.zi"), "# version 2026c\n").unwrap();
    dir
}

fn zones_app(zone: &str, zoneinfo: &std::path::Path) -> (axum::Router, String) {
    let mut tree = configured_tree("hunter2secret");
    tree["time"] = json!({ "timezone": zone, "ntp": { "servers": [] } });
    let (tree, token) = with_token(tree);
    let fake = Arc::new(FakeSettings::new(tree));
    (
        app(AppState::new(fake, SIGNING_KEY).with_zoneinfo(zoneinfo)),
        token,
    )
}

#[tokio::test]
async fn the_catalog_names_the_zones_and_where_the_stored_one_stands() {
    let dir = tzdata();
    let (router, token) = zones_app("Europe/Berlin", dir.path());
    assert_eq!(
        get(&router, ZONES_PATH, None).await.status(),
        StatusCode::UNAUTHORIZED
    );

    let response = bearer(&router, "GET", ZONES_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let etag = header_value(&response, ETAG).to_string();
    assert!(etag.starts_with('"'), "{etag}");
    assert_eq!(
        body_json(response).await,
        json!({
            "tzdataVersion": "2026c",
            "zones": [
                { "id": "UTC", "region": null, "city": null },
                { "id": "Europe/Berlin", "region": "Europe", "city": "Berlin" },
            ],
            "current": { "id": "Europe/Berlin", "installed": true, "listed": true },
        })
    );

    // The same answer again is a 304 to a caller that holds it.
    let request = Request::builder()
        .uri(ZONES_PATH)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(IF_NONE_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        send(&router, request).await.status(),
        StatusCode::NOT_MODIFIED
    );

    // An alias is a zone the device accepts and does not list; a name its
    // tzdata does not carry is neither.
    for (zone, installed) in [("Arctic/Longyearbyen", true), ("Atlantis/Made_Up", false)] {
        let (router, token) = zones_app(zone, dir.path());
        let body = body_json(bearer(&router, "GET", ZONES_PATH, &token).await).await;
        assert_eq!(
            body["current"],
            json!({ "id": zone, "installed": installed, "listed": false }),
            "{zone}"
        );
    }
}

#[tokio::test]
async fn a_device_without_a_zone_table_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (router, token) = zones_app("UTC", dir.path());
    let response = bearer(&router, "GET", ZONES_PATH, &token).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        body_json(response).await["error"]["code"],
        "tzdata_unavailable"
    );
}

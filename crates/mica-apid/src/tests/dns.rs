//! The DNS module.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

// A device with one DHCP port, one static port and the DNS defaults.
fn dns_tree(records: serde_json::Value) -> serde_json::Value {
    let mut tree = configured_tree("hunter2secret");
    tree["network"] = json!({
        "eth1": { "dhcp": false, "static": { "address": "192.168.10.1/24", "dns": ["192.168.10.53"] } },
    });
    tree["dns"] =
        json!({ "global": { "enabled": false, "servers": [] }, "staticRecords": records });
    tree
}

fn observed() -> serde_json::Value {
    json!({
        "interfaces": { "available": true, "count": 7, "entries": [
            { "name": "lo", "link": { "carrier": true }, "dns": [], "addresses": [{ "address": "127.0.0.1" }] },
            { "name": "eth0", "link": { "carrier": true }, "dns": ["192.168.1.1"], "addresses": [{ "address": "192.168.1.24", "configSource": "DHCPv4" }] },
            { "name": "eth1", "link": { "carrier": true }, "dns": ["192.168.10.53"], "addresses": [{ "address": "192.168.10.1" }] },
            { "name": "eth2", "link": { "carrier": false }, "dns": [], "addresses": [], "lease": { "available": false } },
            { "name": "eth3", "link": { "carrier": true }, "dns": [], "addresses": [], "lease": { "available": true } },
            { "name": "dummy0", "link": { "carrier": true }, "dns": [], "addresses": [] },
            { "name": "wlan0", "link": { "carrier": true }, "dns": [], "addresses": [{ "address": "192.168.4.1", "configSource": "static" }] },
        ] },
    })
}

// Interface mode reports each interface that takes part in resolution (with
// carrier, and servers or DHCP to bring them) with where its servers come
// from, the records with their ids, and the last check.
#[tokio::test]
pub(super) async fn the_dns_document_reports_what_lookups_go_to() {
    let (router, fake) = test_app(dns_tree(json!([
        { "id": "r1", "hostname": "nas", "address": "192.168.10.5", "comment": "the NAS" },
    ])));
    fake.set_observed_network(observed());
    let (cookie, _csrf) = mqtt_session(&router).await;

    let response = get(&router, "/api/v1/dns", Some(&cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let document = body_json(response).await;
    assert_eq!(document["mode"], "interface");
    assert_eq!(
        document["global"],
        json!({ "enabled": false, "servers": [] })
    );
    assert_eq!(
        document["effective"],
        json!({ "links": [
            { "name": "eth0", "servers": ["192.168.1.1"], "source": "dhcp" },
            { "name": "eth1", "servers": ["192.168.10.53"], "source": "static" },
            { "name": "eth3", "servers": [], "source": "dhcp" },
        ] })
    );
    assert_eq!(
        document["staticRecords"],
        json!([{ "id": "r1", "hostname": "nas", "address": "192.168.10.5", "comment": "the NAS" }])
    );
    assert_eq!(document["probe"], FakeSettings::dns_probe());
}

// The global servers are written as a task; one that is this device is
// refused by name before micad is asked, and so is an empty list while on.
#[tokio::test]
pub(super) async fn the_global_servers_are_a_task_and_never_the_device_itself() {
    let (router, fake) = test_app(dns_tree(json!([])));
    fake.set_observed_network(observed());
    let (cookie, csrf) = mqtt_session(&router).await;

    for (servers, server) in [
        (json!(["127.0.0.53"]), "127.0.0.53"),
        (json!(["9.9.9.9", "192.168.10.1"]), "192.168.10.1"),
        (json!(["192.168.1.24"]), "192.168.1.24"),
    ] {
        let response = json_request(
            &router,
            "PUT",
            "/api/v1/dns/global",
            json!({ "enabled": true, "servers": servers }),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{servers}"
        );
        let error = envelope(response).await;
        assert_eq!(error["code"], "dns_server_invalid", "{servers}");
        assert_eq!(error["params"]["server"], server, "{servers}");
    }
    for body in [
        json!({ "enabled": true, "servers": [] }),
        json!({ "enabled": true, "servers": ["9.9.9.9", "9.9.9.9"] }),
        json!({ "enabled": true, "servers": ["2620:fe::fe"] }),
        json!({ "enabled": true, "servers": ["9.9.9.9"], "extra": 1 }),
    ] {
        let response = json_request(
            &router,
            "PUT",
            "/api/v1/dns/global",
            body.clone(),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
        assert_eq!(
            envelope(response).await["code"],
            "validation_failed",
            "{body}"
        );
    }
    assert!(fake.set_paths().is_empty());

    let response = json_request(
        &router,
        "PUT",
        "/api/v1/dns/global",
        json!({ "enabled": true, "servers": ["9.9.9.9", "1.1.1.1"] }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert!(body_json(response).await["taskId"].is_string());
    assert_eq!(fake.set_paths(), ["dns.global"]);

    let document = body_json(get(&router, "/api/v1/dns", Some(&cookie)).await).await;
    assert_eq!(document["mode"], "global");
    assert_eq!(
        document["effective"],
        json!({ "global": ["9.9.9.9", "1.1.1.1"] })
    );
}

// A record is added, replaced and removed by its id, each answered at once
// with no task.
#[tokio::test]
pub(super) async fn a_record_is_added_replaced_and_removed_by_id() {
    let (router, fake) = test_app(dns_tree(json!([])));
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "POST",
        "/api/v1/dns/static-records",
        json!({ "hostname": "nas", "address": "192.168.10.5", "comment": "the NAS" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let added = body_json(response).await;
    let id = added["id"].as_str().unwrap().to_string();
    assert_eq!(id.len(), 8, "{added}");
    assert!(added.get("taskId").is_none(), "{added}");
    assert_eq!(
        fake.get_settings("dns.staticRecords").await.unwrap(),
        json!([{ "id": id, "hostname": "nas", "address": "192.168.10.5", "comment": "the NAS" }])
    );

    // Another record under the same name, in any case.
    let response = json_request(
        &router,
        "POST",
        "/api/v1/dns/static-records",
        json!({ "hostname": "NAS", "address": "192.168.10.6" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = envelope(response).await;
    assert_eq!(error["code"], "record_duplicate");
    assert_eq!(error["params"]["hostname"], "NAS");

    for body in [
        json!({ "hostname": "printer", "address": "fd00::5" }),
        json!({ "hostname": "*.lan", "address": "192.168.10.6" }),
        json!({ "hostname": "localhost", "address": "192.168.10.6" }),
        json!({ "hostname": "printer", "address": "192.168.10.6", "id": "mine" }),
    ] {
        let response = json_request(
            &router,
            "POST",
            "/api/v1/dns/static-records",
            body.clone(),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{body}"
        );
        assert_eq!(
            envelope(response).await["code"],
            "validation_failed",
            "{body}"
        );
    }

    let url = format!("/api/v1/dns/static-records/{id}");
    let response = json_request(
        &router,
        "PUT",
        &url,
        json!({ "hostname": "nas", "address": "192.168.10.7" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        body_json(response).await,
        json!({ "id": id, "hostname": "nas", "address": "192.168.10.7" })
    );

    let response = json_request(
        &router,
        "PUT",
        "/api/v1/dns/static-records/nope",
        json!({ "hostname": "nas", "address": "192.168.10.7" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = json_request(
        &router,
        "DELETE",
        &url,
        json!(null),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        fake.get_settings("dns.staticRecords").await.unwrap(),
        json!([])
    );
    let response = json_request(
        &router,
        "DELETE",
        &url,
        json!(null),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// A hundred records are the most there can be.
#[tokio::test]
pub(super) async fn the_hundred_and_first_record_is_refused() {
    let records: Vec<serde_json::Value> = (0..micad_settings::MAX_STATIC_RECORDS)
        .map(|index| json!({ "id": format!("r{index}"), "hostname": format!("host{index}"), "address": "192.168.10.5" }))
        .collect();
    let (router, fake) = test_app(dns_tree(json!(records)));
    let (cookie, csrf) = mqtt_session(&router).await;

    let response = json_request(
        &router,
        "POST",
        "/api/v1/dns/static-records",
        json!({ "hostname": "one-more", "address": "192.168.10.6" }),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let error = envelope(response).await;
    assert_eq!(error["code"], "record_limit");
    assert_eq!(error["params"]["count"], 100);
    assert!(fake.set_paths().is_empty());
}

// The check runs on a POST and answers its result; the settings route no
// longer writes the DNS subtree.
#[tokio::test]
pub(super) async fn the_check_runs_on_request_and_dns_is_not_a_scalar_setting() {
    let (router, _fake) = test_app(dns_tree(json!([])));
    let (cookie, csrf) = mqtt_session(&router).await;

    let refused = get(&router, "/api/v1/dns/probe", Some(&cookie)).await;
    assert_eq!(refused.status(), StatusCode::METHOD_NOT_ALLOWED);
    let response = json_request(
        &router,
        "POST",
        "/api/v1/dns/probe",
        json!({}),
        Some(&cookie),
        Some(&csrf),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await, FakeSettings::dns_probe());

    for path in ["dns.global", "dns.staticRecords"] {
        let response = json_request(
            &router,
            "PUT",
            &format!("/api/v1/settings/{path}"),
            json!([]),
            Some(&cookie),
            Some(&csrf),
        )
        .await;
        assert!(response.status().is_client_error(), "{path}");
    }
}

//! The per-rule codes a refused network write answers, and its `details`.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

async fn refused(router: &Router, token: &str, map: serde_json::Value) -> serde_json::Value {
    let response = put_network_map(router, token, &map).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    envelope(response).await
}

fn codes(error: &serde_json::Value) -> Vec<(String, String, String)> {
    error["details"]
        .as_array()
        .unwrap()
        .iter()
        .map(|detail| {
            (
                detail["interface"].as_str().unwrap().to_string(),
                detail["field"].as_str().unwrap().to_string(),
                detail["code"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn static_at(address: &str) -> serde_json::Value {
    json!({ "dhcp": false, "static": { "address": address } })
}

// Each rule a write is held to answers its own code, on the entry and the
// field it is about, with the values its message names.
#[tokio::test]
pub(super) async fn each_rule_answers_its_own_code() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let before = stored_network_map(&fake).await;

    let error = refused(
        &router,
        &token,
        json!({
            "eth0": static_at("192.168.10.2/24"),
            "eth1": static_at("192.168.10.9/16"),
        }),
    )
    .await;
    assert_eq!(error["code"], "address_overlap");
    assert_eq!(error["params"], json!({ "name": "eth0" }));
    assert_eq!(
        codes(&error),
        [(
            "eth1".into(),
            "static.address".into(),
            "address_overlap".into()
        )]
    );

    let error = refused(&router, &token, json!({
        "eth0": { "dhcp": false, "static": { "address": "192.168.10.2/24", "gateway": "192.168.11.1" } },
    }))
    .await;
    assert_eq!(error["code"], "gateway_out_of_subnet");
    assert_eq!(error["details"][0]["field"], "static.gateway");

    let error = refused(&router, &token, json!({
        "eth0": { "dhcp": true, "routes": [{ "destination": "0.0.0.0/0", "gateway": "192.168.10.1", "metric": 100 }] },
        "eth1": { "dhcp": true, "routes": [{ "destination": "0.0.0.0/0", "gateway": "10.0.0.1", "metric": 100 }] },
    }))
    .await;
    assert_eq!(error["code"], "metric_duplicate");
    assert_eq!(error["params"], json!({ "name": "eth0", "metric": 100 }));
    assert_eq!(error["details"][0]["interface"], "eth1");

    let vlan =
        |id: u16| json!({ "kind": "vlan", "dhcp": true, "vlan": { "parent": "eth0", "id": id } });
    let error = refused(
        &router,
        &token,
        json!({
            "eth0": { "dhcp": true },
            "eth0.100": vlan(100),
            "lan100": vlan(100),
        }),
    )
    .await;
    assert_eq!(error["code"], "vlan_duplicate");
    assert_eq!(
        error["params"],
        json!({ "parent": "eth0", "id": 100, "name": "eth0.100" })
    );
    assert_eq!(error["details"][0]["interface"], "lan100");

    let bridge =
        |port: &str| json!({ "kind": "bridge", "dhcp": true, "bridge": { "ports": [port] } });
    let error = refused(
        &router,
        &token,
        json!({
            "eth1": { "dhcp": false },
            "br0": bridge("eth1"),
            "br1": bridge("eth1"),
        }),
    )
    .await;
    assert_eq!(error["code"], "bridge_port_taken");
    assert_eq!(error["params"], json!({ "port": "eth1", "bridge": "br0" }));

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(stored_network_map(&fake).await, before);
}

// Every rule the body breaks is in `details`, not only the first.
#[tokio::test]
pub(super) async fn every_broken_rule_is_listed() {
    let (router, _fake, _cookie, token) = kinds_app().await;
    let error = refused(&router, &token, json!({
        "eth0": { "dhcp": false, "static": { "address": "192.168.10.2/24", "gateway": "10.9.9.9" } },
        "eth1": static_at("192.168.10.3/24"),
        "vlan9": { "kind": "vlan", "dhcp": true, "vlan": { "parent": "eth7", "id": 9 } },
    }))
    .await;
    assert_eq!(
        codes(&error),
        [
            (
                "vlan9".into(),
                "vlan.parent".into(),
                "vlan_parent_invalid".into()
            ),
            (
                "eth0".into(),
                "static.gateway".into(),
                "gateway_out_of_subnet".into()
            ),
            (
                "eth1".into(),
                "static.address".into(),
                "address_overlap".into()
            ),
        ]
    );
    assert_eq!(error["code"], "vlan_parent_invalid");
}

// A rule two stored entries already break is not this request's fault: an
// edit of a third entry goes through.
#[tokio::test]
pub(super) async fn a_collision_between_entries_the_write_does_not_touch_is_not_its_fault() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let mut map = stored_network_map(&fake).await;
    map["eth0"] = static_at("192.168.10.2/24");
    map["eth1"] = static_at("192.168.10.3/24");
    map.as_object_mut().unwrap().remove("br0");
    fake.set_settings("network", &map).await.unwrap();

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("wg0"),
        &token,
        &json!({ "kind": "wireguard", "dhcp": false, "static": { "address": "10.8.0.2/24" }, "wireguard": { "peers": [] } }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth1"),
        &token,
        &static_at("192.168.10.3/24").to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(envelope(response).await["code"], "address_overlap");
}

/// A device whose `eth2` runs on the image's DHCP file and whose `eth3` runs
/// on a file an entry cannot say.
fn with_image_links(fake: &FakeSettings) {
    fake.set_observed_network(json!({
        "interfaces": { "available": true, "entries": [
            { "name": "eth2", "adoption": {
                "networkFile": "/etc/systemd/network/80-dhcp.network", "entry": { "dhcp": true } } },
            { "name": "eth3", "adoption": {
                "networkFile": "/etc/systemd/network/10-bond.network", "refused": "[Network] Bond=" } },
        ] },
    }));
}

fn vlan_on(parent: &str) -> serde_json::Value {
    json!({ "kind": "vlan", "dhcp": false, "vlan": { "parent": parent, "id": 7 } })
}

// A VLAN on an interface the map does not declare takes the parent over
// with the entry that keeps the image's configuration; a dry run says so.
#[tokio::test]
pub(super) async fn a_vlan_adopts_its_undeclared_parent() {
    let (router, fake, _cookie, token) = kinds_app().await;
    with_image_links(&fake);
    let mut map = stored_network_map(&fake).await;
    map["eth2.7"] = vlan_on("eth2");

    let read = body_json(bearer(&router, "GET", NETWORK_MAP_PATH, &token).await).await;
    let body = json!({ "baseVersion": read["version"], "configured": map });
    let url = format!("{NETWORK_MAP_PATH}?dryRun=true");
    let response = bearer_json(&router, "PUT", &url, &token, &body.to_string()).await;
    assert_eq!(response.status(), StatusCode::OK);
    let answer = body_json(response).await;
    assert_eq!(answer["adopted"], json!(["eth2"]));

    let response = put_network_map(&router, &token, &map).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    assert_eq!(
        stored_network_map(&fake).await["eth2"],
        json!({ "dhcp": true })
    );
}

// The single-entry route writes the parent it adopts with the VLAN.
#[tokio::test]
pub(super) async fn a_vlan_written_alone_adopts_its_parent_too() {
    let (router, fake, _cookie, token) = kinds_app().await;
    with_image_links(&fake);

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth2.7"),
        &token,
        &vlan_on("eth2").to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let stored = stored_network_map(&fake).await;
    assert_eq!(stored["eth2"], json!({ "dhcp": true }));
    assert_eq!(stored["eth2.7"]["vlan"]["parent"], "eth2");
}

#[tokio::test]
pub(super) async fn a_parent_whose_file_an_entry_cannot_say_is_refused() {
    let (router, fake, _cookie, token) = kinds_app().await;
    with_image_links(&fake);
    let before = stored_network_map(&fake).await;
    let mut map = before.clone();
    map["eth3.7"] = vlan_on("eth3");

    let error = refused(&router, &token, map).await;
    assert_eq!(error["code"], "vlan_parent_unsupported");
    assert_eq!(
        error["params"],
        json!({ "parent": "eth3", "reason": "[Network] Bond=" })
    );
    assert_eq!(
        codes(&error),
        [(
            "eth3.7".into(),
            "vlan.parent".into(),
            "vlan_parent_unsupported".into()
        )]
    );

    let response = bearer_json(
        &router,
        "PUT",
        &iface_url("eth3.7"),
        &token,
        &vlan_on("eth3").to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(stored_network_map(&fake).await, before);
}

// `/proc/net/udp` with one userspace socket on `port` and the kernel socket
// a WireGuard device holds on 51820 (inode 0).
fn udp_table(port: u16) -> String {
    format!(
        "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n\
         \x20 1: 00000000:{port:04X} 00000000:0000 07 00000000:00000000 00:00000000 00000000   100        0 31337 2 0000000000000000 0\n\
         \x20 2: 00000000:CA6C 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 0 2 0000000000000000 0\n"
    )
}

// A tunnel's listen port is refused when another tunnel has it or a service
// on the device holds it; the port a tunnel already listens on is its own.
#[tokio::test]
pub(super) async fn a_listen_port_in_use_is_refused() {
    let proc_net = tempfile::TempDir::new().unwrap();
    std::fs::write(proc_net.path().join("udp"), udp_table(51830)).unwrap();
    let (tree, token) = with_token(kinds_tree("hunter2secret"));
    let fake = std::sync::Arc::new(FakeSettings::new(tree));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY).with_proc_net(proc_net.path()));
    let tunnel = |port: u16, subnet: &str| {
        json!({
            "kind": "wireguard", "dhcp": false, "static": { "address": subnet },
            "wireguard": { "listenPort": port, "peers": [] },
        })
    };
    let mut map = stored_network_map(&fake).await;

    map["wg1"] = tunnel(51820, "10.9.0.1/24");
    let error = refused(&router, &token, map.clone()).await;
    assert_eq!(error["code"], "port_in_use");
    assert_eq!(error["params"], json!({ "name": "wg0", "port": 51820 }));
    assert_eq!(
        codes(&error),
        [(
            "wg1".into(),
            "wireguard.listenPort".into(),
            "port_in_use".into()
        )]
    );

    map["wg1"] = tunnel(51830, "10.9.0.1/24");
    let error = refused(&router, &token, map.clone()).await;
    assert_eq!(error["code"], "port_in_use");
    assert_eq!(error["params"], json!({ "port": 51830 }));
    assert_eq!(
        codes(&error),
        [(
            "wg1".into(),
            "wireguard.listenPort".into(),
            "port_in_use".into()
        )]
    );

    map["wg1"] = tunnel(51831, "10.9.0.1/24");
    assert_eq!(
        put_network_map(&router, &token, &map).await.status(),
        StatusCode::ACCEPTED
    );
}

// An interface kept off the internet carries no default route: a static
// gateway or a `0.0.0.0/0` route is refused, and `internet` is for the
// kinds that can carry one.
#[tokio::test]
pub(super) async fn an_interface_off_the_internet_takes_no_gateway() {
    let (router, fake, _cookie, token) = kinds_app().await;
    let error = refused(&router, &token, json!({
        "eth0": { "dhcp": false, "internet": false, "static": { "address": "192.168.10.2/24", "gateway": "192.168.10.1" } },
        "eth1": { "dhcp": true, "internet": false, "routes": [{ "destination": "0.0.0.0/0", "gateway": "10.0.0.1" }] },
        "wg0": { "kind": "wireguard", "internet": false, "static": { "address": "10.8.0.2/24" }, "wireguard": { "peers": [] } },
    }))
    .await;
    assert_eq!(error["code"], "gateway_not_allowed");
    assert_eq!(
        codes(&error),
        [
            (
                "eth0".into(),
                "static.gateway".into(),
                "gateway_not_allowed".into()
            ),
            ("eth1".into(), "routes".into(), "gateway_not_allowed".into()),
            ("wg0".into(), "internet".into(), "validation_failed".into()),
        ]
    );

    let response = put_network_map(
        &router,
        &token,
        &json!({
            "eth1": { "dhcp": true, "internet": false },
        }),
    )
    .await;
    assert!(response.status().is_success(), "{}", response.status());
    assert_eq!(stored_network_map(&fake).await["eth1"]["internet"], false);
}

//! Claiming through the route.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

/// The credential and the record of how the device was claimed reach the bus
/// as ONE write of ONE subtree.
///
/// The assertion is the write LIST and not the resulting tree, because the
/// resulting tree looks identical whether it took one write or two. micad
/// turns one `SetSettings` into one `Store::save`, so "one write here" is
/// exactly "one commit point on the device".
#[tokio::test]
pub(super) async fn a_route_claim_commits_the_credential_and_the_record_in_one_write() {
    let (router, fake) = test_app(seeded_tree());

    let response = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    assert_eq!(
        fake.set_paths(),
        vec!["access".to_string()],
        "the claim must be one write of one subtree"
    );
    let access = access_of(&fake).await;
    assert!(access["webAdmin"]["password_hash"].as_str().is_some());
    assert_eq!(access["claim"]["via"], json!("setup"));
    assert_eq!(access["claim"]["rotationRequired"], json!(false));
    // Setup mints nothing: an operator who wants API access asks for a token
    // with the credential this write just created.
    assert!(access.get("apiTokens").is_none(), "{access}");
}

/// Everything the route has no opinion about survives the whole-subtree write.
///
/// The claim writes `access` and not `access.webAdmin`, so the keys it does
/// not name — the SSH policy, the console switch, the first-boot device
/// credential — have to be written back exactly as they were read. A
/// whole-subtree write that rebuilt the object instead would silently reset
/// the appliance's access policy at the moment it was claimed.
#[tokio::test]
pub(super) async fn the_claim_write_preserves_every_access_key_it_does_not_name() {
    let mut tree = seeded_tree();
    tree["access"] = json!({
        "ssh": {
            "enabled": true,
            "port": 2222,
            "permitRootLogin": false,
            "passwordAuthentication": false,
            "listenAddresses": ["10.0.0.7"],
            "authorizedKeys": [],
        },
        "console": { "shellEnabled": true },
        "device": { "generation": 1 },
    });
    let (router, fake) = test_app(tree);

    let response = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(response.status(), StatusCode::CREATED);

    let access = access_of(&fake).await;
    assert_eq!(access["ssh"]["port"], json!(2222));
    assert_eq!(access["ssh"]["enabled"], json!(true));
    assert_eq!(access["console"]["shellEnabled"], json!(true));
    assert_eq!(access["device"]["generation"], json!(1));
}

// --- Power loss, and the retry ---------------------------------------------

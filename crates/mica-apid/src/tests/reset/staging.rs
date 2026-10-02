//! Staging a reset, tier by tier.

use axum::http::StatusCode;
use serde_json::json;

use super::*;

/// Tiers 1 and 2 are authenticated management actions, and staging one is ONE
/// write of ONE subtree.
///
/// The assertion is the write LIST and not the resulting tree: the intent
/// record is the whole of the commit, so "one write here" is exactly "one
/// `Store::save` on the device", and a device that lost power mid-request is
/// either not asked or asked.
#[tokio::test]
pub(super) async fn an_authenticated_operator_stages_tiers_one_and_two_in_one_write() {
    for (tier, event) in [
        ("configuration", "reset-configuration"),
        ("application-data", "reset-application-data"),
    ] {
        let (router, fake, dir, token) = device(FakePresence::absent());

        let response = bearer_json(
            &router,
            "POST",
            RESET_PATH,
            &token,
            &json!({ "tier": tier }).to_string(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "{tier}");
        assert_api_headers(&response, RESET_PATH);
        let body = body_json(response).await;
        assert_eq!(body["tier"], json!(tier));
        assert_eq!(body["applies"], json!("next-boot"));

        assert_eq!(
            fake.set_paths(),
            vec!["reset".to_string()],
            "staging {tier} wrote more than the intent"
        );
        let staged = fake.get_settings("reset").await.unwrap();
        assert_eq!(staged["tier"], json!(tier));
        // Tiers 1 and 2 carry no presence: they are not presence-gated, and a
        // record claiming otherwise would put a mechanism in the trail that
        // nobody performed.
        assert_eq!(staged["presence"], json!(null));

        assert_eq!(
            audit_events(&audit_lines(dir.path())),
            [(event.to_string(), "staged".to_string())]
        );
    }
}

/// Tier 1 does not touch the management credential — not here and not on the
/// device. apid's half of that is that staging writes `reset` and nothing
/// else, so `access` is exactly as it was.
///
/// A configuration reset that dropped the credential would be a lockout
/// dressed as a settings action, and a remote credential-clearing primitive.
#[tokio::test]
pub(super) async fn staging_a_configuration_reset_leaves_the_management_credential_alone() {
    let (router, fake, _dir, token) = device(FakePresence::absent());
    let before = access_of(&fake).await;

    let response = bearer_json(
        &router,
        "POST",
        RESET_PATH,
        &token,
        &json!({ "tier": "configuration" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    assert_eq!(access_of(&fake).await, before, "staging touched `access`");
    // And the credential it did not touch still authenticates.
    let probe = bearer(&router, "GET", "/api/v1/meta", &token).await;
    assert_eq!(probe.status(), StatusCode::OK);
}

/// Tier 3 is refused without presence, and the refusal writes nothing.
///
/// A credential alone is not enough: the line is drawn where the operation
/// stops being self-serviceable, and a device whose identity and credentials
/// are gone cannot be handed back to its owner over the network.
#[tokio::test]
pub(super) async fn a_full_factory_reset_is_refused_without_presence() {
    let (router, fake, dir, token) = device(FakePresence::absent());

    let response = bearer_json(
        &router,
        "POST",
        RESET_PATH,
        &token,
        &json!({ "tier": "full-factory" }).to_string(),
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(envelope(response).await["code"], "presence_required");
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("reset-full-factory".to_string(), "refused".to_string())]
    );
}

/// With presence asserted it is staged, and the record names the mechanism
/// that authorized it.
#[tokio::test]
pub(super) async fn a_full_factory_reset_is_staged_when_presence_is_asserted() {
    let (router, fake, dir, token) = device(FakePresence::present());

    let response = bearer_json(
        &router,
        "POST",
        RESET_PATH,
        &token,
        &json!({ "tier": "full-factory" }).to_string(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    assert_eq!(fake.set_paths(), vec!["reset".to_string()]);
    let staged = fake.get_settings("reset").await.unwrap();
    assert_eq!(staged["tier"], json!("full-factory"));
    assert_eq!(staged["presence"], json!(FIXTURE_MECHANISM));
    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("reset-full-factory".to_string(), "staged".to_string())]
    );
}

/// **Tier 4 does not exist in any form.** No spelling of secure wipe is a tier
/// this device accepts, and the refusal happens before anything is written.
///
/// Asserted rather than assumed because the absence is the position: every cell of that row bench-dependent on a
/// device-level erase primitive no board has evidenced, and the answer until
/// then is to destroy the medium — not to offer a tier that cannot keep its
/// promise.
#[tokio::test]
pub(super) async fn no_spelling_of_a_fourth_tier_is_accepted() {
    let (router, fake, dir, token) = device(FakePresence::present());

    for spelling in ["secure-wipe", "secureWipe", "secure_wipe", "wipe", "4"] {
        let response = bearer_json(
            &router,
            "POST",
            RESET_PATH,
            &token,
            &json!({ "tier": spelling }).to_string(),
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "`{spelling}` was accepted"
        );
        assert_eq!(envelope(response).await["code"], "validation_failed");
    }
    // A parameterless reset is not a request either.
    let bare = bearer_json(&router, "POST", RESET_PATH, &token, &json!({}).to_string()).await;
    assert_eq!(bare.status(), StatusCode::UNPROCESSABLE_ENTITY);

    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
    // Nothing to audit: a body this route cannot parse names no tier, so there
    // is no event to record it under.
    assert!(!dir.path().join("audit.log").exists());
}

/// The reset route needs a credential, presence or not.
#[tokio::test]
pub(super) async fn the_reset_route_refuses_an_unauthenticated_caller() {
    let (router, fake, _dir, _token) = device(FakePresence::present());

    for tier in ["configuration", "application-data", "full-factory"] {
        let response = post_json(
            &router,
            RESET_PATH,
            &json!({ "tier": tier }).to_string(),
            None,
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{tier}");
        assert_eq!(envelope(response).await["code"], "not_authenticated");
    }
    assert!(fake.set_paths().is_empty(), "{:?}", fake.set_paths());
}

// --- Credential recovery ---------------------------------------------------

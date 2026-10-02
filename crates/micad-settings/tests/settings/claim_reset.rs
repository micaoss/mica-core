//! The claim record and the staged reset.

use micad_settings::{
    ClaimChannel, ClaimSettings, ResetSettings, ResetTier, Settings, SettingsError,
    WebAdminSettings,
};
use serde_json::json;
use std::fs;

use super::*;

/// A device that has never been claimed carries no record at all, and the
/// serialized tree has no `claim` key to mistake for one.
///
/// The property the additive rule depends on: two adjacent versions of
/// the STATE document differ by the version integer alone until something
/// claims the device.
#[test]
pub(super) fn an_unclaimed_tree_carries_no_claim_key() {
    let settings = Settings::default();
    assert_eq!(settings.access.claim, None);

    let text = toml::to_string(&settings).unwrap();
    assert!(!text.contains("claim"), "{text}");
}

/// The record round-trips through the store, and it lands under `access` — the
/// subtree apid's gate already reads, which is what lets a claim commit the
/// credential, the record and the minted token in one save.
#[test]
pub(super) fn the_claim_record_round_trips_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let mut settings = Settings::default();
    settings.access.web_admin = Some(WebAdminSettings {
        password_hash: "$argon2id$v=19$m=19456,t=2,p=1$ZGV2$ZGV2".to_string(),
    });
    settings.access.claim = Some(ClaimSettings {
        via: ClaimChannel::Setup,
        at: 1_700_000_000,
        rotation_required: false,
    });

    let store = store_at(&dir);
    store.save(&settings).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded.access.claim, settings.access.claim);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("[access.claim]"), "{text}");
}

/// The wire spelling both channels serialize to, pinned: apid reads these two
/// strings out of `GetSettings("access")` and a rename here would silently
/// turn every claimed device into an unreadable record.
#[test]
pub(super) fn the_claim_channels_have_kebab_case_wire_names() {
    let mut settings = Settings::default();
    for (channel, expected) in [
        (ClaimChannel::Setup, "setup"),
        (ClaimChannel::ProvisioningDocument, "provisioning-document"),
    ] {
        settings.access.claim = Some(ClaimSettings {
            via: channel,
            at: 0,
            rotation_required: true,
        });
        let access = settings.get("access").unwrap();
        assert_eq!(access["claim"]["via"], json!(expected));
        assert_eq!(access["claim"]["rotationRequired"], json!(true));
    }
}

// --- The staged reset intent (schema v12) ----------------------------------

/// A device with no reset staged carries no `reset` key at all, and the
/// serialized tree has none to mistake for one.
///
/// The property the additive rule depends on: two adjacent versions of
/// the STATE document differ by the version integer alone until a reset is
/// staged.
#[test]
pub(super) fn a_tree_with_no_reset_staged_carries_no_reset_key() {
    let settings = Settings::default();
    assert_eq!(settings.reset, None);

    let text = toml::to_string(&settings).unwrap();
    assert!(!text.contains("reset"), "{text}");
}

/// The record round-trips through the store, so an intent committed before a
/// power loss is still there for the boot that applies it. That survival is
/// the whole mechanism.
#[test]
pub(super) fn the_reset_intent_round_trips_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let settings = Settings {
        reset: Some(ResetSettings {
            tier: ResetTier::FullFactory,
            requested: 1_700_000_000,
            presence: Some("console-attach".to_string()),
        }),
        ..Settings::default()
    };

    let store = store_at(&dir);
    store.save(&settings).unwrap();
    let loaded = store.load().unwrap();

    assert_eq!(loaded.reset, settings.reset);
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("[reset]"), "{text}");
}

/// The wire spelling every tier serializes to, pinned: apid writes these three
/// strings through `SetSettings("reset")` and micad reads them back, so a
/// rename here would strand an intent a device already committed.
#[test]
pub(super) fn the_reset_tiers_have_kebab_case_wire_names_and_there_is_no_fourth() {
    let mut settings = Settings::default();
    for (tier, expected) in [
        (ResetTier::Configuration, "configuration"),
        (ResetTier::ApplicationData, "application-data"),
        (ResetTier::FullFactory, "full-factory"),
    ] {
        settings.reset = Some(ResetSettings {
            tier,
            requested: 0,
            presence: None,
        });
        let staged = settings.get("reset").unwrap();
        assert_eq!(staged["tier"], json!(expected));
        assert!(staged.get("presence").is_none(), "{staged}");
    }

    for spelling in ["secure-wipe", "secureWipe", "secure_wipe", "wipe"] {
        let err = settings
            .set("reset", json!({ "tier": spelling, "requested": 0 }))
            .unwrap_err();
        assert!(
            matches!(err, SettingsError::Validation { .. }),
            "`{spelling}` was accepted as a tier: {err:?}"
        );
    }
}

/// The intent is one dot-path write of one subtree, and clearing it is
/// another — which is what makes an apply's commit a single `Store::save`.
#[test]
pub(super) fn a_reset_intent_is_staged_and_cleared_by_one_write_each() {
    let mut settings = Settings::default();

    settings
        .set(
            "reset",
            json!({ "tier": "configuration", "requested": 1_700_000_000_u64 }),
        )
        .unwrap();
    assert_eq!(
        settings.reset,
        Some(ResetSettings {
            tier: ResetTier::Configuration,
            requested: 1_700_000_000,
            presence: None,
        })
    );

    settings.set("reset", json!(null)).unwrap();
    assert_eq!(settings.reset, None);
}

/// The whole `access` subtree is writable in ONE dot-path write carrying the
/// credential, the claim record and the token list together.
///
/// This is the transaction the claim flow needs: micad turns one `SetSettings`
/// into one `Store::save`, so a claim that reaches the bus as one write cannot
/// leave a device half-claimed on a power loss.
#[test]
pub(super) fn the_whole_access_subtree_is_one_write() {
    let mut settings = Settings::default();

    settings
        .set(
            "access",
            json!({
                "webAdmin": { "password_hash": "$argon2id$v=19$m=19456,t=2,p=1$ZGV2$ZGV2" },
                "claim": { "via": "setup", "at": 7, "rotationRequired": false },
                "apiTokens": [{
                    "id": "3f2a9c41",
                    "name": "first-run setup",
                    "hash": "a".repeat(64),
                    "created": 7,
                }],
            }),
        )
        .unwrap();

    assert!(settings.access.web_admin.is_some());
    assert_eq!(
        settings.access.claim,
        Some(ClaimSettings {
            via: ClaimChannel::Setup,
            at: 7,
            rotation_required: false,
        })
    );
    assert_eq!(settings.access.api_tokens.len(), 1);
}

/// A claim record with a key the schema does not know is refused, and the
/// write leaves the tree untouched — the discipline every other subtree has.
#[test]
pub(super) fn an_unknown_claim_key_is_refused_and_writes_nothing() {
    let mut settings = Settings::default();
    let before = settings.clone();

    let err = settings
        .set(
            "access.claim",
            json!({ "via": "setup", "at": 0, "rotationRequired": false, "expiresAt": 9 }),
        )
        .unwrap_err();

    assert!(matches!(err, SettingsError::Validation { .. }), "{err:?}");
    assert_eq!(settings, before);
}

// --- The documents: fail-closed, the tolerant rollback load, containment ----

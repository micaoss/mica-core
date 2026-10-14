//! API tokens in the access subtree.

use micad_settings::{ApiToken, STATE_SCHEMA_VERSION, Settings, validate_api_tokens};
use serde_json::json;
use std::fs;

use super::*;

/// One well-formed token, spelled the way the store spells it.
pub(super) fn api_token(id: &str, name: &str, digest: char) -> ApiToken {
    ApiToken {
        id: id.to_string(),
        name: name.to_string(),
        hash: digest.to_string().repeat(64),
        created: 1_700_000_000,
    }
}

/// The list round-trips through the store: written to TOML as an array of
/// tables under `access`, read back typed, and byte-stable across a re-save.
#[test]
pub(super) fn the_token_list_round_trips_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.toml");
    let store = store_at(&dir);

    let mut settings = Settings::default();
    settings.access.api_tokens = vec![
        api_token("3f2a9c41", "ci-deploy", 'a'),
        api_token("9d4ec7b0", "backup runner", 'b'),
    ];
    validate_api_tokens(&settings.access.api_tokens).unwrap();
    store.save(&settings).unwrap();

    let text = fs::read_to_string(&path).unwrap();
    assert!(text.contains("[[access.apiTokens]]"), "{text}");
    assert!(text.contains(r#"id = "3f2a9c41""#), "{text}");
    assert!(text.contains("created = 1700000000"), "{text}");

    let loaded = store.load().unwrap();
    assert_eq!(loaded, settings);

    // Order is the list's own and is not sorted underneath the caller: the id
    // is the identity, but the order is what a listing shows.
    assert_eq!(loaded.access.api_tokens[0].id, "3f2a9c41");
    assert_eq!(loaded.access.api_tokens[1].name, "backup runner");

    store.save(&loaded).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), text);
}

/// A STATE document that carries no `apiTokens` key still loads, and loads
/// with an empty list rather than failing on the missing key. That is what
/// makes an additive bump additive: two adjacent versions of one
/// document differ by the version integer alone.
#[test]
pub(super) fn a_document_without_the_token_key_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    fs::write(
        dir.path().join("settings.toml"),
        format!(
            r#"schema_version = {STATE_SCHEMA_VERSION}

[access.webAdmin]
password_hash = "$argon2id$fake"
"#
        ),
    )
    .unwrap();

    let settings = store.load().unwrap();

    assert!(settings.access.api_tokens.is_empty());
    assert_eq!(
        settings.access.web_admin.unwrap().password_hash,
        "$argon2id$fake"
    );
}

/// Mint and revoke are read-modify-write of the whole array, because the
/// dot-path syntax has no array indexing. Both halves go through `set`, which
/// is the call apid makes.
#[test]
pub(super) fn mint_and_revoke_are_whole_array_writes_through_the_dot_path() {
    let mut settings = Settings::default();

    let minted = vec![api_token("3f2a9c41", "ci-deploy", 'a')];
    settings
        .set("access.apiTokens", serde_json::to_value(&minted).unwrap())
        .unwrap();
    assert_eq!(settings.access.api_tokens, minted);

    let both = vec![
        api_token("3f2a9c41", "ci-deploy", 'a'),
        api_token("9d4ec7b0", "backup", 'b'),
    ];
    settings
        .set("access.apiTokens", serde_json::to_value(&both).unwrap())
        .unwrap();
    assert_eq!(settings.access.api_tokens.len(), 2);

    // Revocation is the same write with the entry removed, keyed on the id.
    let kept: Vec<ApiToken> = settings
        .access
        .api_tokens
        .iter()
        .filter(|token| token.id != "3f2a9c41")
        .cloned()
        .collect();
    settings
        .set("access.apiTokens", serde_json::to_value(&kept).unwrap())
        .unwrap();
    assert_eq!(settings.access.api_tokens.len(), 1);
    assert_eq!(settings.access.api_tokens[0].id, "9d4ec7b0");
}

/// The read-side shape apid sees: `GetSettings("access")` carries the list,
/// digests and all, which is exactly why `hash` is on the redaction denylist.
#[test]
pub(super) fn the_access_subtree_carries_the_token_list_verbatim() {
    let mut settings = Settings::default();
    settings.access.api_tokens = vec![api_token("3f2a9c41", "ci-deploy", 'a')];

    let access = settings.get("access").unwrap();
    let entry = &access["apiTokens"].as_array().unwrap()[0];

    assert_eq!(entry["id"], json!("3f2a9c41"));
    assert_eq!(entry["hash"], json!("a".repeat(64)));
    assert!(entry.get("token").is_none(), "a plaintext field exists");
    assert!(entry.get("secret").is_none(), "a plaintext field exists");
}

/// A tree that carries a token still carries no plaintext anywhere: the entry
/// is a digest, a label, an id and a clock reading.
#[test]
pub(super) fn a_stored_token_is_a_digest_and_nothing_else() {
    let mut settings = Settings::default();
    settings.access.api_tokens = vec![api_token("3f2a9c41", "ci-deploy", 'a')];

    let text = toml::to_string(&settings).unwrap();
    let entry: toml::Table = text.parse::<toml::Table>().unwrap()["access"]["apiTokens"]
        .as_array()
        .unwrap()[0]
        .as_table()
        .unwrap()
        .clone();
    let mut keys: Vec<&str> = entry.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["created", "hash", "id", "name"]);
}

/// The validator is reachable from outside the crate and refuses the three
/// shapes the model must not hold. The exhaustive cases live in the unit
/// tests; this asserts the export.
#[test]
pub(super) fn the_token_validator_is_public_and_refuses_a_broken_list() {
    validate_api_tokens(&[]).unwrap();
    validate_api_tokens(&[api_token("3f2a9c41", "ci", 'a')]).unwrap();

    let duplicate_id = [
        api_token("3f2a9c41", "ci", 'a'),
        api_token("3f2a9c41", "cd", 'b'),
    ];
    assert!(validate_api_tokens(&duplicate_id).is_err());

    let duplicate_hash = [
        api_token("3f2a9c41", "ci", 'a'),
        api_token("9d4ec7b0", "cd", 'a'),
    ];
    assert!(validate_api_tokens(&duplicate_hash).is_err());

    let mut malformed = api_token("3f2a9c41", "ci", 'a');
    malformed.hash = "sha256:beef".to_string();
    assert!(validate_api_tokens(&[malformed]).is_err());
}

// --- The claim record (schema v11) -----------------------------------------

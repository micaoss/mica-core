//! SSH authorized keys in the tree.

use micad_settings::{
    AuthorizedKey, Settings, SettingsError, encode_base64_nopad, parse_authorized_key,
    validate_authorized_keys,
};
use serde_json::json;

/// Build a structurally valid blob for `key_type` out of the crate's own
/// encoder: the four-byte algorithm-name length, the name, then filler.
///
/// No key is pasted in from anywhere; the bytes are constructed so the test
/// depends on the format rather than on someone else's key material.
pub(super) fn blob_for(key_type: &str) -> String {
    let mut bytes = Vec::new();
    let name = key_type.as_bytes();
    bytes.extend_from_slice(&u32::try_from(name.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(name);
    while bytes.len() < 64 {
        let index = u8::try_from(bytes.len()).unwrap();
        bytes.push(index.wrapping_mul(11).wrapping_add(5));
    }
    let mut encoded = encode_base64_nopad(&bytes);
    while !encoded.len().is_multiple_of(4) {
        encoded.push('=');
    }
    encoded
}

pub(super) fn key_line(key_type: &str) -> String {
    format!("{key_type} {}", blob_for(key_type))
}

/// A freshly built tree carries the key, and it is empty.
#[test]
pub(super) fn default_settings_serialise_an_empty_authorized_key_list_at_the_current_schema() {
    let settings = Settings::default();
    assert!(settings.access.ssh.authorized_keys.is_empty());

    let text = toml::to_string(&settings).unwrap();
    let doc: toml::Table = text.parse().unwrap();
    assert_eq!(
        doc["access"]["ssh"]["authorizedKeys"],
        toml::Value::Array(Vec::new()),
        "the key must be present and empty, not absent"
    );

    // The rest of the SSH policy is untouched by this schema step: another
    // task owns the default flip, and this one must not pre-empt it.
    assert!(!settings.access.ssh.enabled);
    assert_eq!(settings.access.ssh.port, 22);
    assert!(settings.access.ssh.permit_root_login);
    assert!(settings.access.ssh.password_authentication);
    assert!(settings.access.ssh.listen_addresses.is_empty());
}

/// The list is reachable and writable through the dot-path API, and an
/// entry with an unknown field is refused like every other typed write.
#[test]
pub(super) fn dot_path_reaches_the_authorized_key_list() {
    let mut settings = Settings::default();
    assert_eq!(
        settings.get("access.ssh.authorizedKeys").unwrap(),
        json!([])
    );

    let line = key_line("ssh-ed25519");
    settings
        .set(
            "access.ssh.authorizedKeys",
            json!([{"key": line, "comment": "alice@workstation"}]),
        )
        .unwrap();
    assert_eq!(
        settings.access.ssh.authorized_keys,
        vec![AuthorizedKey {
            key: line.clone(),
            comment: Some("alice@workstation".to_string()),
        }]
    );

    // An absent comment stays absent rather than becoming an empty string.
    settings
        .set("access.ssh.authorizedKeys", json!([{"key": line}]))
        .unwrap();
    assert_eq!(settings.access.ssh.authorized_keys[0].comment, None);
    assert_eq!(
        settings.get("access.ssh.authorizedKeys").unwrap(),
        json!([{"key": line}])
    );

    let before = settings.clone();
    assert!(matches!(
        settings.set("access.ssh.authorizedKeys", json!([{"kye": line}])),
        Err(SettingsError::Validation { .. })
    ));
    assert!(matches!(
        settings.set("access.ssh.authorizedKeys", json!(["a string"])),
        Err(SettingsError::Validation { .. })
    ));
    assert_eq!(settings, before);
}

/// The typed tree is a container, not a validator. It will hold a key that
/// `validate_authorized_keys` refuses, which is exactly why the callers must
/// run the validator before rendering.
#[test]
pub(super) fn the_typed_tree_holds_what_the_validator_would_refuse() {
    let mut settings = Settings::default();
    settings
        .set(
            "access.ssh.authorizedKeys",
            json!([{"key": "ssh-ed25519 not-base64"}]),
        )
        .unwrap();
    assert!(matches!(
        validate_authorized_keys(&settings.access.ssh.authorized_keys),
        Err(SettingsError::Validation { .. })
    ));
}

/// The parser is reachable from the public API and enforces its rules
/// there, so a consumer crate cannot get a weaker check by importing a
/// different symbol.
#[test]
pub(super) fn the_public_parser_accepts_a_real_key_and_refuses_an_options_line() {
    let line = key_line("ssh-ed25519");
    let parsed = parse_authorized_key(&format!("{line} alice@workstation")).unwrap();
    assert_eq!(parsed.key, line);
    assert_eq!(parsed.comment.as_deref(), Some("alice@workstation"));

    for rejected in [
        format!("command=\"/bin/sh\" {line}"),
        format!("# {line}"),
        format!("{line}\nssh-rsa {}", blob_for("ssh-rsa")),
        String::new(),
    ] {
        assert!(
            parse_authorized_key(&rejected).is_err(),
            "must be rejected: {} bytes",
            rejected.len()
        );
    }

    validate_authorized_keys(std::slice::from_ref(&parsed)).unwrap();
    // The same key twice is one grant, not two.
    assert!(validate_authorized_keys(&[parsed.clone(), parsed]).is_err());
}

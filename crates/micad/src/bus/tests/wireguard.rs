//! WireGuard key rotation.

use super::super::MicadService;
use crate::power::MockPower;
use std::sync::{Arc, Mutex};

use super::*;

/// A service whose settings declare one WireGuard tunnel, with a rotation
/// writing into the same throwaway directory.
///
/// No reconcilers: this exercises the bus method's own contract, and the
/// reconcile it triggers is the network reconciler's own tests' subject.
pub(super) fn service_with_wireguard() -> (MicadService, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut settings = micad_settings::Settings::default();
    settings.network.insert(
        "wg0".to_string(),
        micad_settings::IfaceSettings {
            kind: micad_settings::IfaceKind::Wireguard,
            wireguard: Some(micad_settings::WireguardConfig::default()),
            ..micad_settings::IfaceSettings::default()
        },
    );
    settings.network.insert(
        "eth0".to_string(),
        micad_settings::IfaceSettings {
            dhcp: true,
            ..micad_settings::IfaceSettings::default()
        },
    );
    let service = MicadService::new(
        store_in(&dir),
        settings,
        Vec::new(),
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        dir.path().join("shadow"),
        serde_json::json!({}),
    )
    .with_wireguard(Arc::new(crate::reconciler::network::KeyRotation::new(
        crate::wgkeys::Keystore::under(dir.path(), None),
        crate::reconciler::network::NoDelete,
    )));
    (service, dir)
}

#[tokio::test]
pub(super) async fn a_rotation_returns_the_new_public_key_and_writes_no_key_into_the_tree() {
    let (service, dir) = service_with_wireguard();
    let before = service.get_settings("").await.expect("settings");

    let first = service
        .rotate_wireguard_key("wg0")
        .await
        .expect("rotate wireguard key");
    let second = service
        .rotate_wireguard_key("wg0")
        .await
        .expect("rotate wireguard key");

    assert_ne!(
        first, second,
        "a rotation that returns the same key rotated nothing"
    );
    let key_file = dir.path().join("networkd-secrets/wg-wg0.key");
    let private_key = std::fs::read_to_string(&key_file).expect("key file");
    // The tree holds no key field to write into, and the rotation writes
    // none: what a client reads over `GetSettings` is what it read before.
    assert_eq!(service.get_settings("").await.expect("settings"), before);
    assert!(!before.contains(private_key.trim()), "{before}");
    assert!(!second.contains(private_key.trim()));
    // And nothing was persisted: a rotation is not a settings write.
    assert!(!dir.path().join("settings.toml").exists());
}

/// The two refusals, and the error name each one travels under.
#[tokio::test]
pub(super) async fn a_rotation_refuses_an_interface_that_is_not_a_tunnel() {
    use zbus::DBusError as _;
    let (service, dir) = service_with_wireguard();

    let not_a_tunnel = service.rotate_wireguard_key("eth0").await.unwrap_err();
    let not_declared = service.rotate_wireguard_key("wg9").await.unwrap_err();

    let message = not_a_tunnel.description().unwrap_or_default();
    assert!(
        message.contains("is not a WireGuard interface"),
        "{message}"
    );
    assert_eq!(
        not_a_tunnel.name().as_str(),
        "org.freedesktop.DBus.Error.InvalidArgs",
        "an entry of the wrong kind is a bad argument: {message}"
    );

    let message = not_declared.description().unwrap_or_default();
    assert!(
        message.contains("is not a declared network entry"),
        "{message}"
    );
    assert_eq!(
        not_declared.name().as_str(),
        super::super::NOT_FOUND_ERROR,
        "an undeclared entry names nothing, which is a 404 and not a 422: {message}"
    );
    // Refused before the key store is reached, so no key was drawn for an
    // interface that has no business having one.
    assert!(!dir.path().join("secrets").exists());
}

/// `GetState`'s two failure paths, and the error name each one travels
/// under.
#[tokio::test]
pub(super) async fn a_state_path_that_does_not_resolve_is_not_found() {
    use zbus::DBusError as _;
    let (service, _calls, _dir) = service_with_mock();

    let unresolvable = service.get_state("no.such.path").await.unwrap_err();

    let message = unresolvable.description().unwrap_or_default();
    assert!(message.contains("state path not found"), "{message}");
    assert_eq!(
        unresolvable.name().as_str(),
        super::super::NOT_FOUND_ERROR,
        "a dot-path that resolves to nothing names nothing, which is a 404 \
         and not a 422: {message}"
    );
}

#[tokio::test]
pub(super) async fn a_daemon_with_no_key_store_rotates_nothing() {
    let (service, _calls, dir) = service_with_mock();
    let mut settings = micad_settings::Settings::default();
    settings.network.insert(
        "wg0".to_string(),
        micad_settings::IfaceSettings {
            kind: micad_settings::IfaceKind::Wireguard,
            wireguard: Some(micad_settings::WireguardConfig::default()),
            ..micad_settings::IfaceSettings::default()
        },
    );
    service
        .write_setting("network", serde_json::to_value(&settings.network).unwrap())
        .await
        .expect("declare the tunnel");

    let err = service.rotate_wireguard_key("wg0").await.unwrap_err();

    // The dry-run default: a daemon that was never handed a key store has
    // nowhere to put a key, and says so instead of inventing a place.
    let message = zbus::DBusError::description(&err).unwrap_or_default();
    assert!(message.contains("no WireGuard key store"), "{message}");
    assert!(!dir.path().join("secrets").exists());
}

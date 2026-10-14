//! Password authentication and the transient password marker.

use super::*;

#[tokio::test]
pub(super) async fn without_a_transient_password_password_authentication_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(SshSettings {
            enabled: true,
            password_authentication: true,
            ..SshSettings::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(&paths.environment).unwrap(),
        GOLDEN_DEFAULTS_GATED,
        "root is locked, so the method cannot succeed and must not be offered"
    );
    assert_eq!(state["passwordAuthentication"], json!(false));
    assert_eq!(state["passwordAuthenticationRequested"], json!(true));
    assert_eq!(state["transientPasswordActive"], json!(false));
}

#[tokio::test]
pub(super) async fn a_marker_appearing_between_two_applies_turns_passwords_on_and_restarts_dropbear()
 {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    let settings = settings_with(SshSettings {
        enabled: true,
        password_authentication: true,
        ..SshSettings::default()
    });

    let before = reconciler.apply(&settings).await.unwrap();
    assert_eq!(before["passwordAuthentication"], json!(false));
    assert_eq!(
        std::fs::read_to_string(&paths.environment).unwrap(),
        GOLDEN_DEFAULTS_GATED
    );
    let calls_before = calls(&reconciler).len();

    // Nothing in the settings tree changes here — this is exactly what
    // `SetTransientRootPassword` does before it calls `apply_all`.
    set_marker(&paths.shadow);
    let after = reconciler.apply(&settings).await.unwrap();

    assert_eq!(after["passwordAuthentication"], json!(true));
    assert_eq!(after["transientPasswordActive"], json!(true));
    assert_eq!(
        std::fs::read_to_string(&paths.environment).unwrap(),
        GOLDEN_DEFAULTS
    );
    // A restart and nothing else: the server has to re-read -s, and a
    // stop/start pair would leave a window with no listener at all.
    // mica-system's KillMode=process is what keeps the operator's session.
    assert_eq!(
        calls(&reconciler)[calls_before..],
        ["restart dropbear.service".to_string()],
        "dropbear must pick up the flipped arguments: {:?}",
        calls(&reconciler)
    );
}

#[tokio::test]
pub(super) async fn a_marker_does_not_turn_passwords_on_when_the_setting_says_no() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    set_marker(&paths.shadow);

    let state = reconciler
        .apply(&settings_with(SshSettings {
            enabled: true,
            password_authentication: false,
            ..SshSettings::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        state["passwordAuthentication"],
        json!(false),
        "the gate is an AND of setting and marker, not an OR"
    );
    assert_eq!(state["transientPasswordActive"], json!(true));
    assert_eq!(state["passwordAuthenticationRequested"], json!(false));
    assert_eq!(
        std::fs::read_to_string(&paths.environment).unwrap(),
        GOLDEN_DEFAULTS_GATED
    );
}

#[tokio::test]
pub(super) async fn an_empty_marker_does_not_count_as_a_transient_password() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    std::fs::write(crate::transient::transient_marker_path(&paths.shadow), "").unwrap();

    let state = reconciler
        .apply(&settings_with(SshSettings {
            enabled: true,
            password_authentication: true,
            ..SshSettings::default()
        }))
        .await
        .unwrap();

    assert_eq!(state["transientPasswordActive"], json!(false));
    assert_eq!(state["passwordAuthentication"], json!(false));
}

// ---- published state --------------------------------------------------

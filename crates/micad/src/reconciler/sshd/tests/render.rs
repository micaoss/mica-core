//! The dropbear arguments: listeners, ports and the environment file.

use super::*;

#[test]
pub(super) fn empty_listen_addresses_render_one_listener_on_every_address() {
    let rendered = render_environment(&SshSettings::default(), true);

    assert_eq!(rendered, GOLDEN_DEFAULTS);
}

#[test]
pub(super) fn each_listen_address_becomes_one_listener() {
    let rendered = render_environment(
        &SshSettings {
            enabled: true,
            port: 2222,
            permit_root_login: false,
            password_authentication: false,
            listen_addresses: vec!["10.0.0.5".to_string(), "fd00::1".to_string()],
            authorized_keys: Vec::new(),
        },
        false,
    );

    assert_eq!(rendered, GOLDEN_LISTEN);
    assert_eq!(rendered.matches("-p ").count(), 2);
}

#[test]
pub(super) fn an_address_that_carries_its_own_port_keeps_it() {
    let rendered = render_environment(
        &SshSettings {
            listen_addresses: vec!["10.0.0.5:2200".to_string(), "[fd00::1]:2201".to_string()],
            ..SshSettings::default()
        },
        true,
    );

    assert_eq!(
        rendered,
        "DROPBEAR_ARGS=\"-p 10.0.0.5:2200 -p [fd00::1]:2201\"\n"
    );
}

/// The contract with mica-system's unit: one line, one variable, and
/// always a `-p` (its ExecStartPre refuses a start without one).
#[test]
pub(super) fn every_render_is_exactly_one_dropbear_args_line_with_a_listener() {
    for permit_root_login in [true, false] {
        for password in [true, false] {
            for listen in [vec![], vec!["192.0.2.1".to_string()]] {
                let rendered = render_environment(
                    &SshSettings {
                        permit_root_login,
                        listen_addresses: listen,
                        ..SshSettings::default()
                    },
                    password,
                );
                assert_eq!(rendered.lines().count(), 1, "{rendered}");
                assert!(rendered.starts_with("DROPBEAR_ARGS=\"-p "), "{rendered}");
                assert!(rendered.ends_with("\"\n"), "{rendered}");
            }
        }
    }
}

/// `permitRootLogin = false` refuses root by every method, which is `-w`;
/// `-g` would refuse only root's password, and the only password this
/// device has is root's transient one.
#[test]
pub(super) fn root_login_refusal_is_w_and_g_is_never_rendered() {
    for permit_root_login in [true, false] {
        for password in [true, false] {
            let rendered = render_environment(
                &SshSettings {
                    permit_root_login,
                    ..SshSettings::default()
                },
                password,
            );
            assert_eq!(rendered.contains(" -w"), !permit_root_login, "{rendered}");
            assert_eq!(rendered.contains(" -s"), !password, "{rendered}");
            assert!(!rendered.contains("-g"), "{rendered}");
        }
    }
}

#[tokio::test]
pub(super) async fn apply_rejects_a_listen_address_that_is_not_one() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let mut ssh = ssh_settings(true);
    // A space or a quote here would land verbatim inside DROPBEAR_ARGS,
    // where it splits the value into arguments dropbear never asked for.
    ssh.listen_addresses = vec!["10.0.0.5 -B\" -R".to_string()];

    let err = reconciler.apply(&settings_with(ssh)).await.unwrap_err();

    assert!(err.to_string().contains("listenAddresses"), "{err}");
    assert!(
        !paths.environment.exists(),
        "the reconcile must abort before anything is rendered"
    );
    assert!(!paths.keys.exists());
    assert!(calls(&reconciler).is_empty());
}

#[tokio::test]
pub(super) async fn apply_rejects_more_listen_addresses_than_dropbear_binds() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let mut ssh = ssh_settings(true);
    ssh.listen_addresses = (1..=11).map(|i| format!("192.0.2.{i}")).collect();

    let err = reconciler.apply(&settings_with(ssh)).await.unwrap_err();

    assert!(err.to_string().contains("at most 10"), "{err}");
    assert!(!paths.environment.exists());
}

#[test]
pub(super) fn render_is_deterministic() {
    let ssh = SshSettings {
        enabled: true,
        port: 2222,
        permit_root_login: false,
        password_authentication: true,
        listen_addresses: vec!["10.0.0.5".to_string()],
        authorized_keys: Vec::new(),
    };

    assert_eq!(
        render_environment(&ssh, true),
        render_environment(&ssh.clone(), true)
    );
}

#[tokio::test]
pub(super) async fn apply_writes_the_golden_environment_file_creating_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    // GOLDEN_DEFAULTS_GATED, not GOLDEN_DEFAULTS: the fixture writes no
    // transient marker, so password authentication is gated off.
    assert_eq!(
        std::fs::read_to_string(&paths.environment).unwrap(),
        GOLDEN_DEFAULTS_GATED
    );
    assert_eq!(mode_of(&paths.environment), 0o644);
}

#[tokio::test]
pub(super) async fn apply_leaves_no_temporary_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    for directory in [
        paths.environment.parent().unwrap(),
        paths.keys.parent().unwrap(),
        paths.mica_keys.parent().unwrap(),
        dir.path(),
    ] {
        let leftovers: Vec<_> = std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().contains("micad-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
    }
}

// ---- unit state -------------------------------------------------------

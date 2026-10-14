//! Starting, stopping, restarting and resetting dropbear.

use super::super::super::systemd::mock::MockUnitControl;
use std::os::unix::fs::PermissionsExt;

use super::*;

#[tokio::test]
pub(super) async fn disabled_to_enabled_enables_then_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    assert_eq!(
        calls(&reconciler),
        vec![
            "enable dropbear.service".to_string(),
            "start dropbear.service".to_string()
        ]
    );
    // Enablement is a unit STATE change, not a configuration change: the
    // start reads the freshly written arguments, so nothing restarts.
    assert!(!calls(&reconciler).contains(&"restart dropbear.service".to_string()));
    assert_eq!(state["enabled"], json!(true));
    assert_eq!(state["activeState"], json!("active"));
    assert_eq!(state["unitFileState"], json!("enabled-runtime"));
    assert_eq!(state["unit"], json!("dropbear.service"));
    assert_eq!(reconciler.name(), "sshd");
    assert_eq!(reconciler.subtree(), "access.ssh");
}

#[tokio::test]
pub(super) async fn enabled_to_disabled_stops_then_disables() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "active", "enabled");

    let state = reconciler
        .apply(&settings_with(ssh_settings(false)))
        .await
        .unwrap();

    assert_eq!(
        calls(&reconciler),
        vec![
            "stop dropbear.service".to_string(),
            "disable dropbear.service".to_string()
        ]
    );
    assert_eq!(state["enabled"], json!(false));
    assert_eq!(state["activeState"], json!("inactive"));
    assert_eq!(state["unitFileState"], json!("disabled"));
}

#[tokio::test]
pub(super) async fn already_running_and_enabled_needs_no_calls() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    preexisting_environment(&paths, GOLDEN_DEFAULTS_GATED);

    reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    assert!(
        calls(&reconciler).is_empty(),
        "converged system got calls: {:?}",
        calls(&reconciler)
    );
}

#[tokio::test]
pub(super) async fn already_stopped_and_disabled_needs_no_calls() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings_with(ssh_settings(false)))
        .await
        .unwrap();

    assert!(
        calls(&reconciler).is_empty(),
        "converged system got calls: {:?}",
        calls(&reconciler)
    );
}

#[tokio::test]
pub(super) async fn reapplying_the_same_settings_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let settings = settings_with(ssh_settings(true));

    reconciler.apply(&settings).await.unwrap();
    let after_first = calls(&reconciler);
    // A marker the reconciler would clobber if it rewrote the file: the
    // renderer always produces mode 0644.
    std::fs::set_permissions(&paths.environment, std::fs::Permissions::from_mode(0o600)).unwrap();

    reconciler.apply(&settings).await.unwrap();

    assert_eq!(calls(&reconciler), after_first);
    assert_eq!(
        mode_of(&paths.environment),
        0o600,
        "an unchanged environment file must not be rewritten"
    );
    assert_eq!(
        std::fs::read_to_string(&paths.environment).unwrap(),
        GOLDEN_DEFAULTS_GATED
    );
}

/// dropbear reads its arguments only at start, so changed arguments under
/// a running server are a restart — and nothing else: the unit is neither
/// stopped nor re-enabled on the way.
#[tokio::test]
pub(super) async fn changing_the_arguments_of_a_running_dropbear_restarts_it() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    preexisting_environment(&paths, GOLDEN_DEFAULTS_GATED);

    let state = reconciler
        .apply(&settings_with(SshSettings {
            enabled: true,
            port: 2222,
            ..SshSettings::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        calls(&reconciler),
        vec!["restart dropbear.service".to_string()]
    );
    assert_eq!(
        std::fs::read_to_string(&paths.environment).unwrap(),
        "DROPBEAR_ARGS=\"-p 2222 -s\"\n"
    );
    assert_eq!(state["port"], json!(2222));
}

#[tokio::test]
pub(super) async fn changing_the_arguments_of_a_stopped_dropbear_starts_it_without_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "enabled");
    preexisting_environment(&paths, GOLDEN_DEFAULTS_GATED);

    reconciler
        .apply(&settings_with(SshSettings {
            enabled: true,
            port: 2222,
            ..SshSettings::default()
        }))
        .await
        .unwrap();

    // A start reads the file on the way up, so the change is applied
    // without a restart.
    assert_eq!(
        calls(&reconciler),
        vec!["start dropbear.service".to_string()]
    );
    assert!(
        std::fs::read_to_string(&paths.environment)
            .unwrap()
            .contains("-p 2222")
    );
}

/// dropbear.service fails its start without `/run/mica/dropbear.env`
/// (the EnvironmentFile is required) and would accept only what the key
/// files say at the moment of the first login, so every unit operation
/// has to see the finished files. Observed from inside the unit control,
/// at the moment each call is made.
#[tokio::test]
pub(super) async fn arguments_and_keys_are_on_disk_before_any_unit_operation() {
    struct Observing {
        inner: MockUnitControl,
        watched: Vec<PathBuf>,
        seen: std::sync::Mutex<Vec<(String, bool)>>,
    }
    impl Observing {
        fn observe(&self, verb: &str, unit: &str) {
            let ready = self
                .watched
                .iter()
                .all(|path| std::fs::metadata(path).is_ok_and(|meta| meta.len() > 0));
            self.seen
                .lock()
                .unwrap()
                .push((format!("{verb} {unit}"), ready));
        }
    }
    #[async_trait::async_trait]
    impl UnitControl for Observing {
        async fn active_state(&self, unit: &str) -> Result<String> {
            self.inner.active_state(unit).await
        }
        async fn unit_file_state(&self, unit: &str) -> Result<String> {
            self.inner.unit_file_state(unit).await
        }
        async fn start(&self, unit: &str) -> Result<()> {
            self.observe("start", unit);
            self.inner.start(unit).await
        }
        async fn stop(&self, unit: &str) -> Result<()> {
            self.observe("stop", unit);
            self.inner.stop(unit).await
        }
        async fn restart(&self, unit: &str) -> Result<()> {
            self.observe("restart", unit);
            self.inner.restart(unit).await
        }
        async fn reset_failed(&self, unit: &str) -> Result<()> {
            self.observe("reset-failed", unit);
            self.inner.reset_failed(unit).await
        }
        async fn enable(&self, unit: &str) -> Result<()> {
            self.observe("enable", unit);
            self.inner.enable(unit).await
        }
        async fn disable(&self, unit: &str) -> Result<()> {
            self.observe("disable", unit);
            self.inner.disable(unit).await
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let (probe, paths) = fixture(dir.path(), "inactive", "disabled");
    drop(probe);
    let reconciler = SshdReconciler::new(
        paths.environment.clone(),
        paths.passwd.clone(),
        paths.shadow.clone(),
        Observing {
            inner: MockUnitControl::new("inactive", "disabled"),
            watched: vec![
                paths.environment.clone(),
                paths.keys.clone(),
                paths.mica_keys.clone(),
            ],
            seen: std::sync::Mutex::new(Vec::new()),
        },
    );
    let keys = vec![raw_key(&canonical(REAL_ED25519_LINE), None)];

    reconciler
        .apply(&settings_with_keys(keys.clone()))
        .await
        .unwrap();
    // And a change under the now running server: restart, same rule.
    reconciler
        .apply(&settings_with(SshSettings {
            enabled: true,
            port: 2222,
            authorized_keys: keys,
            ..SshSettings::default()
        }))
        .await
        .unwrap();

    let seen = reconciler.control.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            ("enable dropbear.service".to_string(), true),
            ("start dropbear.service".to_string(), true),
            ("restart dropbear.service".to_string(), true),
        ]
    );
}

/// dropbear.service restarts on failure under systemd's default start
/// limit, so a server that cannot bind (an address not yet on any
/// interface) ends up `failed` with start jobs refused. The operator's
/// corrected settings must bring it up on this apply: the failure is
/// cleared first, and only when there is one.
#[tokio::test]
pub(super) async fn a_failed_dropbear_is_reset_before_it_is_started() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "failed", "enabled-runtime");

    reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    assert_eq!(
        calls(&reconciler),
        vec![
            "reset-failed dropbear.service".to_string(),
            "start dropbear.service".to_string(),
        ]
    );
}

#[tokio::test]
pub(super) async fn a_dropbear_that_is_not_failed_is_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "enabled-runtime");

    reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    assert_eq!(
        calls(&reconciler),
        vec!["start dropbear.service".to_string()]
    );
}

/// The whole policy surface through `apply`: root login, the password
/// setting and whether a transient password is active, with a key list
/// present. Keys are rendered whatever the password policy; `-s` follows
/// the EFFECTIVE value; `-w` follows `permitRootLogin` alone.
#[tokio::test]
pub(super) async fn every_root_password_and_transient_combination_renders_its_flags() {
    for permit_root_login in [true, false] {
        for requested in [true, false] {
            for transient in [true, false] {
                let dir = tempfile::tempdir().unwrap();
                let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
                if transient {
                    set_marker(&paths.shadow);
                }
                let case = format!(
                    "permitRootLogin={permit_root_login} passwordAuthentication={requested} \
                     transient={transient}"
                );

                let state = reconciler
                    .apply(&settings_with(SshSettings {
                        enabled: true,
                        permit_root_login,
                        password_authentication: requested,
                        authorized_keys: vec![raw_key(&canonical(REAL_ED25519_LINE), None)],
                        ..SshSettings::default()
                    }))
                    .await
                    .unwrap();

                let effective = requested && transient;
                let mut expected = String::from("DROPBEAR_ARGS=\"-p 22");
                if !effective {
                    expected.push_str(" -s");
                }
                if !permit_root_login {
                    expected.push_str(" -w");
                }
                expected.push_str("\"\n");
                assert_eq!(
                    std::fs::read_to_string(&paths.environment).unwrap(),
                    expected,
                    "{case}"
                );
                assert_eq!(state["passwordAuthentication"], json!(effective), "{case}");
                assert_eq!(
                    state["passwordAuthenticationRequested"],
                    json!(requested),
                    "{case}"
                );
                assert_eq!(state["transientPasswordActive"], json!(transient), "{case}");
                assert_eq!(state["permitRootLogin"], json!(permit_root_login), "{case}");
                for keys in [&paths.keys, &paths.mica_keys] {
                    assert_eq!(
                        std::fs::read_to_string(keys).unwrap(),
                        format!("{}\n", canonical(REAL_ED25519_LINE)),
                        "{case}: keys are rendered whatever the password policy"
                    );
                }
            }
        }
    }
}

/// A port change reaches every listener, IPv4 and IPv6 alike, except an
/// entry that names its own port.
#[tokio::test]
pub(super) async fn a_port_change_reaches_every_listener_but_one_with_its_own_port() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let listen = vec![
        "192.0.2.7".to_string(),
        "2001:db8::7".to_string(),
        "[2001:db8::8]:2022".to_string(),
    ];

    for port in [22, 2200] {
        reconciler
            .apply(&settings_with(SshSettings {
                enabled: true,
                port,
                listen_addresses: listen.clone(),
                ..SshSettings::default()
            }))
            .await
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(&paths.environment).unwrap(),
            format!(
                "DROPBEAR_ARGS=\"-p 192.0.2.7:{port} -p [2001:db8::7]:{port} \
                 -p [2001:db8::8]:2022 -s\"\n"
            )
        );
    }
}

#[tokio::test]
pub(super) async fn changed_arguments_while_ssh_is_being_disabled_only_stop_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "active", "enabled");
    preexisting_environment(&paths, GOLDEN_DEFAULTS_GATED);

    reconciler
        .apply(&settings_with(SshSettings {
            enabled: false,
            port: 2222,
            ..SshSettings::default()
        }))
        .await
        .unwrap();

    assert_eq!(
        calls(&reconciler),
        vec![
            "stop dropbear.service".to_string(),
            "disable dropbear.service".to_string()
        ]
    );
}

// ---- the device password does not reach the shadow file ---------------

#[tokio::test]
pub(super) async fn no_reconcile_writes_anything_into_the_shadow_file() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(&paths.shadow).unwrap(),
        SHADOW,
        "the root entry is not this reconciler's to write any more"
    );
    assert_eq!(mode_of(&paths.shadow), SHADOW_MODE);
    assert!(
        state.get("rootPassword").is_none(),
        "the removed device-password write must not still be advertised: {state}"
    );
}

#[tokio::test]
pub(super) async fn a_shadow_file_that_is_missing_or_broken_does_not_stop_the_reconcile() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    std::fs::remove_file(&paths.shadow).unwrap();

    reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .expect("the shadow file is not an input to this reconciler");

    assert!(
        !paths.shadow.exists(),
        "a missing shadow file must not be created"
    );
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable dropbear.service".to_string(),
            "start dropbear.service".to_string()
        ]
    );
}

// ---- containment ------------------------------------------------------

#[tokio::test]
pub(super) async fn every_path_the_reconciler_writes_stays_inside_the_tempdir() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    for path in [
        &paths.environment,
        &paths.keys,
        &paths.mica_keys,
        &paths.shadow,
    ] {
        assert!(
            path.starts_with(dir.path()),
            "{} escapes the tempdir",
            path.display()
        );
    }
    assert_eq!(
        state["environmentFile"],
        json!(paths.environment.display().to_string())
    );
    assert_ne!(state["environmentFile"], json!(DEFAULT_ENVIRONMENT_FILE));
    assert_eq!(
        state["authorizedKeysPaths"],
        json!([
            paths.keys.display().to_string(),
            paths.mica_keys.display().to_string(),
        ])
    );
    assert!(
        !state["authorizedKeysPaths"]
            .to_string()
            .contains("\"/root/"),
        "a test must never render into the real /root"
    );
}

// ---- real keys, committed as test constants ---------------------------
//
// Generated with `ssh-keygen` purely for this test. Public keys are not
// secrets, and these correspond to no device: the private halves were
// discarded at generation time and exist nowhere.

/// On an OpenRC root the server is mica-ssh's `mica-dropbear`, apart from
/// any `dropbear` script another package may ship.
#[test]
pub(super) fn an_openrc_root_drives_mica_dropbear() {
    let reconciler = SshdReconciler::openrc(MockUnitControl::new("inactive", "static"));
    assert_eq!(reconciler.unit, "mica-dropbear.service");
    assert_eq!(
        SshdReconciler::production(MockUnitControl::new("inactive", "static")).unit,
        "dropbear.service"
    );
}

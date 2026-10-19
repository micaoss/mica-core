//! The managed accounts' key files, their modes and what the apply refuses.

use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::*;

#[tokio::test]
pub(super) async fn published_state_carries_fingerprints_and_never_key_material() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with_keys(vec![
            raw_key(&canonical(REAL_ED25519_LINE), Some("laptop")),
            raw_key(&canonical(REAL_RSA_LINE), None),
        ]))
        .await
        .unwrap();

    assert_eq!(
        state["authorizedKeysPaths"],
        json!([
            paths.keys.display().to_string(),
            paths.mica_keys.display().to_string(),
        ]),
        "every rendered path is named, in managed-account order"
    );
    assert_eq!(
        state["authorizedKeys"],
        json!([
            {"fingerprint": REAL_ED25519_FINGERPRINT, "comment": "laptop"},
            {"fingerprint": REAL_RSA_FINGERPRINT, "comment": null},
        ]),
        "fingerprints in render order, comment null when the key has none"
    );

    let serialised = state.to_string();
    for line in [REAL_ED25519_LINE, REAL_RSA_LINE] {
        let blob = line.split(' ').nth(1).unwrap();
        assert!(
            !serialised.contains(blob),
            "key material must never reach the published state tree"
        );
    }
}

#[tokio::test]
pub(super) async fn published_state_reports_an_empty_key_list_as_an_empty_array() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings_with_keys(Vec::new()))
        .await
        .unwrap();

    assert_eq!(state["authorizedKeys"], json!([]));
}

// ---- ownership and permissions dropbear checks ------------------------

#[tokio::test]
pub(super) async fn every_key_file_is_the_accounts_0600_file_in_its_0700_ssh_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let (uid, gid) = own_ids();

    reconciler
        .apply(&settings_with_keys(vec![raw_key(
            &canonical(REAL_ED25519_LINE),
            None,
        )]))
        .await
        .unwrap();

    for file in [&paths.keys, &paths.mica_keys] {
        let ssh_dir = file.parent().unwrap();
        assert_eq!(mode_of(file), 0o600, "{}", file.display());
        assert_eq!(mode_of(ssh_dir), 0o700, "{}", ssh_dir.display());
        for path in [file.as_path(), ssh_dir] {
            let meta = std::fs::symlink_metadata(path).unwrap();
            assert_eq!((meta.uid(), meta.gid()), (uid, gid), "{}", path.display());
        }
    }
}

#[tokio::test]
pub(super) async fn an_existing_ssh_directory_is_brought_to_0700() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let ssh_dir = paths.mica_home.join(".ssh");
    std::fs::create_dir(&ssh_dir).unwrap();
    std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(ssh_dir.join("known_hosts"), "operator data").unwrap();

    reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap();

    assert_eq!(mode_of(&ssh_dir), 0o700);
    assert_eq!(
        std::fs::read_to_string(ssh_dir.join("known_hosts")).unwrap(),
        "operator data",
        "only the key file is this reconciler's"
    );
}

/// dropbear refuses every key under a home that group or others can write.
/// The apply fails naming the home, before ANY account's keys or the
/// server's arguments change, and the home is not repaired.
#[tokio::test]
pub(super) async fn a_writable_home_fails_the_apply_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    std::fs::set_permissions(&paths.mica_home, std::fs::Permissions::from_mode(0o775)).unwrap();

    let err = reconciler
        .apply(&settings_with_keys(vec![raw_key(
            &canonical(REAL_ED25519_LINE),
            None,
        )]))
        .await
        .unwrap_err();

    let message = format!("{err:#}");
    assert!(
        message.contains(&paths.mica_home.display().to_string()),
        "{message}"
    );
    assert!(message.contains("group- or world-writable"), "{message}");
    assert!(
        !paths.keys.exists(),
        "root's keys changed on a failed apply"
    );
    assert!(!paths.mica_keys.exists());
    assert!(!paths.environment.exists());
    assert!(calls(&reconciler).is_empty());
    assert_eq!(mode_of(&paths.mica_home), 0o775);
}

/// The home is writable by the account and micad is root: a `~/.ssh`
/// pointing elsewhere must not carry a root-written file there.
#[tokio::test]
pub(super) async fn a_symlinked_ssh_directory_is_refused_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let elsewhere = dir.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, paths.mica_home.join(".ssh")).unwrap();

    let err = reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .unwrap_err();

    assert!(format!("{err:#}").contains("symbolic link"), "{err:#}");
    assert!(!elsewhere.join("authorized_keys").exists());
    assert!(!paths.keys.exists());
}

/// Planted names inside `~/.ssh` are replaced, never written through.
#[tokio::test]
pub(super) async fn symlinks_planted_as_the_key_file_or_its_temporary_are_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "untouched").unwrap();
    let ssh_dir = paths.mica_home.join(".ssh");
    std::fs::create_dir(&ssh_dir).unwrap();
    std::os::unix::fs::symlink(&victim, ssh_dir.join(".authorized_keys.micad-tmp")).unwrap();
    std::os::unix::fs::symlink(&victim, ssh_dir.join("authorized_keys")).unwrap();

    reconciler
        .apply(&settings_with_keys(vec![raw_key(
            &canonical(REAL_ED25519_LINE),
            None,
        )]))
        .await
        .unwrap();

    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "untouched");
    assert!(
        !std::fs::symlink_metadata(&paths.mica_keys)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&paths.mica_keys).unwrap(),
        format!("{}\n", canonical(REAL_ED25519_LINE))
    );
}

/// micad reads the existing key file to skip a no-op rewrite. A FIFO
/// planted under that name would block a plain read-only open until a
/// writer appeared, holding every reconcile behind the account's whim.
#[test]
pub(super) fn a_fifo_planted_as_the_key_file_neither_hangs_the_apply_nor_survives_it() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    std::fs::create_dir(paths.mica_home.join(".ssh")).unwrap();
    rustix::fs::mknodat(
        rustix::fs::CWD,
        &paths.mica_keys,
        rustix::fs::FileType::Fifo,
        Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();

    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let result = runtime.block_on(reconciler.apply(&settings_with(ssh_settings(true))));
        let _ = done.send(result.map(|_| ()).map_err(|err| format!("{err:#}")));
    });

    let result = finished
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("the apply must not block on a FIFO");
    result.unwrap();
    assert!(
        std::fs::symlink_metadata(&paths.mica_keys)
            .unwrap()
            .file_type()
            .is_file()
    );
}

#[tokio::test]
pub(super) async fn an_unchanged_key_list_is_not_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let settings = settings_with_keys(vec![raw_key(&canonical(REAL_ED25519_LINE), None)]);
    reconciler.apply(&settings).await.unwrap();
    // A marker the reconciler would clobber if it rewrote the file: the
    // renderer always produces mode 0600.
    std::fs::set_permissions(&paths.keys, std::fs::Permissions::from_mode(0o640)).unwrap();

    reconciler.apply(&settings).await.unwrap();

    assert_eq!(
        mode_of(&paths.keys),
        0o640,
        "an unchanged key file must not be rewritten"
    );
}

// ---- one key set, rendered for every managed login account ------------

/// The central guard. One validated list, two files, identical bytes.
#[tokio::test]
pub(super) async fn one_key_list_renders_byte_identical_files_for_every_managed_account() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings_with_keys(vec![
            raw_key(&canonical(REAL_ED25519_LINE), Some("laptop")),
            raw_key(&canonical(REAL_RSA_LINE), None),
        ]))
        .await
        .unwrap();

    let expected = format!(
        "{} laptop\n{}\n",
        canonical(REAL_ED25519_LINE),
        canonical(REAL_RSA_LINE)
    );
    for path in [&paths.keys, &paths.mica_keys] {
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            expected,
            "{} does not carry the operator's key list",
            path.display()
        );
    }
}

/// The file set is exactly the constant list. The fixture's account
/// database also names `daemon`, whose home gets nothing: a scan of
/// `/etc/passwd` would render for whatever accounts the host happens to
/// have, which is the thing the constant exists to prevent.
#[tokio::test]
pub(super) async fn only_the_managed_accounts_get_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let daemon_home = dir.path().join("daemon");
    std::fs::create_dir(&daemon_home).unwrap();
    let (uid, gid) = own_ids();
    std::fs::write(
        &paths.passwd,
        std::fs::read_to_string(&paths.passwd).unwrap().replace(
            "daemon:x:1:1:daemon:/usr/sbin:/usr/sbin/nologin",
            &format!(
                "daemon:x:{uid}:{gid}:daemon:{}:/bin/bash",
                daemon_home.display()
            ),
        ),
    )
    .unwrap();

    reconciler
        .apply(&settings_with_keys(vec![raw_key(
            &canonical(REAL_ED25519_LINE),
            None,
        )]))
        .await
        .unwrap();

    assert!(paths.keys.exists());
    assert!(paths.mica_keys.exists());
    assert!(!daemon_home.join(".ssh").exists());
    assert_eq!(MANAGED_LOGIN_ACCOUNTS, ["root", "mica"]);
}

/// An account the database does not name, or whose home does not exist,
/// cannot log in with a key; it is skipped rather than failing the apply,
/// which would keep SSH closed for root too, and it is absent from the
/// published paths.
#[tokio::test]
pub(super) async fn an_account_without_an_entry_or_a_home_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let passwd = std::fs::read_to_string(&paths.passwd).unwrap();
    let without_mica: String = passwd
        .lines()
        .filter(|line| !line.starts_with("mica:"))
        .map(|line| format!("{line}\n"))
        .collect();
    std::fs::write(&paths.passwd, without_mica).unwrap();

    let state = reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .expect("a missing account must not fail the reconcile");

    assert!(paths.keys.exists());
    assert!(!paths.mica_home.join(".ssh").exists());
    assert_eq!(
        state["authorizedKeysPaths"],
        json!([paths.keys.display().to_string()])
    );

    std::fs::write(&paths.passwd, passwd).unwrap();
    std::fs::remove_dir(&paths.mica_home).unwrap();
    let state = reconciler
        .apply(&settings_with(ssh_settings(true)))
        .await
        .expect("a missing home must not fail the reconcile");
    assert!(!paths.mica_home.exists(), "a home is not created");
    assert_eq!(
        state["authorizedKeysPaths"],
        json!([paths.keys.display().to_string()])
    );
}

#[test]
pub(super) fn the_account_entry_is_found_by_exact_name() {
    let passwd = "micax:x:5:5::/nowhere:/bin/sh\n\
                  broken:x:notanumber:1::/x:/bin/sh\n\
                  mica:x:1000:1000:mica operator:/home/mica:/bin/bash\n";

    assert_eq!(
        find_account(passwd, "mica"),
        Some(Account {
            name: "mica".to_string(),
            uid: 1000,
            gid: 1000,
            home: PathBuf::from("/home/mica"),
        })
    );
    assert_eq!(find_account(passwd, "broken"), None);
    assert_eq!(find_account(passwd, "root"), None);
}

/// An empty list empties EVERY account file. Two empty files, not two
/// deletions and not one of each: the golden rule above, per account.
#[tokio::test]
pub(super) async fn an_empty_list_empties_every_account_file_without_deleting_any() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    reconciler
        .apply(&settings_with_keys(vec![raw_key(
            &canonical(REAL_ED25519_LINE),
            None,
        )]))
        .await
        .unwrap();

    reconciler
        .apply(&settings_with_keys(Vec::new()))
        .await
        .unwrap();

    for path in [&paths.keys, &paths.mica_keys] {
        assert!(path.exists(), "{} must not be deleted", path.display());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "",
            "{} must be empty",
            path.display()
        );
    }
}

/// A key the operator removes stops granting access to every account in the
/// same reconcile. A rewrite that reached only `root` would leave the key
/// live for `mica`, which is the removal silently not happening.
#[tokio::test]
pub(super) async fn removing_one_key_of_three_rewrites_every_account_file_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    reconciler
        .apply(&settings_with_keys(vec![
            raw_key(&canonical(REAL_ED25519_LINE), Some("laptop")),
            raw_key(&canonical(REAL_RSA_LINE), None),
            raw_key(&canonical(REAL_ED25519_SECOND_LINE), Some("phone")),
        ]))
        .await
        .unwrap();

    reconciler
        .apply(&settings_with_keys(vec![
            raw_key(&canonical(REAL_ED25519_LINE), Some("laptop")),
            raw_key(&canonical(REAL_ED25519_SECOND_LINE), Some("phone")),
        ]))
        .await
        .unwrap();

    let expected = format!(
        "{} laptop\n{} phone\n",
        canonical(REAL_ED25519_LINE),
        canonical(REAL_ED25519_SECOND_LINE)
    );
    let removed_blob = canonical(REAL_RSA_LINE);
    for path in [&paths.keys, &paths.mica_keys] {
        let content = std::fs::read_to_string(path).unwrap();
        assert_eq!(content, expected, "{} was not rewritten", path.display());
        assert!(
            !content.contains(&removed_blob),
            "the removed key still grants access through {}",
            path.display()
        );
    }
}

/// Fail-loud, per account, with no partial application. Validation runs
/// before the first file is opened, so a rejected list cannot update one
/// account while another keeps the previous keys — the ordering hazard of
/// writing more than one file.
#[tokio::test]
pub(super) async fn a_validation_failure_updates_neither_account() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    reconciler
        .apply(&settings_with_keys(vec![raw_key(
            &canonical(REAL_ED25519_LINE),
            Some("keep-me"),
        )]))
        .await
        .unwrap();
    let before = std::fs::read(&paths.keys).unwrap();
    assert_eq!(std::fs::read(&paths.mica_keys).unwrap(), before);

    // Valid first entry, rejected second: a renderer that wrote as it went
    // would have put the good prefix somewhere before failing.
    reconciler
        .apply(&settings_with_keys(vec![
            raw_key(&canonical(REAL_RSA_LINE), Some("new")),
            raw_key("ssh-ed25519 not-base64!!", None),
        ]))
        .await
        .expect_err("an invalid list must fail the apply");

    for path in [&paths.keys, &paths.mica_keys] {
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "{} changed during a failed apply",
            path.display()
        );
    }
}

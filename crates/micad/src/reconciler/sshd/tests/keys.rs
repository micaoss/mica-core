//! Rendering and validating the authorized keys.

use std::path::Path;

use super::*;

/// `ssh-keygen -t ed25519 -C rfct-034-test-ed25519`, verbatim.
pub(super) const REAL_ED25519_LINE: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIL99V7xPTOP3jZjnbVPM7xC+ckwzkOQPalUpsvtPzYo8 rfct-034-test-ed25519";
/// `ssh-keygen -t rsa -b 2048 -C rfct-034-test-rsa`, verbatim.
pub(super) const REAL_RSA_LINE: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABAQDT2F3imgGgI+xGNSQI+0alU1qRwyU3gCc8wU6msXSzZsVc8OYlg4VIqxsV/GLpBmgRz5lGoxjTT2TU0t1VwaMs845NqRIWzpG88ohD1LMn7RnUrNTxf4syFuvmELmYstqMfc6Q6rApqFoA6023Rl2orgd8N3SQ2wPAw8Rk9OLwim9/R7tX8C8FTbnMtepzTvOUNGTDAaKYhTZZnZpsGCwKa9f2aWyaS2XqLwn9uWpmHRUAkV10l45W2rLhnceejwwHotlZUIAFt8rlmS1ojRaLWqECVAuO5CDTt64KLLRniw8yHIYsWkeVsHZXCxq+J7oUVI3ogOSYs1M4I2eFCccD rfct-034-test-rsa";
/// A second Ed25519 key, so the multi-key golden holds three distinct keys.
pub(super) const REAL_ED25519_SECOND_LINE: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAILFM+HTH5h41h/zyK4CwjXx9E1l8Nwks1NaywRMiSsEP rfct-034-test-ed25519-second";

/// Fingerprints as reported by `ssh-keygen -lf <file>` for the three keys
/// above, copied from that command's output. Comparing this module's
/// fingerprint against a constant that came from OpenSSH is the point: a
/// fingerprint function checked only against itself proves nothing.
pub(super) const REAL_ED25519_FINGERPRINT: &str =
    "SHA256:HrgN3GLi6Mop2uSRjgOoxImM8zRkFmgqCKoeGD9QOaM";
pub(super) const REAL_RSA_FINGERPRINT: &str = "SHA256:zv0xTYuVTo5pFpcl/svzzz/vJFvoguWxKlghlXQS1bE";
pub(super) const REAL_ED25519_SECOND_FINGERPRINT: &str =
    "SHA256:d7yiR/zCsNFh8WmU6CGLWEG5vE06icIelqVoNc8TT2E";

/// The canonical `<type> <blob>` half of a full `ssh-keygen` line.
pub(super) fn canonical(line: &str) -> String {
    let mut fields = line.splitn(3, ' ');
    let key_type = fields.next().unwrap();
    let blob = fields.next().unwrap();
    format!("{key_type} {blob}")
}

/// An [`AuthorizedKey`] built directly, bypassing the parser — the shape a
/// corrupted settings file on STATE would present.
pub(super) fn raw_key(key: &str, comment: Option<&str>) -> AuthorizedKey {
    AuthorizedKey {
        key: key.to_string(),
        comment: comment.map(str::to_string),
    }
}

pub(super) fn settings_with_keys(keys: Vec<AuthorizedKey>) -> Settings {
    settings_with(SshSettings {
        enabled: true,
        authorized_keys: keys,
        ..SshSettings::default()
    })
}

/// Set a transient password marker beside `shadow`, the way
/// `transient::set_transient_root_password` does.
pub(super) fn set_marker(shadow: &Path) {
    std::fs::write(
        crate::transient::transient_marker_path(shadow),
        "$2b$12$notarealhashjustnonempty\n",
    )
    .unwrap();
}

// ---- golden renders ---------------------------------------------------

#[test]
pub(super) fn an_empty_list_renders_an_empty_file() {
    assert_eq!(render_authorized_keys(&[]), "");
}

#[test]
pub(super) fn one_key_without_a_comment_renders_one_bare_line() {
    let rendered = render_authorized_keys(&[raw_key(&canonical(REAL_ED25519_LINE), None)]);

    assert_eq!(rendered, format!("{}\n", canonical(REAL_ED25519_LINE)));
}

#[test]
pub(super) fn one_key_with_a_comment_renders_key_space_comment() {
    let rendered = render_authorized_keys(&[raw_key(
        &canonical(REAL_ED25519_LINE),
        Some("laptop@example"),
    )]);

    assert_eq!(
        rendered,
        format!("{} laptop@example\n", canonical(REAL_ED25519_LINE))
    );
}

#[test]
pub(super) fn three_keys_render_in_settings_order_mixing_commented_and_bare() {
    let rendered = render_authorized_keys(&[
        raw_key(&canonical(REAL_ED25519_LINE), Some("first")),
        raw_key(&canonical(REAL_RSA_LINE), None),
        raw_key(&canonical(REAL_ED25519_SECOND_LINE), Some("third")),
    ]);

    assert_eq!(
        rendered,
        format!(
            "{} first\n{}\n{} third\n",
            canonical(REAL_ED25519_LINE),
            canonical(REAL_RSA_LINE),
            canonical(REAL_ED25519_SECOND_LINE)
        )
    );
    assert_eq!(rendered.lines().count(), 3);
}

#[test]
pub(super) fn rendering_the_same_keys_twice_gives_identical_bytes() {
    let keys = vec![
        raw_key(&canonical(REAL_ED25519_LINE), Some("first")),
        raw_key(&canonical(REAL_RSA_LINE), None),
    ];

    assert_eq!(
        render_authorized_keys(&keys),
        render_authorized_keys(&keys.clone())
    );
}

// ---- a real ssh-keygen key round-trips --------------------------------

/// Genuine `ssh-keygen` output survives the parser and comes back out of
/// the render byte-identical — asserted against a rendered file rather
/// than against pasted key material.
#[test]
pub(super) fn a_real_ssh_keygen_line_parses_canonicalises_and_renders_back_identically() {
    for line in [REAL_ED25519_LINE, REAL_RSA_LINE, REAL_ED25519_SECOND_LINE] {
        let parsed = micad_settings::parse_authorized_key(line)
            .unwrap_or_else(|err| panic!("real ssh-keygen line rejected: {line}: {err}"));

        assert_eq!(parsed.key, canonical(line), "comment leaked into `key`");
        assert_eq!(
            parsed.comment.as_deref(),
            Some(line.splitn(3, ' ').nth(2).unwrap())
        );
        assert_eq!(
            render_authorized_keys(std::slice::from_ref(&parsed)),
            format!("{line}\n"),
            "rendered line differs from the ssh-keygen line it came from"
        );
        micad_settings::validate_authorized_keys(std::slice::from_ref(&parsed)).unwrap();
    }
}

/// The same round-trip on a key generated at test time, so the committed
/// constants above cannot quietly drift away from what OpenSSH emits.
///
/// Skipped when `ssh-keygen` is absent, which is why the committed-constant
/// test above exists as well: this file never becomes a silent no-op.
#[test]
pub(super) fn a_freshly_generated_key_round_trips_when_ssh_keygen_is_available() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh");
    let generated = std::process::Command::new("ssh-keygen")
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "fresh@rfct-034",
            "-f",
        ])
        .arg(&path)
        .status();
    let Ok(status) = generated else {
        eprintln!("ssh-keygen not on this host; committed-constant round-trip still ran");
        return;
    };
    assert!(status.success(), "ssh-keygen failed");

    let line = std::fs::read_to_string(path.with_extension("pub")).unwrap();
    let line = line.trim_end_matches('\n');
    let parsed = micad_settings::parse_authorized_key(line).unwrap();

    assert_eq!(parsed.key, canonical(line));
    assert_eq!(parsed.comment.as_deref(), Some("fresh@rfct-034"));
    assert_eq!(
        render_authorized_keys(std::slice::from_ref(&parsed)),
        format!("{line}\n")
    );
}

// ---- fingerprints agree with ssh-keygen -lf ---------------------------

#[test]
pub(super) fn fingerprints_match_what_ssh_keygen_reports() {
    for (line, expected) in [
        (REAL_ED25519_LINE, REAL_ED25519_FINGERPRINT),
        (REAL_RSA_LINE, REAL_RSA_FINGERPRINT),
        (REAL_ED25519_SECOND_LINE, REAL_ED25519_SECOND_FINGERPRINT),
    ] {
        assert_eq!(
            micad_settings::ssh_fingerprint(&canonical(line)).as_deref(),
            Some(expected)
        );
    }
}

#[test]
pub(super) fn a_key_with_no_decodable_blob_has_no_fingerprint() {
    assert_eq!(micad_settings::ssh_fingerprint("ssh-ed25519"), None);
    assert_eq!(
        micad_settings::ssh_fingerprint("ssh-ed25519 not!base64"),
        None
    );
}

// ---- validation failures leave the file untouched ---------------------

/// Apply once with a good key so there is a rendered file to protect, then
/// apply `bad` and assert the failure changed nothing.
pub(super) async fn assert_bad_keys_leave_the_file_untouched(bad: Vec<AuthorizedKey>, what: &str) {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let good = vec![raw_key(&canonical(REAL_ED25519_LINE), Some("keep-me"))];
    reconciler
        .apply(&settings_with_keys(good))
        .await
        .expect("the good apply must succeed");
    let before = std::fs::read(&paths.keys).unwrap();
    assert!(!before.is_empty(), "nothing was rendered to protect");
    assert_eq!(
        std::fs::read(&paths.mica_keys).unwrap(),
        before,
        "the good apply must have rendered both accounts alike"
    );

    let error = reconciler
        .apply(&settings_with_keys(bad))
        .await
        .expect_err(&format!("{what} must fail the apply"));

    for path in [&paths.keys, &paths.mica_keys] {
        assert_eq!(
            std::fs::read(path).unwrap(),
            before,
            "{what}: {} must be byte-identical after a failed apply",
            path.display()
        );
    }
    let message = format!("{error:#}");
    assert!(
        message.contains("access.ssh.authorizedKeys"),
        "{what}: error should name the setting: {message}"
    );
}

#[tokio::test]
pub(super) async fn a_newline_embedded_in_the_key_field_fails_and_changes_nothing() {
    let injected = format!(
        "{}\nssh-ed25519 AAAAsomethingelse",
        canonical(REAL_ED25519_LINE)
    );
    assert_bad_keys_leave_the_file_untouched(
        vec![raw_key(&injected, None)],
        "a newline in the key field",
    )
    .await;
}

#[tokio::test]
pub(super) async fn a_comment_smuggled_into_the_key_field_fails_and_changes_nothing() {
    assert_bad_keys_leave_the_file_untouched(
        vec![raw_key(REAL_ED25519_LINE, None)],
        "a comment inside the key field",
    )
    .await;
}

#[tokio::test]
pub(super) async fn a_duplicate_key_pair_fails_and_changes_nothing() {
    assert_bad_keys_leave_the_file_untouched(
        vec![
            raw_key(&canonical(REAL_ED25519_LINE), Some("one")),
            raw_key(&canonical(REAL_ED25519_LINE), Some("two")),
        ],
        "a duplicated key",
    )
    .await;
}

#[tokio::test]
pub(super) async fn the_error_from_an_invalid_list_names_the_offending_index() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");

    let error = reconciler
        .apply(&settings_with_keys(vec![
            raw_key(&canonical(REAL_ED25519_LINE), None),
            raw_key("ssh-ed25519 !!!!", None),
        ]))
        .await
        .expect_err("an unparseable entry must fail the apply");

    let message = format!("{error:#}");
    assert!(
        message.contains("entry 1"),
        "error should name the offending index: {message}"
    );
}

/// The positive direction of the same guard: a valid list renders, and it
/// overwrites whatever was there before rather than appending to it.
#[tokio::test]
pub(super) async fn a_valid_list_renders_and_overwrites_the_previous_content() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings_with_keys(vec![
            raw_key(&canonical(REAL_ED25519_LINE), Some("first")),
            raw_key(&canonical(REAL_RSA_LINE), None),
        ]))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(&paths.keys).unwrap(),
        format!(
            "{} first\n{}\n",
            canonical(REAL_ED25519_LINE),
            canonical(REAL_RSA_LINE)
        )
    );

    reconciler
        .apply(&settings_with_keys(vec![raw_key(
            &canonical(REAL_ED25519_SECOND_LINE),
            None,
        )]))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(&paths.keys).unwrap(),
        format!("{}\n", canonical(REAL_ED25519_SECOND_LINE)),
        "the removed keys must be gone, not appended to"
    );
}

/// Removing every key empties the file rather than deleting it: an absent
/// file and an empty file mean the same thing to dropbear, and a key removed
/// has to stop working immediately either way.
#[tokio::test]
pub(super) async fn removing_every_key_empties_the_file_without_deleting_it() {
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

    assert!(paths.keys.exists(), "the file must not be deleted");
    assert_eq!(std::fs::read_to_string(&paths.keys).unwrap(), "");
}

// ---- per-character hostile input in the comment -----------------------

#[tokio::test]
pub(super) async fn each_control_character_in_a_comment_fails_and_changes_nothing() {
    for (ch, name) in [
        ('\0', "NUL"),
        ('\n', "line feed"),
        ('\r', "carriage return"),
        ('\t', "tab"),
        ('\u{7f}', "delete"),
    ] {
        assert_bad_keys_leave_the_file_untouched(
            vec![raw_key(
                &canonical(REAL_ED25519_LINE),
                Some(&format!("host{ch}name")),
            )],
            &format!("a {name} in the comment"),
        )
        .await;
    }
}

/// The other direction: shell metacharacters are ordinary comment text.
/// The rendered file is read by dropbear, not by a shell, and a guard that
/// rejected these would refuse comments operators really write.
#[tokio::test]
pub(super) async fn shell_metacharacters_in_a_comment_render_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let comment = "a$b`c\\d\"e;f";

    reconciler
        .apply(&settings_with_keys(vec![raw_key(
            &canonical(REAL_ED25519_LINE),
            Some(comment),
        )]))
        .await
        .expect("shell metacharacters are legitimate comment text");

    assert_eq!(
        std::fs::read_to_string(&paths.keys).unwrap(),
        format!("{} {comment}\n", canonical(REAL_ED25519_LINE))
    );
}

// ---- password authentication gating -----------------------------------

//! A hostile store, and a panic beneath discovery.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::UnixListener;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};

use super::*;

/// Build one hostile store per name. Every one of these is a state the
/// daemon must hold; none of them may be an error it returns.
pub(super) fn hostile(base: &Path) -> Vec<(&'static str, Store)> {
    let mut cases: Vec<(&'static str, Store)> = Vec::new();
    let mut root = |name: &'static str| -> PathBuf {
        let path = base.join(name);
        cases.push((name, Store::new(&path)));
        path
    };

    root("absent-root");

    let path = root("root-is-a-regular-file");
    fs::write(&path, b"not a directory").expect("write file");

    let path = root("root-is-a-dangling-symlink");
    symlink(base.join("nowhere-at-all"), &path).expect("symlink");

    let path = root("root-is-unreadable");
    fs::create_dir_all(&path).expect("create root");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let path = root("bundles-is-a-regular-file");
    fs::create_dir_all(&path).expect("create root");
    fs::write(path.join("bundles"), b"x").expect("write file");
    symlink("bundles/1", path.join("current")).expect("symlink");

    let path = root("current-dangles-into-nowhere");
    fs::create_dir_all(path.join("bundles")).expect("create bundles");
    symlink("bundles/7", path.join("current")).expect("symlink");

    let path = root("current-points-outside-the-store");
    fs::create_dir_all(&path).expect("create root");
    symlink("/proc/self/root/nonexistent/1", path.join("current")).expect("symlink");

    let path = root("current-is-a-regular-file");
    fs::create_dir_all(&path).expect("create root");
    fs::write(path.join("current"), b"not a symlink").expect("write file");

    let path = root("current-is-a-directory");
    fs::create_dir_all(path.join("current")).expect("create dir");

    let path = root("current-target-is-not-numeric");
    fs::create_dir_all(path.join("bundles/latest")).expect("create bundles");
    symlink("bundles/latest", path.join("current")).expect("symlink");

    let path = root("generation-is-a-regular-file");
    fs::create_dir_all(path.join("bundles")).expect("create bundles");
    fs::write(path.join("bundles/1"), b"x").expect("write file");
    symlink("bundles/1", path.join("current")).expect("symlink");

    let path = root("tree-contains-a-socket");
    fs::create_dir_all(path.join("bundles/1")).expect("create bundles");
    fs::write(path.join("bundles/1/index.html"), b"<!doctype html>").expect("write index");
    UnixListener::bind(path.join("bundles/1/sock")).expect("bind socket");
    symlink("bundles/1", path.join("current")).expect("symlink");

    let path = root("record-is-garbage");
    fs::create_dir_all(path.join("bundles/1")).expect("create bundles");
    fs::create_dir_all(path.join("records")).expect("create records");
    fs::write(path.join("bundles/1/index.html"), b"<!doctype html>").expect("write index");
    fs::write(path.join("records/1.json"), b"}{ not json").expect("write record");
    symlink("bundles/1", path.join("current")).expect("symlink");

    let path = root("record-is-a-directory");
    fs::create_dir_all(path.join("bundles/1")).expect("create bundles");
    fs::create_dir_all(path.join("records/1.json")).expect("create records");
    fs::write(path.join("bundles/1/index.html"), b"<!doctype html>").expect("write index");
    symlink("bundles/1", path.join("current")).expect("symlink");

    let path = root("manifest-is-garbage");
    fs::create_dir_all(path.join("bundles/1")).expect("create bundles");
    fs::write(path.join("bundles/1/index.html"), b"<!doctype html>").expect("write index");
    fs::write(path.join("bundles/1/mica-ui.json"), b"\x00\xff not json").expect("write manifest");
    symlink("bundles/1", path.join("current")).expect("symlink");

    let path = root("manifest-declares-no-versions");
    fs::create_dir_all(path.join("bundles/1")).expect("create bundles");
    fs::write(path.join("bundles/1/index.html"), b"<!doctype html>").expect("write index");
    fs::write(
        path.join("bundles/1/mica-ui.json"),
        br#"{"name":"n","version":"1","immutableDir":"a","apiVersions":[]}"#,
    )
    .expect("write manifest");
    symlink("bundles/1", path.join("current")).expect("symlink");

    let path = root("staged-tree-contains-a-symlink");
    fs::create_dir_all(path.join(".staging-3")).expect("create staging");
    fs::write(path.join(".staging-3/index.html"), b"<!doctype html>").expect("write index");
    symlink("/etc/passwd", path.join(".staging-3/leak")).expect("symlink");

    let path = root("staged-generation-overflows-u64");
    fs::create_dir_all(path.join(".staging-99999999999999999999")).expect("create staging");

    let path = root("staging-is-a-regular-file");
    fs::create_dir_all(&path).expect("create root");
    fs::write(path.join(".staging-1"), b"x").expect("write file");

    cases
}

/// The safety claim: a bad bundle cannot take the listener down.
///
/// Every hostile store below is run through the real entry point. The
/// assertion is a named set — which roots returned normally, diffed
/// against which roots were built — so a regression names the root that
/// broke rather than reporting a count. `catch_unwind` is what makes the
/// diff possible: without it the first panic would abort the test and the
/// remaining roots would never be tried.
#[test]
pub(super) fn no_hostile_store_can_stop_start_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cases = hostile(dir.path());
    let expected: BTreeSet<&str> = cases.iter().map(|(name, _)| *name).collect();
    assert!(!expected.is_empty());

    let mut returned = BTreeSet::new();
    let mut states = Vec::new();
    for (name, store) in &cases {
        // The panic hook is left in place on purpose: a failure here must
        // print where it panicked, not only that it did.
        if let Ok(state) = std::panic::catch_unwind(AssertUnwindSafe(|| {
            run(store, &crate::audit::Audit::journal_only())
        })) {
            returned.insert(*name);
            states.push((*name, state));
        }
    }

    let missing: Vec<&&str> = expected.difference(&returned).collect();
    assert!(
        missing.is_empty(),
        "start-up did not return for {missing:?}; a bundle that can do this is a crash loop with no listener bound"
    );

    // Every outcome is one of the four defined states -- guaranteed by the
    // return type, and printed here so a regression is readable.
    let mut report = String::new();
    for (name, state) in &states {
        writeln!(report, "  {name}: {state}").expect("format");
    }
    assert_eq!(states.len(), cases.len(), "states:\n{report}");

    // Clean-up: the 0o000 root would otherwise defeat TempDir's own
    // recursive remove on a non-root test runner.
    let unreadable = dir.path().join("root-is-unreadable");
    if unreadable.exists() {
        let _ = fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o755));
    }
}

/// The same property for the async entry point `main` actually calls, and
/// one layer deeper: a panic raised *below* this module -- in code it only
/// calls -- arrives as a `JoinError` and becomes a state.
#[tokio::test]
pub(super) async fn a_panic_beneath_discovery_becomes_a_state_and_not_an_unwind() {
    let joined = tokio::task::spawn_blocking(|| -> BundleState {
        panic!("deliberate: stands in for a panic in code `run` calls")
    })
    .await;
    assert!(joined.is_err(), "the fixture must actually panic");

    let (state, log) = capture(|| join(joined));
    assert!(
        matches!(state, BundleState::Unavailable { .. }),
        "a panic must become a state, got {state}"
    );
    names(&log, &["did not complete"], "the panic is logged");
}

/// `discover` is what `main` calls, so it is exercised end to end at least
/// once rather than only through its synchronous body.
#[tokio::test]
pub(super) async fn discover_holds_the_state_for_a_real_store() {
    let (_dir, store) = store();
    activate(&store, 1, &["v1"], SERVED_API_VERSIONS);
    let state = discover(store.clone(), journal_audit()).await;
    assert!(
        matches!(state, BundleState::Active { generation: 1, .. }),
        "got {state}"
    );
    assert!(!state.to_string().is_empty());

    let (_dir2, empty) = fresh();
    assert_eq!(discover(empty, journal_audit()).await, BundleState::BuiltIn);
}

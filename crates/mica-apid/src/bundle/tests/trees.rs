//! What a staged tree may hold.

use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::os::unix::net::UnixListener;
use std::process::Command;

use super::*;

/// Absence of the root is a defined state, and the answer is named rather
/// than empty.
#[test]
pub(super) fn absent_root_reads_as_the_built_in_ui() {
    let (_dir, store) = store();
    assert!(!store.root().exists());
    let installed = store.status().expect("status");
    assert_eq!(installed, Installed::BuiltIn);
}

/// The root is created by the install path, never at start-up.
#[test]
pub(super) fn reading_status_never_creates_the_root() {
    let (_dir, store) = store();
    store.status().expect("status");
    store.discover_staged().expect("discover");
    store.recheck_active(&SERVED).expect("recheck");
    assert!(!store.root().exists());
}

/// Class 2, first half.
#[test]
pub(super) fn rejects_a_tree_with_no_index() {
    let (_dir, store) = store();
    let staging = store.staging_dir(1);
    fs::create_dir_all(&staging).expect("create staging");
    fs::write(staging.join("app.js"), b"x").expect("write asset");
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    assert_eq!(rejection(&err), Rejection::MissingIndex);
    assert_eq!(store.status().expect("status"), Installed::BuiltIn);
}

/// Class 2, second half: "or whose index is a directory".
#[test]
pub(super) fn rejects_a_tree_whose_index_is_a_directory() {
    let (_dir, store) = store();
    let staging = store.staging_dir(1);
    fs::create_dir_all(staging.join(INDEX_NAME)).expect("create index dir");
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    assert_eq!(rejection(&err), Rejection::IndexNotRegularFile);
}

/// Rule 2 / rule 5: a symlink is removed from the tree at
/// unpack, before anything is reachable.
#[test]
pub(super) fn rejects_a_staged_tree_containing_a_symlink() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    symlink("/etc/passwd", staging.join("assets/passwd")).expect("symlink");
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    assert_eq!(
        rejection(&err),
        Rejection::IrregularEntry {
            path: PathBuf::from("assets/passwd"),
            kind: EntryKind::Symlink,
        }
    );
    assert_eq!(store.status().expect("status"), Installed::BuiltIn);
}

/// Rule 2: no FIFOs.
#[test]
pub(super) fn rejects_a_staged_tree_containing_a_fifo() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    let fifo = staging.join("assets/pipe");
    let status = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(status.success(), "mkfifo failed: {status}");
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    assert_eq!(
        rejection(&err),
        Rejection::IrregularEntry {
            path: PathBuf::from("assets/pipe"),
            kind: EntryKind::Fifo,
        }
    );
}

/// Rule 2: no sockets.
#[test]
pub(super) fn rejects_a_staged_tree_containing_a_socket() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    let socket = staging.join("assets/sock");
    let listener = UnixListener::bind(&socket).expect("bind unix socket");
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    drop(listener);
    assert_eq!(
        rejection(&err),
        Rejection::IrregularEntry {
            path: PathBuf::from("assets/sock"),
            kind: EntryKind::Socket,
        }
    );
}

/// Rule 2: no hardlinks. A hardlink is a regular file, so the only
/// thing that distinguishes it is its link count.
#[test]
pub(super) fn rejects_a_staged_tree_containing_a_hardlink() {
    let (dir, store) = store();
    let staging = stage_valid(&store, 1);
    let outside = dir.path().join("outside.txt");
    fs::write(&outside, b"secret").expect("write outside file");
    fs::hard_link(&outside, staging.join("assets/link")).expect("hard link");
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    assert_eq!(
        rejection(&err),
        Rejection::Hardlink {
            path: PathBuf::from("assets/link"),
            links: 2,
        }
    );
}

/// A bundle with no manifest is valid. Degradation, not rejection.
#[test]
pub(super) fn a_tree_without_a_manifest_activates_unchecked() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    let activation = store.activate(1, &SERVED).expect("activate");
    assert_eq!(activation.compat, CompatCheck::NotRun);
    assert_eq!(activation.manifest, None);
    assert_eq!(
        fs::read_link(store.current_link()).expect("read current"),
        PathBuf::from("bundles/1")
    );

    let installed = store.status().expect("status");
    let ui = custom(&installed);
    assert_eq!(ui.generation, 1);
    assert_eq!(ui.manifest, None);
    assert!(ui.index_readable);
    let recorded = ui.recorded.as_ref().expect("an activation record");
    assert_eq!(recorded.compat, CompatCheck::NotRun);
    assert!(recorded.digest_matches);
}

/// 0755 on directories, 0644 on files, on the installed tree.
#[test]
pub(super) fn the_installed_tree_carries_the_layout_modes() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    fs::set_permissions(
        staging.join("assets/app.js"),
        fs::Permissions::from_mode(0o600),
    )
    .expect("chmod asset");
    store.activate(1, &SERVED).expect("activate");
    let bundle = store.bundle_dir(1);
    let mode = |path: PathBuf| {
        fs::symlink_metadata(path)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode(bundle.clone()), DIR_MODE);
    assert_eq!(mode(bundle.join("assets")), DIR_MODE);
    assert_eq!(mode(bundle.join("assets/app.js")), FILE_MODE);
    assert_eq!(mode(bundle.join(INDEX_NAME)), FILE_MODE);
}

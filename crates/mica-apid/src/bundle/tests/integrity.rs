//! Uploads, digests and refused activations.

use std::os::unix::fs::symlink;

use super::*;

#[test]
pub(super) fn upload_refuses_a_reused_name_and_version_with_different_content() {
    let (_dir, store) = store();
    let first = stage_valid(&store, 1);
    write_manifest(&first, &["v1"]);
    store.install(1, &SERVED, 100, 200).expect("install first");

    let second = stage_valid(&store, 2);
    write_manifest(&second, &["v1"]);
    fs::write(second.join("assets/app.js"), b"different").expect("change second package");
    let err = store
        .install(2, &SERVED, 100, 200)
        .expect_err("same identity with different content must fail");
    assert_eq!(
        rejection(&err),
        Rejection::NameVersionConflict {
            name: "demo".to_string(),
            version: "1.2.3".to_string(),
            generation: 1,
        }
    );
    assert!(!store.bundle_dir(2).exists());
}

#[test]
pub(super) fn upload_refuses_a_thirty_third_retained_generation() {
    let (_dir, store) = store();
    fs::create_dir_all(store.bundles_dir()).expect("create retained root");
    for generation in 1..=32 {
        fs::create_dir(store.bundle_dir(generation)).expect("create retained generation");
    }
    let staging = stage_valid(&store, 33);
    write_manifest(&staging, &["v1"]);
    let err = store
        .install(33, &SERVED, 100, 200)
        .expect_err("retention limit must fail closed");
    assert_eq!(rejection(&err), Rejection::RetentionLimit(32));
    assert!(!store.bundle_dir(33).exists());
}

/// Class 3: a corruption written in over a root shell is detected by
/// the digest, at activation and at start-up — not per request.
#[test]
pub(super) fn the_digest_recheck_detects_a_file_mutated_after_activation() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    assert_eq!(
        store
            .recheck_active(&SERVED)
            .expect("recheck")
            .expect("active")
            .digest_matches,
        Some(true)
    );

    fs::write(store.bundle_dir(1).join("assets/app.js"), b"console.log(2)")
        .expect("mutate the installed tree");

    let recheck = store
        .recheck_active(&SERVED)
        .expect("recheck")
        .expect("active");
    assert_eq!(recheck.digest_matches, Some(false));
    assert!(recheck.corrupt());
    let installed = store.status().expect("status");
    assert!(
        !custom(&installed)
            .recorded
            .as_ref()
            .expect("record")
            .digest_matches
    );
}

/// A tree that no longer walks — a symlink planted under `bundles/` — is a
/// digest mismatch rather than an error, because start-up must hold a
/// state and never return one.
#[test]
pub(super) fn a_symlink_planted_after_activation_reads_as_a_mismatch() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    symlink("/etc/passwd", store.bundle_dir(1).join("passwd")).expect("symlink");
    let recheck = store
        .recheck_active(&SERVED)
        .expect("recheck")
        .expect("active");
    assert_eq!(recheck.digest_matches, Some(false));
}

/// Validation runs on the staged tree and never on the live one, so
/// a refusal leaves the previously active bundle active and untouched.
#[test]
pub(super) fn a_refused_activation_leaves_the_active_bundle_untouched() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    let first = store.activate(1, &SERVED).expect("activate");

    // Generation 2 is staged with no index.html.
    let staging = store.staging_dir(2);
    fs::create_dir_all(&staging).expect("create staging");
    fs::write(staging.join("app.js"), b"x").expect("write asset");
    let err = store.activate(2, &SERVED).expect_err("must be refused");
    assert_eq!(rejection(&err), Rejection::MissingIndex);

    assert_eq!(
        fs::read_link(store.current_link()).expect("read current"),
        PathBuf::from("bundles/1")
    );
    let installed = store.status().expect("status");
    let ui = custom(&installed);
    assert_eq!(ui.generation, 1);
    assert!(ui.index_readable);
    let recorded = ui.recorded.as_ref().expect("record");
    assert!(recorded.digest_matches);
    assert_eq!(recorded.digest, first.digest);
    // The rejected tree is still staged, and never reached bundles/.
    assert!(!store.bundle_dir(2).exists());
    assert!(staging.join("app.js").exists());
}

/// Activation never overwrites an installed generation.
#[test]
pub(super) fn activating_an_existing_generation_is_refused() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    stage_valid(&store, 1);
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    assert_eq!(rejection(&err), Rejection::GenerationExists(1));
}

/// The second local install path: apid picks up a staged
/// directory.
#[test]
pub(super) fn pick_up_staged_activates_the_highest_staged_generation() {
    let (_dir, store) = store();
    assert!(
        store
            .pick_up_staged(&SERVED)
            .expect("nothing staged")
            .is_none()
    );

    stage_valid(&store, 2);
    stage_valid(&store, 7);
    assert_eq!(store.discover_staged().expect("discover"), vec![2, 7]);
    let activation = store
        .pick_up_staged(&SERVED)
        .expect("pick up")
        .expect("something staged");
    assert_eq!(activation.generation, 7);
    assert_eq!(custom(&store.status().expect("status")).generation, 7);
    assert_eq!(store.discover_staged().expect("discover"), vec![2]);
}

/// The digest is a property of the tree's contents and paths, not of the
/// order the filesystem returned them in.
#[test]
pub(super) fn the_digest_is_stable_across_two_identical_trees() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    let first = store.activate(1, &SERVED).expect("activate");
    stage_valid(&store, 2);
    let second = store.activate(2, &SERVED).expect("activate");
    assert_eq!(first.digest, second.digest);
}

//! Deactivation, selection and deletion.

use std::os::unix::fs::symlink;

use super::*;

/// Deactivate removes the pointer and keeps the tree.
#[test]
pub(super) fn deactivate_keeps_the_bundle_on_disk() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");

    assert!(store.deactivate().expect("deactivate"));
    assert_eq!(store.status().expect("status"), Installed::BuiltIn);
    assert!(store.bundle_dir(1).join(INDEX_NAME).exists());
    assert!(!store.current_link().exists());
    // Idempotent: deactivating twice is not an error.
    assert!(!store.deactivate().expect("deactivate again"));
}

#[test]
pub(super) fn a_deactivated_bundle_is_reported_and_selected_only_after_revalidation() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    write_manifest(&staging, &["v1"]);
    store.activate(1, &SERVED).expect("activate");
    store.deactivate().expect("deactivate");

    let candidate = store
        .available_custom(&SERVED)
        .expect("inspect retained bundles")
        .expect("a retained bundle");
    assert_eq!(candidate.generation, 1);
    assert!(candidate.usable);
    assert_eq!(candidate.unavailable_reason, None);
    assert_eq!(candidate.digest_matches, Some(true));
    assert_eq!(candidate.compatible, Some(true));

    let selected = store
        .select_available_custom(&SERVED)
        .expect("select retained bundle")
        .expect("a usable retained bundle");
    assert!(selected.changed);
    assert_eq!(selected.candidate, candidate);
    assert_eq!(custom(&store.status().expect("status")).generation, 1);

    let selected_again = store
        .select_available_custom(&SERVED)
        .expect("select retained bundle again")
        .expect("the active bundle remains usable");
    assert!(!selected_again.changed);
}

#[test]
pub(super) fn a_corrupt_retained_bundle_is_visible_but_cannot_be_selected() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    store.deactivate().expect("deactivate");
    fs::write(store.bundle_dir(1).join("assets/app.js"), b"changed")
        .expect("corrupt retained bundle");

    let candidate = store
        .available_custom(&SERVED)
        .expect("inspect retained bundles")
        .expect("the unusable bundle remains visible");
    assert!(!candidate.usable);
    assert_eq!(
        candidate.unavailable_reason,
        Some(CandidateUnavailable::DigestMismatch)
    );
    assert_eq!(candidate.digest_matches, Some(false));
    assert!(
        store
            .select_available_custom(&SERVED)
            .expect("selection is a state, not an error")
            .is_none()
    );
    assert_eq!(store.active_generation().expect("read current"), None);
}

#[test]
pub(super) fn a_retained_bundle_is_rechecked_against_the_current_served_api_set() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    write_manifest(&staging, &["v1"]);
    store.activate(1, &SERVED).expect("activate");
    store.deactivate().expect("deactivate");

    let candidate = store
        .available_custom(&["v9"])
        .expect("inspect against the new served set")
        .expect("the incompatible bundle remains visible");
    assert!(!candidate.usable);
    assert_eq!(candidate.digest_matches, Some(true));
    assert_eq!(candidate.compatible, Some(false));
    assert_eq!(
        candidate.unavailable_reason,
        Some(CandidateUnavailable::Incompatible)
    );
    assert!(
        store
            .select_available_custom(&["v9"])
            .expect("selection is a state, not an error")
            .is_none()
    );
}

#[test]
pub(super) fn selection_skips_a_newer_unusable_generation() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate generation 1");
    stage_valid(&store, 2);
    store.activate(2, &SERVED).expect("activate generation 2");
    store.deactivate().expect("deactivate");
    fs::write(store.bundle_dir(2).join(INDEX_NAME), b"changed").expect("corrupt newest generation");

    let candidate = store
        .available_custom(&SERVED)
        .expect("inspect retained bundles")
        .expect("generation 1 is still usable");
    assert_eq!(candidate.generation, 1);
    assert!(candidate.usable);
    let selected = store
        .select_available_custom(&SERVED)
        .expect("select retained bundle")
        .expect("generation 1 is selectable");
    assert_eq!(selected.candidate.generation, 1);
}

#[test]
pub(super) fn selection_repairs_a_pointer_that_only_looks_like_the_generation() {
    let (dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    store.deactivate().expect("deactivate");
    let unmanaged = dir.path().join("unmanaged/1");
    fs::create_dir_all(&unmanaged).expect("create unmanaged tree");
    symlink(&unmanaged, store.current_link()).expect("plant unmanaged current pointer");

    let selected = store
        .select_available_custom(&SERVED)
        .expect("select retained bundle")
        .expect("the managed bundle is usable");
    assert!(selected.changed);
    assert_eq!(
        fs::read_link(store.current_link()).expect("read repaired pointer"),
        PathBuf::from("bundles/1")
    );
}

/// Deactivate is reversible — that is what keeping two generations
/// buys.
#[test]
pub(super) fn a_deactivated_bundle_can_be_reactivated_from_disk() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    store.deactivate().expect("deactivate");
    store.point_current_at(1).expect("re-point current");
    assert_eq!(custom(&store.status().expect("status")).generation, 1);
}

/// Delete after deactivate removes the tree.
#[test]
pub(super) fn delete_after_deactivate_removes_the_tree() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    store.deactivate().expect("deactivate");
    store.delete(1).expect("delete");
    assert!(!store.bundle_dir(1).exists());
    assert!(!store.trash_dir(1).exists());
    assert_eq!(store.generations().expect("generations"), Vec::<u64>::new());
}

/// Never unlink the tree `current` points at.
#[test]
pub(super) fn delete_while_active_is_refused() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    let err = store.delete(1).expect_err("must be refused");
    assert_eq!(rejection(&err), Rejection::DeleteWhileActive(1));
    assert!(store.bundle_dir(1).join(INDEX_NAME).exists());
    assert_eq!(custom(&store.status().expect("status")).generation, 1);
}

/// Installed versions remain until an operator explicitly deletes them.
#[test]
pub(super) fn activation_never_prunes_retained_generations() {
    let (_dir, store) = store();
    for generation in 1..=4 {
        stage_valid(&store, generation);
        store.activate(generation, &SERVED).expect("activate");
    }
    assert_eq!(store.generations().expect("generations"), vec![1, 2, 3, 4]);
    assert_eq!(custom(&store.status().expect("status")).generation, 4);
    assert!(store.bundle_dir(3).join(INDEX_NAME).exists());
    assert!(store.record_path(2).exists());
    assert!(store.record_path(3).exists());
}

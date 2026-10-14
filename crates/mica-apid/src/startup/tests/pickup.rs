//! An absent root and a staged directory at start-up.

use std::fs;

use super::*;

/// "This is not an error -- it is the shipped state of every device, and
/// it must not be logged as one." Asserted in both directions: the state
/// is named, and the log carries no WARN and no ERROR.
#[test]
pub(super) fn an_absent_srv_ui_is_normal_operation_and_not_an_error() {
    let (_dir, store) = store();
    assert!(!store.root().exists());

    let (state, log) = capture(|| run(&store, &crate::audit::Audit::journal_only()));
    assert_eq!(state, BundleState::BuiltIn);
    names(
        &log,
        &[crate::bundle::NO_CUSTOM_BUNDLE],
        "the no-bundle state must be a named answer",
    );
    assert!(
        !log.contains("WARN") && !log.contains("ERROR"),
        "class 1 must not be logged as an error\n--- log ---\n{log}"
    );
    assert!(
        !store.root().exists(),
        "the root is created by the install path, never at start-up"
    );
}

// The staged-directory install path.

/// "an operator places a tree on the device and calls an activate
/// operation, **or apid picks up a staged directory**."
#[test]
pub(super) fn a_staged_directory_is_picked_up_at_start_up() {
    let (_dir, store) = store();
    let staging = stage(&store, 7);
    write_manifest(&staging, &["v1"]);
    assert_eq!(active(&store), None);

    let (state, log) = capture(|| run(&store, &crate::audit::Audit::journal_only()));
    assert!(
        matches!(state, BundleState::Active { generation: 7, .. }),
        "the staged tree must become the active bundle, got {state}"
    );
    assert_eq!(active(&store), Some(7));
    names(
        &log,
        &["picked up a staged UI bundle"],
        "the pick-up is logged",
    );
    assert!(!store.staging_dir(7).exists());
}

/// A staged tree that cannot be activated is logged and start-up carries
/// on: the already-active bundle is still evaluated and still active.
#[test]
pub(super) fn an_unactivatable_staged_tree_does_not_stop_the_recheck() {
    let (_dir, store) = store();
    activate(&store, 1, &["v1"], SERVED_API_VERSIONS);
    // Staged generation 9 has no index.html -- class 2, refused.
    fs::create_dir_all(store.staging_dir(9)).expect("create staging");

    let (state, log) = capture(|| run(&store, &crate::audit::Audit::journal_only()));
    assert!(
        matches!(state, BundleState::Active { generation: 1, .. }),
        "the active bundle must be unaffected, got {state}"
    );
    names(
        &log,
        &["could not be activated", "nothing else changed"],
        "the refusal is logged",
    );
    assert_eq!(active(&store), Some(1));
}

// The constraint.

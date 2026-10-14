//! The served API set against a bundle's manifest.

use super::*;

/// Class 5: a declared range that intersects the served set activates.
#[test]
pub(super) fn a_manifest_intersecting_the_served_set_activates() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    write_manifest(&staging, &["v2"]);
    let activation = store.activate(1, &SERVED).expect("activate");
    assert_eq!(
        activation.compat,
        CompatCheck::Ran {
            declared: vec!["v2".to_string()],
            served: vec!["v1".to_string(), "v2".to_string()],
            compatible: true,
        }
    );
    let installed = store.status().expect("status");
    let ui = custom(&installed);
    assert_eq!(
        ui.manifest,
        Some(ManifestSummary {
            name: "demo".to_string(),
            version: "1.2.3".to_string(),
        })
    );
}

/// Class 5: an empty intersection, and nothing else, refuses.
#[test]
pub(super) fn a_manifest_with_an_empty_intersection_is_refused() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    write_manifest(&staging, &["v0"]);
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    assert_eq!(
        rejection(&err),
        Rejection::Incompatible {
            declared: vec!["v0".to_string()],
            served: vec!["v1".to_string(), "v2".to_string()],
        }
    );
    assert_eq!(store.status().expect("status"), Installed::BuiltIn);
    assert!(!store.bundle_dir(1).exists());
}

/// The single most important negative test here: the relation is
/// **membership in the served set**, not equality with `current`. A bundle
/// built against the outgoing major must survive exactly the generation
/// the dual-major recommendation exists to protect.
#[test]
pub(super) fn a_manifest_matching_only_the_non_current_member_activates() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    // Served set ["v1", "v2"], `current` is "v2"; this bundle knows only v1.
    write_manifest(&staging, &["v1"]);
    let activation = store.activate(1, &SERVED).expect("must activate");
    assert_eq!(
        activation.compat,
        CompatCheck::Ran {
            declared: vec!["v1".to_string()],
            served: vec!["v1".to_string(), "v2".to_string()],
            compatible: true,
        }
    );
    let recheck = store
        .recheck_active(&SERVED)
        .expect("recheck")
        .expect("a bundle is active");
    assert!(matches!(
        recheck.compat,
        CompatCheck::Ran {
            compatible: true,
            ..
        }
    ));
}

/// The same bundle after an A/B update to an image serving only `v2`:
/// start-up's re-check now finds an empty intersection.
#[test]
pub(super) fn the_startup_recheck_finds_the_empty_intersection_after_an_update() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    write_manifest(&staging, &["v1"]);
    store.activate(1, &SERVED).expect("activate");

    let recheck = store
        .recheck_active(&["v2"])
        .expect("recheck")
        .expect("a bundle is active");
    assert_eq!(
        recheck.compat,
        CompatCheck::Ran {
            declared: vec!["v1".to_string()],
            served: vec!["v2".to_string()],
            compatible: false,
        }
    );
    // The re-check reports; it never deactivates on its own.
    assert!(store.current_link().exists());
}

/// A bundle that could not be checked is never reported incompatible.
#[test]
pub(super) fn the_startup_recheck_never_deactivates_an_unchecked_bundle() {
    let (_dir, store) = store();
    stage_valid(&store, 1);
    store.activate(1, &SERVED).expect("activate");
    let recheck = store
        .recheck_active(&["v9"])
        .expect("recheck")
        .expect("a bundle is active");
    assert_eq!(recheck.compat, CompatCheck::NotRun);
    assert!(!recheck.corrupt());
}

/// Step 2: a manifest that is present must parse.
#[test]
pub(super) fn a_present_but_unparsable_manifest_is_refused() {
    let (_dir, store) = store();
    let staging = stage_valid(&store, 1);
    fs::write(staging.join(MANIFEST_NAME), b"{not json").expect("write manifest");
    let err = store.activate(1, &SERVED).expect_err("must be refused");
    assert!(matches!(rejection(&err), Rejection::ManifestUnparsable(_)));
}

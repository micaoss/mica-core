//! The start-up recheck against the served set.

use crate::bundle::Installed;
use std::collections::BTreeSet;
use std::fs;

use super::*;

/// The shape of the served set: an array, with `current` always
/// a member. Declared by identity and diffed, never counted.
#[test]
pub(super) fn the_served_set_is_the_array_2_1_specifies() {
    let expected: BTreeSet<&str> = ["v1"].into_iter().collect();
    let observed: BTreeSet<&str> = SERVED_API_VERSIONS.iter().copied().collect();
    let missing: Vec<&&str> = expected.difference(&observed).collect();
    let unexpected: Vec<&&str> = observed.difference(&expected).collect();
    assert!(
        missing.is_empty() && unexpected.is_empty(),
        "served set: missing {missing:?}, unexpected {unexpected:?}"
    );
    assert_eq!(
        observed.len(),
        SERVED_API_VERSIONS.len(),
        "the served set carries a duplicate: {SERVED_API_VERSIONS:?}"
    );
    assert!(
        SERVED_API_VERSIONS.contains(&CURRENT_API_VERSION),
        "`current` is always a member of `versions`, but {CURRENT_API_VERSION:?} is not in {SERVED_API_VERSIONS:?}"
    );
}

/// The constant is not merely defined: `run` feeds it to the class-5
/// check. A bundle activated against a served set this binary does **not**
/// serve is deactivated by `run`, and the log names the constant's members
/// as the set it was compared against.
#[test]
pub(super) fn run_reads_the_served_set_constant() {
    let (_dir, store) = store();
    // Activated against a served set of ["v0"] — an earlier image. This
    // binary serves SERVED_API_VERSIONS, which does not contain it.
    activate(&store, 1, &["v0"], &["v0"]);
    assert_eq!(active(&store), Some(1));

    let (state, log) = capture(|| run(&store, &crate::audit::Audit::journal_only()));
    assert!(
        matches!(state, BundleState::Deactivated { generation: 1, .. }),
        "expected a deactivation, got {state}"
    );
    names(
        &log,
        &[r#"declared=["v0"]"#, r#"served=["v1"]"#],
        "the class-5 line must name the constant it compared against",
    );
    assert_eq!(active(&store), None);

    // And the other direction: a bundle declaring the constant's own
    // member survives `run`, so the constant is not being ignored.
    let (_dir2, keeper) = fresh();
    activate(&keeper, 1, &["v1"], SERVED_API_VERSIONS);
    assert!(
        matches!(
            run(&keeper, &crate::audit::Audit::journal_only()),
            BundleState::Active { generation: 1, .. }
        ),
        "a bundle declaring the served member must stay active"
    );
    assert_eq!(active(&keeper), Some(1));
}

// Class 5.

/// A non-empty intersection is not a deactivation trigger, however partial.
#[test]
pub(super) fn an_intersecting_range_stays_active() {
    let (_dir, store) = store();
    activate(&store, 1, &["v1", "v2", "v3"], &DUAL);
    let (state, log) = capture(|| evaluate(&store, &crate::audit::Audit::journal_only(), &DUAL));

    let BundleState::Active { generation, compat } = state else {
        panic!("expected the bundle to stay active, got {state}");
    };
    assert_eq!(generation, 1);
    let CompatCheck::Ran {
        declared,
        served,
        compatible,
    } = compat
    else {
        panic!("expected the check to have run");
    };
    assert!(compatible);
    // Named set, not a count: *which* members intersected.
    let intersection: BTreeSet<&str> = declared
        .iter()
        .map(String::as_str)
        .filter(|v| served.iter().any(|s| s == v))
        .collect();
    let expected: BTreeSet<&str> = ["v1", "v2"].into_iter().collect();
    assert_eq!(
        intersection, expected,
        "declared {declared:?} against served {served:?}"
    );
    assert_eq!(active(&store), Some(1));
    assert!(
        !log.contains("deactivat"),
        "nothing may be deactivated\n{log}"
    );
}

/// The outgoing major is served alongside the new one for one image
/// generation, and an escape hatch that fires on the
/// wrong condition is worse than one that does not exist. A bundle that matches
/// only the outgoing major must stay active — equality against `current`
/// would remove exactly the bundles the recommendation exists to protect.
#[test]
pub(super) fn a_bundle_matching_only_the_outgoing_major_stays_active() {
    let (_dir, store) = store();
    activate(&store, 1, &["v1"], &DUAL);

    let state = evaluate(&store, &crate::audit::Audit::journal_only(), &DUAL);
    let BundleState::Active { generation, compat } = state else {
        panic!("a bundle matching the outgoing major must stay active, got {state}");
    };
    assert_eq!(generation, 1);
    let CompatCheck::Ran {
        declared, served, ..
    } = &compat
    else {
        panic!("expected the check to have run");
    };

    // The intersection is exactly {v1} -- the outgoing major, by name.
    let intersection: BTreeSet<&str> = declared
        .iter()
        .map(String::as_str)
        .filter(|v| served.iter().any(|s| s == v))
        .collect();
    assert_eq!(intersection, ["v1"].into_iter().collect::<BTreeSet<&str>>());

    // And the control that makes the test mean what it claims: `current`
    // is *not* in the declared range, so an implementation written as
    // equality-with-`current` would have deactivated this bundle.
    assert!(
        !declared.iter().any(|v| v == DUAL_CURRENT),
        "the fixture is wrong: {declared:?} contains `current` {DUAL_CURRENT:?}, so this test could pass under an equality check"
    );
    assert!(served.iter().any(|v| v == DUAL_CURRENT));
    assert_eq!(active(&store), Some(1));
}

/// The sole class-5 trigger: an empty intersection. This is the A/B
/// case — activated against the old image's API, re-checked against the
/// new image's, which is *"the only moment at which anything on the device
/// is in a position to notice."*
#[test]
pub(super) fn an_empty_intersection_deactivates_and_the_log_names_both_sets() {
    let (_dir, store) = store();
    activate(&store, 1, &["v1"], &["v1"]);
    assert_eq!(active(&store), Some(1));

    // The A/B update: this slot serves v2 only.
    let (state, log) = capture(|| evaluate(&store, &crate::audit::Audit::journal_only(), &["v2"]));

    let BundleState::Deactivated {
        generation,
        reasons,
        removed,
    } = state
    else {
        panic!("an empty intersection must deactivate, got {state}");
    };
    assert_eq!(generation, 1);
    assert!(removed);
    assert_eq!(
        reasons,
        vec![Reason::Incompatible {
            declared: vec!["v1".to_string()],
            served: vec!["v2".to_string()],
        }]
    );
    names(
        &log,
        &[r#"declared=["v1"]"#, r#"served=["v2"]"#],
        "the log line requires both the declared range and the served set in the line",
    );
    assert_eq!(active(&store), None, "the pointer must be gone");
    assert!(
        store.bundle_dir(1).is_dir(),
        "deactivate removes the pointer, never the tree"
    );
}

/// "A bundle with no manifest cannot be checked, so the correct
/// behaviour is to activate it and record that it was activated
/// unchecked." Degradation, not rejection.
#[test]
pub(super) fn a_bundle_with_no_manifest_stays_active_and_is_recorded_unchecked() {
    let (_dir, store) = store();
    stage(&store, 1);
    store.activate(1, SERVED_API_VERSIONS).expect("activate");

    let (state, log) = capture(|| run(&store, &crate::audit::Audit::journal_only()));
    assert_eq!(
        state,
        BundleState::Active {
            generation: 1,
            compat: CompatCheck::NotRun
        }
    );
    names(&log, &["unchecked"], "the unchecked state must be named");
    assert_eq!(active(&store), Some(1));

    // And it is recorded on disk as unchecked, not merely reported.
    let Installed::Custom(ui) = store.status().expect("status") else {
        panic!("expected an active bundle");
    };
    assert_eq!(
        ui.recorded.expect("an activation record").compat,
        CompatCheck::NotRun
    );
}

// Class 3.

/// Class 3's second reachable cause: "an operator writing into `/mica/ui`
/// over a root shell". Detected at the next restart -- which is this
/// module -- and not per request.
#[test]
pub(super) fn a_digest_mismatch_deactivates_and_logs() {
    let (_dir, store) = store();
    activate(&store, 1, &["v1"], SERVED_API_VERSIONS);
    fs::write(store.bundle_dir(1).join("planted.js"), b"root shell").expect("plant a file");

    let (state, log) = capture(|| run(&store, &crate::audit::Audit::journal_only()));
    let BundleState::Deactivated {
        generation,
        reasons,
        removed,
    } = state
    else {
        panic!("a digest mismatch must deactivate, got {state}");
    };
    assert_eq!(generation, 1);
    assert!(removed);
    assert_eq!(reasons, vec![Reason::DigestMismatch]);
    names(
        &log,
        &["digest recorded at activation", "deactivating"],
        "class 3 requires the mismatch to be logged",
    );
    assert_eq!(active(&store), None);
    assert!(store.bundle_dir(1).is_dir());
}

// Class 1.

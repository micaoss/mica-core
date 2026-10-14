//! Bind readiness and probes.

use super::*;

/// The readiness contract, driven case by case. The two
/// `Unavailable` cases are the ones that matter: each is a state in which
/// a writer must NOT fall back to another filesystem, so neither may be
/// softened to `degraded`.
#[test]
pub(super) fn a_bind_is_ready_only_when_it_is_actually_data_and_writable() {
    let data = data_tier("/dev/mmcblk0p11");
    assert_eq!(
        classify_readiness(&bound("/dev/mmcblk0p11"), Some(&data), Pressure::Normal),
        Readiness::Ready
    );

    // Not mounted at all.
    let unmounted = BindEvidence {
        mount: None,
        ..bound("/dev/mmcblk0p11")
    };
    assert_eq!(
        classify_readiness(&unmounted, Some(&data), Pressure::Normal),
        Readiness::Unavailable
    );

    // Mounted, but from something that is not the DATA partition: the
    // path is right and the filesystem is wrong, which is exactly the
    // forbidden fallback.
    assert_eq!(
        classify_readiness(&bound("/dev/mmcblk0p9"), Some(&data), Pressure::Normal),
        Readiness::Unavailable
    );

    // The source under /mnt/data is a symlink, not a directory:
    // substitution, which mica-data-layout dies on.
    let substituted = BindEvidence {
        source_is_directory: Some(false),
        ..bound("/dev/mmcblk0p11")
    };
    assert_eq!(
        classify_readiness(&substituted, Some(&data), Pressure::Normal),
        Readiness::Unavailable
    );

    // Read-only, a failed probe and a critically full pool are each
    // degraded: the namespace is the right one, it just cannot be
    // written now.
    let read_only = BindEvidence {
        mount: Some(MountEvidence {
            read_only: true,
            ..mounted("/dev/mmcblk0p11", "/mica")
        }),
        ..bound("/dev/mmcblk0p11")
    };
    assert_eq!(
        classify_readiness(&read_only, Some(&data), Pressure::Normal),
        Readiness::Degraded
    );
    let failed = BindEvidence {
        probe: Some(ProbeOutcome::Failed("ENOSPC".to_string())),
        ..bound("/dev/mmcblk0p11")
    };
    assert_eq!(
        classify_readiness(&failed, Some(&data), Pressure::Normal),
        Readiness::Degraded
    );
    assert_eq!(
        classify_readiness(&bound("/dev/mmcblk0p11"), Some(&data), Pressure::Critical),
        Readiness::Degraded
    );

    // A probe that did not run never upgrades a verdict and never
    // downgrades one: it is silence, and silence is neither.
    let not_attempted = BindEvidence {
        probe: Some(ProbeOutcome::NotAttempted("no subtree".to_string())),
        ..bound("/dev/mmcblk0p11")
    };
    assert_eq!(
        classify_readiness(&not_attempted, Some(&data), Pressure::Normal),
        Readiness::Ready
    );

    // No DATA tier to compare against: unknown, not ready. Claiming
    // readiness here would rest on a comparison nobody made.
    assert_eq!(
        classify_readiness(&bound("/dev/mmcblk0p11"), None, Pressure::Normal),
        Readiness::Unknown
    );
}

/// A probe is rendered as what it was. The `NotAttempted` case must never
/// serialize anything a client could read as a pass.
#[test]
pub(super) fn a_probe_that_did_not_run_never_renders_as_passed() {
    let render = |probe: ProbeOutcome| {
        bind_json(
            &BINDS[0],
            Some(&BindEvidence {
                probe: Some(probe),
                ..bound("/dev/mmcblk0p11")
            }),
            Some(&data_tier("/dev/mmcblk0p11")),
            Pressure::Normal,
        )["probe"]
            .clone()
    };
    assert_eq!(render(ProbeOutcome::Passed)["passed"], true);
    assert_eq!(
        render(ProbeOutcome::Failed("EROFS".into()))["passed"],
        false
    );
    assert_eq!(
        render(ProbeOutcome::Failed("EROFS".into()))["error"],
        "EROFS"
    );

    let skipped = render(ProbeOutcome::NotAttempted(
        "mica-data-layout has not run".into(),
    ));
    assert_eq!(skipped["attempted"], false);
    assert_eq!(skipped.get("passed"), None, "{skipped}");
    assert_eq!(skipped["reason"], "mica-data-layout has not run");
}

/// `sourceOnData` answers the contract's first question on its own,
/// rather than only through the verdict.
#[test]
pub(super) fn a_bind_reports_whether_its_source_is_really_data() {
    let data = data_tier("/dev/mmcblk0p11");
    let right = bind_json(
        &BINDS[0],
        Some(&bound("/dev/mmcblk0p11")),
        Some(&data),
        Pressure::Normal,
    );
    assert_eq!(right["sourceOnData"], true);
    assert_eq!(right["readiness"], "ready");
    assert_eq!(right["source"], "/mnt/data/mica");
    assert_eq!(right["owner"], "system");

    let wrong = bind_json(
        &BINDS[0],
        Some(&bound("/dev/mmcblk0p9")),
        Some(&data),
        Pressure::Normal,
    );
    assert_eq!(wrong["sourceOnData"], false);
    assert_eq!(wrong["readiness"], "unavailable");

    // No evidence at all is `unknown` with a reason, not a quiet default.
    let unseen = bind_json(&BINDS[1], None, Some(&data), Pressure::Normal);
    assert_eq!(unseen["readiness"], "unknown");
    assert!(unseen["detail"].as_str().is_some(), "{unseen}");
}

//! What the driver installs, discards or leaves alone.

use crate::time_status::SyncStatus;

use super::*;

#[tokio::test]
pub(super) async fn a_native_catalog_with_no_newer_generation_starts_no_download_or_install() {
    let mut scene = Scene::auto(&open_window(), "window");
    for _ in 0..2 {
        scene.daemon.will_check(Ok(Settled::NoneCompatible));
        scene.cadence.advance_past_the_check_interval();
        scene.tick().await;
        assert!(scene.daemon.installs().is_empty());
        assert!(!scene.daemon.calls().contains(&Call::Fetch));
        assert_eq!(
            scene.daemon.deferrals().last().unwrap().0,
            update_codes::DEFER_NO_NEWER_RELEASE
        );
    }
}

/// An automatic install requires a clock the device
/// believes, and **checks and fetches are unaffected** — asserted here as
/// well as the refusal, because a test of the refusal alone would pass
/// against a stricter product than the one designed, one where a
/// clockless device stops even discovering updates.
#[tokio::test]
pub(super) async fn an_untrusted_clock_defers_the_install_and_leaves_the_check_and_fetch_alone() {
    let mut scene = Scene::auto(&open_window(), "manual");
    FakeDaemon::set(&scene.daemon.clock, untrusted_clock());
    scene.daemon.will_check(Ok(Settled::Done(the_candidate())));
    scene
        .daemon
        .will_fetch(Ok(Settled::Done(BUNDLE.to_string())));
    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;

    let calls = scene.daemon.calls();
    assert!(calls.contains(&Call::Check), "the check is not time-keyed");
    assert!(calls.contains(&Call::Fetch), "nor is the fetch");
    assert!(
        scene.daemon.installs().is_empty(),
        "the install is: a maintenance window is UTC wall-clock"
    );
    let (reason, detail) = scene.daemon.the_deferral();
    assert_eq!(reason, update_codes::DEFER_CLOCK_UNTRUSTED);
    assert!(
        detail.contains("offline-degraded") && detail.contains("has not advanced since boot"),
        "the deferral names both limbs so an operator knows which to fix: {detail}"
    );

    // The second limb: a saved floor advancing since boot is a clock this
    // device believes even while timesyncd still reports `offline-degraded`.
    FakeDaemon::set(
        &scene.daemon.clock,
        ClockTrust {
            status: Some(SyncStatus::OfflineDegraded),
            floor_advanced: true,
        },
    );
    scene.daemon.will_check(Ok(Settled::Done(the_candidate())));
    scene.tick().await;
    assert_eq!(
        scene.daemon.installs(),
        vec![BUNDLE.to_string()],
        "either limb of the install predicate admits the install"
    );
}

/// A release withdrawn between the fetch and the window is
/// deleted rather than installed, and the automatic path is the only one
/// that is stricter here.
#[tokio::test]
pub(super) async fn a_withdrawn_bundle_is_discarded_rather_than_installed() {
    let mut scene = Scene::auto(&open_window(), "manual");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Ok(Settled::NoneCompatible));

    scene.tick().await;

    assert!(scene.daemon.installs().is_empty());
    assert_eq!(
        scene.daemon.calls().last(),
        Some(&Call::Discard(
            "the current metadata no longer names it".to_string()
        ))
    );
    assert!(
        scene.daemon.deferrals().is_empty(),
        "a withdrawal is not a refusal to record; the state says the descriptor is gone"
    );
}

/// Step 4: `manual` installs and stops at `reboot-required`.
/// "Install automatically" and "reboot automatically" are not the same
/// promise, and `manual` is the default.
#[tokio::test]
pub(super) async fn a_manual_reboot_policy_installs_and_stops() {
    let mut scene = Scene::auto(&open_window(), "manual");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Ok(Settled::Done(the_candidate())));
    scene.tick().await;
    assert_eq!(scene.daemon.installs(), vec![BUNDLE.to_string()]);

    FakeDaemon::set(
        &scene.daemon.facts,
        Some(UpdateFacts {
            reboot_pending: false,
            install_status: Some("done".to_string()),
        }),
    );
    scene.tick().await;
    assert!(
        !scene.daemon.calls().contains(&Call::Reboot),
        "the install happened; the reboot is a human's"
    );
}

/// Step 4, the driver's half of the never-arms-the-override
/// invariant: against a closed gate the automatic path defers, repeats,
/// and reports the escape hatch it cannot itself take.
#[tokio::test]
pub(super) async fn a_closed_reboot_gate_defers_and_the_driver_takes_no_way_around_it() {
    const CLOSED: &str = "reboot refused by the safe-to-reboot gate: mica-vision reports \
                          blocking: recording. An administrator can lift a health block \
                          with SetRebootOverride (POST /api/v1/update/reboot-override).";

    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Ok(Settled::Done(the_candidate())));
    scene.tick().await;
    FakeDaemon::set(
        &scene.daemon.facts,
        Some(UpdateFacts {
            reboot_pending: false,
            install_status: Some("done".to_string()),
        }),
    );
    FakeDaemon::set(&scene.daemon.reboot, Err(CLOSED.to_string()));

    for _ in 0..3 {
        scene.tick().await;
    }

    let deferrals = scene.daemon.deferrals();
    assert_eq!(
        deferrals.len(),
        3,
        "a permanently blocking application permanently defers, visibly: {deferrals:?}"
    );
    for (reason, detail) in &deferrals {
        assert_eq!(reason, update_codes::DEFER_REBOOT_GATE_CLOSED);
        assert_eq!(
            detail, CLOSED,
            "the gate's refusal is recorded verbatim, override sentence and all"
        );
    }

    // The reboot is owed and stays owed: the gate opening is the only
    // thing that discharges it, and the driver did not quietly drop it
    // over three refused attempts.
    FakeDaemon::set(&scene.daemon.reboot, Ok(()));
    scene.tick().await;
    assert_eq!(
        scene
            .daemon
            .calls()
            .iter()
            .filter(|call| **call == Call::Reboot)
            .count(),
        4,
        "the next window re-attempts, and the gate is what decides"
    );
    assert_eq!(
        scene.daemon.calls().last(),
        Some(&Call::Resume(None)),
        "a reboot that went through clears every reason it was refused for"
    );
}

/// `policy = "off"` initiates nothing, and an owed automatic reboot is
/// dropped rather than carried: the operator has just said the device
/// initiates nothing, so a pending slot stays pending and a human reboots
/// into it.
#[tokio::test]
pub(super) async fn switching_to_off_drops_the_reboot_the_driver_owed() {
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Ok(Settled::Done(the_candidate())));
    scene.tick().await;
    FakeDaemon::set(
        &scene.daemon.facts,
        Some(UpdateFacts {
            reboot_pending: false,
            install_status: Some("done".to_string()),
        }),
    );

    scene.write_document(r#"{"policy": "off", "source": {"url": "http://mirror/tuf"}}"#);
    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;
    assert!(
        !scene.daemon.calls().contains(&Call::Reboot),
        "`off` initiates nothing, including the reboot it owed a moment ago"
    );

    // And the owed reboot is gone rather than parked: turning `auto` back
    // on does not reboot on the strength of an install the operator has
    // since disowned.
    FakeDaemon::set(&scene.daemon.staged, None);
    scene.rewrite(&open_window(), "window");
    scene.daemon.will_check(Ok(Settled::NoneCompatible));
    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;
    assert!(!scene.daemon.calls().contains(&Call::Reboot));
}

/// A document that exists and does not parse: the device initiates
/// nothing at all — no check, no fetch, no install — because there is no
/// selection to read a mode out of, and there is no falling back to
/// the baked source here.
#[tokio::test]
pub(super) async fn an_unparseable_document_initiates_nothing() {
    let mut scene = Scene::auto(&open_window(), "window");
    scene.write_document("{not json");
    scene.cadence.advance_past_the_check_interval();

    scene.tick().await;

    assert!(
        scene.daemon.calls().is_empty(),
        "an unreadable document is not a device that goes and checks: {:?}",
        scene.daemon.calls()
    );
}

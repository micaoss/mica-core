//! When a check runs: anchors, intervals and the cadence seam.

use chrono::Duration as Wall;

use super::*;

/// A named time of day is the cadence, not a floor on one: the interval
/// elapses and nothing checks until the clock face comes round.
#[tokio::test]
pub(super) async fn an_anchored_check_waits_for_the_named_time_of_day() {
    let mut scene = Scene::auto(&open_window(), "manual");
    scene.write_document(&anchored_document(
        &clock_face(Wall::hours(2)),
        &open_window(),
    ));
    scene.daemon.will_check(Ok(Settled::NoneCompatible));
    scene.daemon.will_check(Ok(Settled::NoneCompatible));

    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;
    assert!(
        !scene.daemon.calls().contains(&Call::Check),
        "the anchor has not come round: {:?}",
        scene.daemon.calls()
    );

    scene
        .cadence
        .advance_wall(Wall::hours(2) + Wall::minutes(1));
    scene.tick().await;
    assert_eq!(checks(&scene), 1, "the anchor came round and it checked");

    // Once per crossing: an hour later is the same day's anchor, which
    // this driver has answered.
    scene.cadence.advance_wall(Wall::hours(1));
    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;
    assert_eq!(checks(&scene), 1, "the same crossing checked twice");

    // The next day's crossing is a new one.
    scene.cadence.advance_wall(Wall::hours(24));
    scene.tick().await;
    assert_eq!(checks(&scene), 2, "tomorrow's anchor did not check");
}

/// An anchor that passed while the device was down does not fire at boot:
/// a device rebooting hourly under a daily anchor would otherwise check
/// hourly, which is what the interval already guards against.
#[tokio::test]
pub(super) async fn an_anchor_that_passed_before_the_driver_started_does_not_fire() {
    let mut scene = Scene::auto(&open_window(), "manual");
    scene.write_document(&anchored_document(
        &clock_face(-Wall::hours(2)),
        &open_window(),
    ));
    scene.daemon.will_check(Ok(Settled::NoneCompatible));

    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;
    assert_eq!(
        checks(&scene),
        0,
        "an anchor crossed before the start fired anyway"
    );
}

/// The anchor is the one thing here that needs a wall clock, so a device
/// that does not believe its own goes back to the interval rather than
/// stopping: a clockless device must keep discovering updates, which is
/// the rule the install gate is the other half of.
#[tokio::test]
pub(super) async fn an_untrusted_clock_checks_on_the_interval_instead_of_the_anchor() {
    let mut scene = Scene::auto(&open_window(), "manual");
    scene.write_document(&anchored_document(
        &clock_face(Wall::hours(2)),
        &open_window(),
    ));
    FakeDaemon::set(&scene.daemon.clock, untrusted_clock());
    scene.daemon.will_check(Ok(Settled::NoneCompatible));

    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;
    assert_eq!(
        checks(&scene),
        1,
        "an untrusted clock stopped the checks instead of falling back"
    );
}

/// How many checks the driver has asked for.
pub(super) fn checks(scene: &Scene) -> usize {
    scene
        .daemon
        .calls()
        .iter()
        .filter(|call| **call == Call::Check)
        .count()
}

/// The seam itself, and the property every test below rests on: a driver
/// reaches its next pass because a test moved its clock, not because a
/// test waited.
#[tokio::test]
pub(super) async fn the_cadence_seam_advances_the_driver_without_sleeping() {
    let mut scene = Scene::auto(&open_window(), "manual");
    scene.daemon.will_check(Ok(Settled::NoneCompatible));

    // The first check falls one interval after start, not on the first
    // tick: a device that reboots hourly must not check hourly.
    scene.tick().await;
    scene.cadence.advance(Duration::from_secs(59 * 60));
    scene.tick().await;
    assert!(
        !scene.daemon.calls().contains(&Call::Check),
        "the interval has not elapsed: {:?}",
        scene.daemon.calls()
    );

    scene.cadence.advance(Duration::from_secs(2 * 60));
    scene.tick().await;
    assert_eq!(
        scene
            .daemon
            .calls()
            .iter()
            .filter(|call| **call == Call::Check)
            .count(),
        1,
        "the interval elapsed and the driver checked once"
    );

    // Attempt-based, not success-based: the check above found nothing and
    // the next one is still a whole interval away.
    scene.tick().await;
    assert_eq!(
        scene
            .daemon
            .calls()
            .iter()
            .filter(|call| **call == Call::Check)
            .count(),
        1,
        "a checked cadence must not retry on the next tick"
    );
}

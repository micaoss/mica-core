//! Every deferral the driver can mint.

use crate::update_lifecycle::Unready;
use std::collections::BTreeSet;

use super::*;

/// Each deferral reason reachable in a test.
#[tokio::test]
pub(super) async fn every_deferral_reason_the_driver_can_mint_is_reachable() {
    let mut observed: BTreeSet<String> = BTreeSet::new();
    let mut record = |scene: &Scene, expected: &str, detail_contains: &str| {
        let (reason, detail) = scene.daemon.the_deferral();
        assert_eq!(reason, expected, "detail was: {detail}");
        assert!(
            detail.contains(detail_contains),
            "`{expected}` must carry the refusing rule; got: {detail}"
        );
        observed.insert(reason);
    };

    // 1. The source publishes nothing newer. Recorded rather than passed
    //    over: `idle` is also what a device that never checked renders as.
    let mut scene = Scene::auto(&open_window(), "window");
    scene.daemon.will_check(Ok(Settled::NoneCompatible));
    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_NO_NEWER_RELEASE,
        "nothing newer for this product",
    );

    // 2. The cadence check, refused by the policy an operator would meet.
    let mut scene = Scene::auto(&open_window(), "window");
    scene.daemon.will_check(Err(Refusal::Policy(
        "network mode is offline: updates arrive by import only".to_string(),
    )));
    scene.cadence.advance_past_the_check_interval();
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_CHECK_REFUSED,
        "network mode is offline",
    );

    // 5. The fetch, refused by the policy an operator would meet.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.available, Some(the_candidate()));
    scene.daemon.will_fetch(Err(Refusal::Policy(
        "network mode is metered: descriptor downloads are refused".to_string(),
    )));
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_FETCH_REFUSED,
        "network mode is metered",
    );

    // 6. the predicate, first in the list on purpose.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    FakeDaemon::set(&scene.daemon.clock, untrusted_clock());
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_CLOCK_UNTRUSTED,
        "offline-degraded",
    );

    // 7. The maintenance window, which `auto` requires the document to
    //    name and which gates the install and only the install.
    let mut scene = Scene::auto(&shut_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_OUTSIDE_WINDOW,
        "outside every configured maintenance window",
    );

    // 8. An unanswered slot query is not "nothing is pending".
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    FakeDaemon::set(&scene.daemon.facts, None);
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_DEPLOYMENT_STATUS_UNKNOWN,
        "the native backend did not answer",
    );

    // 9. A slot already installed and waiting for its first boot.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    FakeDaemon::set(
        &scene.daemon.facts,
        Some(UpdateFacts {
            reboot_pending: true,
            install_status: None,
        }),
    );
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_REBOOT_PENDING,
        "waiting for its first boot",
    );

    // 10. The re-check, refused by the workspace probe.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Ok(Settled::Unready(Unready {
        status: "degraded".to_string(),
        kind: update_codes::WORKSPACE_PROBE_FAILED,
        detail: "/mica is mounted read-only".to_string(),
    })));
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_WORKSPACE_UNREADY,
        "/mica is mounted read-only",
    );

    // 11. The re-check ran and failed.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene
        .daemon
        .will_check(Ok(Settled::Failed(update_codes::CodedReason::new(
            update_codes::CLIENT_EXIT_FAILURE,
            "state file corrupt",
        ))));
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_RECHECK_FAILED,
        "state file corrupt",
    );

    // 12. The re-check was not admitted at all.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Err(Refusal::Unavailable(
        "/usr/bin/mica-deploy is not present on this image".to_string(),
    )));
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_RECHECK_REFUSED,
        "mica-deploy is not present",
    );

    // 13. The metadata names something else. The staged descriptor is
    //     superseded rather than provably withdrawn, so it is not deleted.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Ok(Settled::Done(candidate(
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb.json",
        "1.6.0",
    ))));
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_SUPERSEDED,
        "the check now names bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
    );
    assert_eq!(
        scene.daemon.staged.lock().expect("staged").as_deref(),
        Some(BUNDLE),
        "a superseded descriptor is left where a human can still install it"
    );

    // 16. The install route's own refusal, verbatim.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Ok(Settled::Done(the_candidate())));
    FakeDaemon::set(
        &scene.daemon.install,
        Err("an update install is already running; query GetUpdateState and retry".to_string()),
    );
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_INSTALL_REFUSED,
        "already running",
    );

    // 17. The safe-to-reboot gate, reached the only way the driver can
    //     reach it: an install of its own that finished.
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
    FakeDaemon::set(
        &scene.daemon.reboot,
        Err("reboot refused by the safe-to-reboot gate: mica-vision reports blocking".to_string()),
    );
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_REBOOT_GATE_CLOSED,
        "safe-to-reboot gate",
    );

    // 18. The window again, on the reboot rather than on the install: an
    //     operator who shut it between the two is obeyed by both.
    let mut scene = Scene::auto(&open_window(), "window");
    FakeDaemon::set(&scene.daemon.staged, Some(BUNDLE.to_string()));
    scene.daemon.will_check(Ok(Settled::Done(the_candidate())));
    scene.tick().await;
    scene.rewrite(&shut_window(), "window");
    FakeDaemon::set(
        &scene.daemon.facts,
        Some(UpdateFacts {
            reboot_pending: false,
            install_status: Some("done".to_string()),
        }),
    );
    scene.tick().await;
    record(
        &scene,
        update_codes::DEFER_OUTSIDE_WINDOW,
        "outside every configured maintenance window",
    );
    assert!(
        !scene.daemon.calls().contains(&Call::Reboot),
        "a shut window is asked before the gate is"
    );

    let declared: BTreeSet<String> = update_codes::DEFERRALS
        .into_iter()
        .map(str::to_string)
        .collect();
    assert_eq!(
        observed, declared,
        "every word in the deferral vocabulary must be one a pass produces"
    );
}

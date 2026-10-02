use super::*;

// GuardStore: the "a power cycle must not reset the clock"
// The LoginGuard tests above are about the CURVE. These are about the
// curve SURVIVING, which is the property that actually matters and the one
// an in-RAM counter satisfies vacuously.

/// A store rebuilt from the same path is still throttled. This is the
/// classic embedded bypass — pull the power, come back to a clean slate —
/// and it is the whole reason this type exists.
#[test]
fn a_restart_does_not_reset_the_armed_window() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login_guard.json");

    // Seeded with a run, so the armed window is 16 seconds rather than
    // BACKOFF_BASE's one. The property under test is "an armed window
    // survives a restart" and does not depend on which step of the curve is
    // armed, but arming the first step races the format's own resolution:
    // the write and the read below are two filesystem round-trips, and
    // `PersistedGuard` carries the deadline as whole UNIX seconds —
    // `to_persisted` writes `now_unix + remaining.as_secs.max(1)` and
    // `from_persisted` subtracts a freshly-read `now_unix`, so the two
    // truncations do not cancel and a write and read landing on opposite
    // sides of a second boundary reduce a one-second window to zero.
    // Sixteen seconds absorbs both the truncation and any load this suite
    // can generate; the one-second step is covered without a clock by
    // `backoff_doubles_from_the_base_and_stops_at_the_cap`. The truncation
    // is a real if minor property of the on-disk format — a restart inside
    // the first second can drop that step's window while keeping the
    // failure count — recorded here rather than fixed, this test's job
    // being the round trip.
    std::fs::write(
        &path,
        serde_json::to_string(&PersistedGuard {
            failures: 5,
            locked_until_unix: 0,
        })
        .unwrap(),
    )
    .unwrap();

    let store = GuardStore::load(path.clone());
    assert!(store.begin_attempt(), "the first attempt is admitted");
    store.confirm_failure();
    assert!(
        !store.begin_attempt(),
        "the failure armed a window, so the next attempt is refused"
    );
    drop(store);

    let restarted = GuardStore::load(path);
    assert!(
        !restarted.begin_attempt(),
        "a restart admitted an attempt the armed window had refused: the counter did not \
         survive, which is exactly the bypass the backoff must prevent"
    );
}

/// ...and the RUN survives too, not merely the window. A restart that
/// kept the lockout but forgot the failure count would let an attacker
/// hold the curve at its base step forever by power-cycling.
#[test]
fn a_restart_carries_the_failure_run_so_the_curve_keeps_climbing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login_guard.json");

    let store = GuardStore::load(path.clone());
    for _ in 0..4 {
        store.with(|guard| guard.record_failure());
    }
    let before = store.with(|guard| guard.failures);
    drop(store);

    let restarted = GuardStore::load(path);
    assert_eq!(
        restarted.with(|guard| guard.failures),
        before,
        "the consecutive-failure run reset across a restart, so the backoff would start \
         again from BACKOFF_BASE however many failures preceded it"
    );
    assert!(before >= 4);
}

/// A window that had already elapsed when the file was written must not
/// come back as a lockout. The persisted form is an absolute deadline, so
/// getting this backwards locks an operator out of a device that was
/// never throttled.
#[test]
fn an_elapsed_window_does_not_come_back_as_a_lockout() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login_guard.json");
    std::fs::write(
        &path,
        serde_json::to_string(&PersistedGuard {
            failures: 3,
            locked_until_unix: crate::persist::now_unix().saturating_sub(60),
        })
        .unwrap(),
    )
    .unwrap();

    let store = GuardStore::load(path);
    // The failure run first: admitting the attempt is ALSO what a store
    // that never read the file at all would do, so this assertion is what
    // makes the next one evidence about loading rather than about
    // defaults. (Measured: without it, this case stays green under a
    // mutation that makes `load` ignore the file entirely.)
    assert_eq!(
        store.with(|guard| guard.failures),
        3,
        "the file was not read, so nothing below is a claim about reloading"
    );
    assert!(
        store.begin_attempt(),
        "a deadline 60s in the past was reloaded as an armed window"
    );
}

/// A far-future deadline is capped at BACKOFF_MAX on load. "Never
/// permanent" has to survive bad data, not just good arithmetic.
#[test]
fn a_far_future_deadline_is_capped_rather_than_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login_guard.json");
    std::fs::write(
        &path,
        serde_json::to_string(&PersistedGuard {
            failures: 9,
            locked_until_unix: crate::persist::now_unix() + 10 * 365 * 24 * 3600,
        })
        .unwrap(),
    )
    .unwrap();

    let store = GuardStore::load(path);
    let remaining = store
        .with(|guard| guard.locked_until)
        .expect("a future deadline arms a window")
        .saturating_duration_since(Instant::now());
    assert!(
        remaining <= BACKOFF_MAX,
        "a ten-year deadline survived load as {remaining:?}; the cap is the only thing \
         standing between a bit-flip and a permanently bricked management interface"
    );
}

/// Corruption degrades to the in-RAM guard, never to a lockout and never
/// to a daemon that will not start.
#[test]
fn a_corrupt_state_file_starts_clean_rather_than_locking_out() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login_guard.json");
    std::fs::write(&path, b"{ this is not json").unwrap();

    let store = GuardStore::load(path);
    assert!(
        store.begin_attempt(),
        "a corrupt rate-limiter file refused a login; the failure direction has to be \
         open here, because the alternative is an appliance no operator can reach"
    );
}

/// The file is written 0600, and it is written at all. Both halves: a
/// test that only checked the mode would pass on a file that was never
/// created.
#[test]
fn the_persisted_file_is_written_and_is_not_world_readable() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login_guard.json");
    let store = GuardStore::load(path.clone());
    store.begin_attempt();
    store.confirm_failure();

    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "login-guard state is mode {mode:o}");
}

/// A refused attempt mutates nothing and must therefore write nothing.
/// `GuardStore::with`'s changed-check is what makes this true, and
/// without it an attacker hammering a throttled endpoint converts every
/// refusal into an fsync — flash wear bought with an HTTP request.
///
/// Nothing here is timed: asserting that N consecutive calls are all
/// refused would make the armed window a deadline the loop must fit
/// inside, and a loaded machine would fail it while the product behaves
/// correctly. The loop stops at the first ADMITTED attempt — an admission
/// is the window expiring, which is allowed and legitimately writes — and
/// asserts the property once per refusal, so the count is evidence rather
/// than a budget.
#[test]
fn a_refused_attempt_does_not_rewrite_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("login_guard.json");
    let store = GuardStore::load(path.clone());
    assert!(store.begin_attempt(), "the first attempt is admitted");
    // The run is seeded so the window this failure arms is the curve's
    // cap. That is the PRECONDITION — "the store is refusing" — and not
    // the property: which step of the curve is armed changes nothing
    // below, and arming the base step made even entering the loop a bet
    // on one second. The loop is what removes the race; this only stops
    // the first iteration from being the same bet in miniature.
    store.with(|guard| guard.failures = 10);
    store.confirm_failure();

    let before = std::fs::metadata(&path).unwrap().modified().unwrap();
    let armed = std::fs::read(&path).unwrap();
    let mut refused = 0u32;
    for _ in 0..50 {
        if store.begin_attempt() {
            break;
        }
        refused += 1;
        assert_eq!(
            std::fs::read(&path).unwrap(),
            armed,
            "a refused attempt changed the persisted state"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before,
            "a refused attempt rewrote the file; each one is an fsync an unauthenticated \
             caller can trigger"
        );
    }
    assert!(
        refused > 0,
        "not one attempt was refused, so nothing above was asserted"
    );
}

/// The ephemeral form touches no filesystem at all — the negative control
/// for every test above, and what `AppState::new` gives tests.
#[test]
fn an_ephemeral_store_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let store = GuardStore::ephemeral();
    store.begin_attempt();
    store.confirm_failure();
    store.record_success();
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        0,
        "the ephemeral store created a file"
    );
}

#[test]
fn hash_roundtrip() {
    let hash = hash_password("correct horse").unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert!(verify_password(&hash, "correct horse"));
    assert!(!verify_password(&hash, "wrong"));
    assert!(!verify_password("not a phc string", "wrong"));
}

#[test]
fn backoff_doubles_from_the_base_and_stops_at_the_cap() {
    assert_eq!(backoff_for(0), Duration::ZERO);
    assert_eq!(backoff_for(1), BACKOFF_BASE);
    assert_eq!(backoff_for(2), Duration::from_secs(2));
    assert_eq!(backoff_for(3), Duration::from_secs(4));
    assert_eq!(backoff_for(9), Duration::from_secs(256));
    // The cap bites here and holds for every count past it, including the
    // ones where `2^(n-1)` no longer fits in a u64.
    assert_eq!(backoff_for(10), BACKOFF_MAX);
    assert_eq!(backoff_for(64), BACKOFF_MAX);
    assert_eq!(backoff_for(u32::MAX), BACKOFF_MAX);
}

#[test]
fn one_failure_already_arms_a_window() {
    let mut guard = LoginGuard::default();
    assert!(guard.check());
    guard.record_failure();
    // One failure, and the next attempt is already refused.
    assert!(!guard.check());
}

#[test]
fn an_elapsed_window_does_not_reset_the_run() {
    let mut guard = LoginGuard::default();
    for _ in 0..3 {
        guard.record_failure();
    }
    // Expire the window the way the clock would, without waiting on it.
    guard.locked_until = Some(Instant::now() - Duration::from_secs(1));
    assert!(guard.check());
    assert_eq!(guard.failures, 3, "riding out a window must not be free");

    // So the next failure escalates rather than restarting the curve.
    guard.record_failure();
    assert_eq!(guard.failures, 4);
}

#[test]
fn success_ends_the_run_and_clears_the_window() {
    let mut guard = LoginGuard::default();
    for _ in 0..5 {
        guard.record_failure();
    }
    assert!(!guard.check());
    guard.record_success();
    assert!(guard.check());
    assert_eq!(guard.failures, 0);

    // A fresh run starts back at the base, not where the last one stopped.
    guard.record_failure();
    assert_eq!(guard.failures, 1);
}

#[test]
fn begin_attempt_charges_at_admission_not_at_outcome() {
    let mut guard = LoginGuard::default();
    assert!(guard.begin_attempt());
    // The window armed when the first attempt was admitted, so a second
    // attempt racing it is refused before the first reports any outcome —
    // the property a separate check-then-record pair did not have.
    assert!(!guard.begin_attempt());

    // Success repays the admission charge entirely.
    guard.record_success();
    assert_eq!(guard.failures, 0);
    assert!(guard.begin_attempt());
    assert_eq!(
        guard.failures, 1,
        "an admitted attempt is a charged attempt"
    );
}

#[test]
fn the_lockout_is_never_permanent() {
    let mut guard = LoginGuard::default();
    for _ in 0..1000 {
        guard.record_failure();
    }
    let until = guard.locked_until.expect("a window is armed");
    // An administrator who knows the password waits at most BACKOFF_MAX --
    // there is no threshold past which the daemon stops answering, because
    // apid has no physical-presence release to clear one with.
    assert!(until <= Instant::now() + BACKOFF_MAX);
}

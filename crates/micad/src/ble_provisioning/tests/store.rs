//! The verifier, the lockout and the history on disk.

use super::super::spake2plus::{Transcript, answer, prover, scalar};
use super::super::store::*;
use super::super::*;

#[test]
fn a_made_verifier_round_trips_and_answers_its_password() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    assert!(store.verifier().unwrap().is_none());
    let record = make_record(b"harbour-pw-1", 1000).unwrap();
    assert_eq!(record.salt.len(), SALT_LEN);
    store.set_verifier(&record).unwrap();
    let read = store.verifier().unwrap().unwrap();
    assert_eq!(read, record);
    let mode = std::fs::metadata(dir.path().join("secrets/ble-verifier.json"))
        .unwrap()
        .permissions();
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
        0o600
    );

    let (w0, w1) = spake2plus::derive(
        b"harbour-pw-1",
        &read.salt,
        std::num::NonZeroU32::new(1000).unwrap(),
        ID_PROVER,
        ID_VERIFIER,
    );
    let t = Transcript {
        context: CONTEXT_PASSWORD,
        id_prover: ID_PROVER,
        id_verifier: ID_VERIFIER,
    };
    let x = scalar(&[7; 32]);
    let device = answer(&read, &t, &prover::share(&w0, &x)).unwrap();
    let phone = prover::finish(&t, &w0, &w1, &x, &device.share_v).unwrap();
    assert_eq!(phone, device.keys);
}

#[test]
fn five_failures_lock_for_ten_minutes_then_an_hour_then_a_day() {
    let mut lockout = Lockout::default();
    for left in (1..=5).rev() {
        assert_eq!(lockout.attempts_left(), left);
        assert_eq!(lockout.remaining(100), None);
        lockout.charge(100);
    }
    assert_eq!(lockout.remaining(100), Some(600));
    assert_eq!(lockout.attempts_left(), 5);
    for _ in 0..5 {
        lockout.charge(1000);
    }
    assert_eq!(lockout.remaining(1000), Some(3600));
    for _ in 0..5 {
        lockout.charge(10_000);
    }
    assert_eq!(lockout.remaining(10_000), Some(86_400));
    for _ in 0..5 {
        lockout.charge(200_000);
    }
    assert_eq!(lockout.remaining(200_000), Some(86_400));
}

#[test]
fn a_success_takes_its_charge_back_and_resets_the_count() {
    let mut lockout = Lockout::default();
    for _ in 0..4 {
        lockout.charge(100);
    }
    let before = lockout.clone();
    lockout.charge(100);
    assert!(lockout.remaining(100).is_some());
    lockout.succeed(&before);
    assert_eq!(lockout, Lockout::default());
}

#[test]
fn the_lockout_survives_a_restart_and_the_history_keeps_ten() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::new(dir.path());
    assert_eq!(store.lockout(), Lockout::default());
    let mut lockout = Lockout::default();
    lockout.charge(5);
    store.set_lockout(&lockout).unwrap();
    assert_eq!(Store::new(dir.path()).lockout(), lockout);

    for at in 0..12 {
        store
            .push_history(Entry {
                at,
                trigger: "boot".into(),
                outcome: "finished".into(),
            })
            .unwrap();
    }
    let history = store.history();
    assert_eq!(history.len(), HISTORY_LEN);
    assert_eq!(history[0].at, 2);
    assert_eq!(history[9].at, 11);
}

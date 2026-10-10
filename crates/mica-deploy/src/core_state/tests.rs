// Tests answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let system = dir.path().join("system");
    let meta = dir.path().join("meta");
    std::fs::create_dir_all(system.join(SETS_DIR)).unwrap();
    std::fs::create_dir_all(&meta).unwrap();
    (dir, system, meta)
}

#[test]
fn a_system_that_never_held_a_set_has_an_empty_state() {
    let (_dir, system, _meta) = dirs();
    assert_eq!(read_state(&system).unwrap(), CoreState::default());
    assert!(
        read_state(&system.join("absent"))
            .unwrap()
            .current
            .is_none()
    );
}

#[test]
fn the_state_round_trips_and_refuses_what_it_cannot_mean() {
    let (_dir, system, _meta) = dirs();
    let state = CoreState {
        current: Some(ID.into()),
        pending: Some(OTHER.into()),
    };
    write_state(&system, &state).unwrap();
    assert_eq!(read_state(&system).unwrap(), state);
    for text in [
        format!("{{\"current\":\"{ID}\",\"pending\":\"{ID}\"}}"),
        "{\"current\":\"../../etc\"}".to_string(),
        format!("{{\"current\":\"{ID}\",\"previous\":\"{OTHER}\"}}"),
        "not json".to_string(),
    ] {
        std::fs::write(system.join("core-sets/state.json"), &text).unwrap();
        assert!(read_state(&system).is_err(), "{text}");
    }
}

/// A trial is three boots, each spent before it is used, and nothing is tried
/// once they are gone or when the trial is another set's.
#[test]
fn a_trial_spends_one_boot_at_a_time_and_then_tries_nothing() {
    let (_dir, _system, meta) = dirs();
    assert!(!spend_attempt(&meta, ID), "no trial was started");
    start_trial(&meta, ID).unwrap();
    assert!(!trial_is_over(&meta, ID));
    assert!(!spend_attempt(&meta, OTHER), "another set's trial");
    for left in (0..TRIAL_ATTEMPTS).rev() {
        assert!(spend_attempt(&meta, ID));
        assert_eq!(read_trial(&meta).unwrap().attempts_left, left);
    }
    assert!(!spend_attempt(&meta, ID));
    assert!(trial_is_over(&meta, ID));
    end_trial(&meta).unwrap();
    assert!(read_trial(&meta).is_none());
    end_trial(&meta).unwrap();
}

/// A trial file that cannot be read is no trial: the answer that tries
/// nothing new, whatever was written into it.
#[test]
fn a_damaged_trial_tries_nothing() {
    let (_dir, _system, meta) = dirs();
    for text in ["", "{", "{\"id\":\"x\",\"attemptsLeft\":3}", "{\"id\":3}"] {
        std::fs::write(meta.join("core-trial.json"), text).unwrap();
        assert!(read_trial(&meta).is_none(), "{text}");
        assert!(!spend_attempt(&meta, ID));
        assert!(trial_is_over(&meta, ID));
    }
}

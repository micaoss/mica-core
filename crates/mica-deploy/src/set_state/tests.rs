// Tests answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CORE: SetKind = SetKind::Core;
const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let system = dir.path().join("system");
    let meta = dir.path().join("meta");
    std::fs::create_dir_all(kind_dir(&system, CORE)).unwrap();
    std::fs::create_dir_all(&meta).unwrap();
    (dir, system, meta)
}

#[test]
fn a_system_that_never_held_a_set_has_an_empty_state() {
    let (_dir, system, _meta) = dirs();
    assert_eq!(read_state(&system, CORE).unwrap(), SetState::default());
    assert!(
        read_state(&system.join("absent"), CORE)
            .unwrap()
            .current
            .is_none()
    );
}

#[test]
fn the_state_round_trips_and_refuses_what_it_cannot_mean() {
    let (_dir, system, _meta) = dirs();
    let state = SetState {
        current: Some(ID.into()),
        pending: Some(OTHER.into()),
        pair: Some("c".repeat(64)),
        bound: [("d".repeat(64), OTHER.to_string())].into(),
    };
    write_state(&system, CORE, &state).unwrap();
    assert_eq!(read_state(&system, CORE).unwrap(), state);
    assert_eq!(state.paired(), Some("c".repeat(64).as_str()));
    let unpaired = SetState {
        pending: None,
        ..state.clone()
    };
    assert_eq!(unpaired.paired(), None, "a pair without a pending set");
    let crowded = SetState {
        bound: (0..3)
            .map(|n| (format!("{n:064}"), ID.to_string()))
            .collect(),
        ..state
    };
    assert!(write_state(&system, CORE, &crowded).is_err());
    for text in [
        format!("{{\"current\":\"{ID}\",\"pending\":\"{ID}\"}}"),
        "{\"current\":\"../../etc\"}".to_string(),
        "{\"pair\":\"../../etc\"}".to_string(),
        format!("{{\"bound\":{{\"../x\":\"{ID}\"}}}}"),
        format!("{{\"bound\":{{\"{OTHER}\":\"../x\"}}}}"),
        serde_json::to_string(&crowded).unwrap(),
        format!("{{\"current\":\"{ID}\",\"previous\":\"{OTHER}\"}}"),
        "not json".to_string(),
    ] {
        std::fs::write(system.join("sets/core/state.json"), &text).unwrap();
        assert!(read_state(&system, CORE).is_err(), "{text}");
    }
}

/// A trial is three boots, each spent before it is used, and nothing is tried
/// once they are gone or when the trial is another set's.
#[test]
fn a_trial_spends_one_boot_at_a_time_and_then_tries_nothing() {
    let (_dir, _system, meta) = dirs();
    assert!(!spend_attempt(&meta, CORE, ID), "no trial was started");
    start_trial(&meta, CORE, ID).unwrap();
    assert!(!trial_is_over(&meta, CORE, ID));
    assert!(!spend_attempt(&meta, CORE, OTHER), "another set's trial");
    for left in (0..TRIAL_ATTEMPTS).rev() {
        assert!(spend_attempt(&meta, CORE, ID));
        assert_eq!(read_trial(&meta).unwrap().attempts_left, left);
    }
    assert!(!spend_attempt(&meta, CORE, ID));
    assert!(trial_is_over(&meta, CORE, ID));
    end_trial(&meta).unwrap();
    assert!(read_trial(&meta).is_none());
    end_trial(&meta).unwrap();
}

/// A trial file that cannot be read is no trial: the answer that tries
/// nothing new, whatever was written into it.
#[test]
fn a_damaged_trial_tries_nothing() {
    let (_dir, _system, meta) = dirs();
    let other_kind = format!("{{\"kind\":\"charts\",\"id\":\"{ID}\",\"attemptsLeft\":3}}");
    let no_kind = format!("{{\"id\":\"{ID}\",\"attemptsLeft\":3}}");
    for text in [
        "",
        "{",
        "{\"kind\":\"core\",\"id\":\"x\",\"attemptsLeft\":3}",
        "{\"id\":3}",
        // A kind this reader does not implement, and a trial that names none.
        other_kind.as_str(),
        no_kind.as_str(),
    ] {
        std::fs::write(meta.join("set-trial.json"), text).unwrap();
        assert!(read_trial(&meta).is_none(), "{text}");
        assert!(!spend_attempt(&meta, CORE, ID));
        assert!(trial_is_over(&meta, CORE, ID));
    }
}

use serde_json::json;

use super::*;

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

#[test]
fn nothing_installs_without_the_install_word() {
    assert_eq!(parse(&args(&[])), Ok(Command::Status));
    assert_eq!(parse(&args(&["status"])), Ok(Command::Status));
    assert_eq!(parse(&args(&["check"])), Ok(Command::Check));
    assert_eq!(
        parse(&args(&["install"])),
        Ok(Command::Install { reboot: false })
    );
    assert_eq!(
        parse(&args(&["install", "--reboot"])),
        Ok(Command::Install { reboot: true })
    );
    assert_eq!(
        parse(&args(&["rollback", "--reboot"])),
        Ok(Command::Rollback { reboot: true })
    );
    assert_eq!(parse(&args(&["--help"])), Ok(Command::Help));
    for misuse in [
        &["--reboot"][..],
        &["check", "--reboot"],
        &["install", "now"],
        &["upgrade"],
    ] {
        assert!(parse(&args(misuse)).is_err(), "{misuse:?}");
    }
}

#[test]
fn an_install_is_followed_through_its_steps_to_its_outcome() {
    let me = ":1.42";
    let step = |doc: Value| progress(&doc, me, None, Watch::Install);
    let offered = json!({"version": "0.0.6", "kind": "set", "setKind": "core"});

    assert_eq!(
        step(json!({"lifecycle": {"state": "checking"}})),
        Progress::Working("checking".to_string())
    );
    // The check found a release and the fetch has not started yet.
    assert_eq!(
        step(json!({"lifecycle": {"state": "idle", "available": offered}})),
        Progress::Between
    );
    assert_eq!(
        step(json!({"lifecycle": {"state": "downloading", "available": offered}})),
        Progress::Working("downloading".to_string())
    );
    assert_eq!(
        step(json!({"lifecycle": {"state": "ready", "available": offered}})),
        Progress::Between
    );
    // Somebody else's install is not this one.
    assert_eq!(
        step(
            json!({"lifecycle": {"state": "ready", "available": offered},
            "install": {"status": "done", "requested_by": ":1.7"}})
        ),
        Progress::Between
    );
    assert_eq!(
        step(json!({"lifecycle": {"state": "installing"},
            "install": {"status": "running", "requested_by": me}})),
        Progress::Working("installing".to_string())
    );
    assert_eq!(
        step(json!({"lifecycle": {"state": "reboot-required"},
            "install": {"status": "done", "requested_by": me}})),
        Progress::Finished(Ok(Done::Installed {
            reboot_pending: true
        }))
    );
    assert_eq!(
        step(json!({"lifecycle": {"state": "idle"},
            "install": {"status": "failed", "requested_by": me, "error": "signature"}})),
        Progress::Finished(Err("signature".to_string()))
    );
}

#[test]
fn an_install_ends_up_to_date_failed_or_refused() {
    let step = |doc: Value, before: Option<&str>| progress(&doc, ":1.42", before, Watch::Install);
    assert_eq!(
        step(json!({"lifecycle": {"state": "idle"}}), None),
        Progress::Finished(Ok(Done::UpToDate))
    );
    // A descriptor staged before still reads `ready`; nothing newer is offered.
    assert_eq!(
        step(json!({"lifecycle": {"state": "ready"}}), None),
        Progress::Finished(Ok(Done::UpToDate))
    );
    assert_eq!(
        step(
            json!({"lifecycle": {"state": "failed",
                "reason": "check was killed by signal 6 (SIGABRT)"}}),
            None
        ),
        Progress::Finished(Err("check was killed by signal 6 (SIGABRT)".to_string()))
    );
    let refused = json!({"lifecycle": {"state": "ready", "available": {"version": "2"},
        "last_refusal": "install refused: outside every configured maintenance window"}});
    assert_eq!(
        step(refused.clone(), None),
        Progress::Finished(Err(
            "install refused: outside every configured maintenance window".to_string()
        ))
    );
    // The same refusal recorded before the request is not this request's.
    assert_eq!(
        step(
            refused,
            Some("install refused: outside every configured maintenance window")
        ),
        Progress::Between
    );
}

#[test]
fn a_check_names_what_it_found() {
    let step = |doc: Value| progress(&doc, ":1.42", None, Watch::Check);
    assert_eq!(
        step(json!({"lifecycle": {"state": "idle",
            "available": {"version": "0.0.6", "kind": "set", "setKind": "core"}}})),
        Progress::Finished(Ok(Done::Available("core 0.0.6".to_string())))
    );
    assert_eq!(
        step(json!({"lifecycle": {"state": "idle",
            "available": {"version": "1.4.0", "kind": "deployment"}}})),
        Progress::Finished(Ok(Done::Available("system 1.4.0".to_string())))
    );
    assert_eq!(
        step(json!({"lifecycle": {"state": "idle"}})),
        Progress::Finished(Ok(Done::UpToDate))
    );
}

#[test]
fn a_rollback_takes_only_a_permitted_target() {
    let running = "a".repeat(64);
    let target = "b".repeat(64);
    assert_eq!(
        rollback_target(&json!({"boot": {"deploymentId": running},
            "rollback": {"permitted": true, "target": target}}))
        .unwrap(),
        (running.clone(), target)
    );
    let refused = rollback_target(&json!({"boot": {"deploymentId": running},
        "rollback": {"permitted": false, "reason": "no_usable_fallback"}}))
    .unwrap_err();
    assert!(
        refused.to_string().contains("no_usable_fallback"),
        "{refused}"
    );
}

#[test]
fn status_reads_as_a_few_lines() {
    let running = "a".repeat(64);
    let doc = json!({
        "boot": {"deploymentId": running},
        "deployments": [{"id": running, "version": "20261015-1321"}],
        "sets": {"core": {"current": {"version": "0.0.5", "channel": "general"}}},
        "lifecycle": {"state": "idle", "last_check": "2026-10-15T20:00:00Z",
            "available": {"version": "0.0.6", "kind": "set", "setKind": "core"}},
    });
    assert_eq!(
        render_status(&doc),
        "system     20261015-1321 (aaaaaaaaaaaa)\n\
         core       0.0.5 (channel general)\n\
         state      idle\n\
         available  core 0.0.6\n\
         checked    2026-10-15T20:00:00Z\n"
    );
}

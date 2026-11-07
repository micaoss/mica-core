use super::*;
use std::fs;

fn device() -> (tempfile::TempDir, FactoryReset) {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("data/meta")).unwrap();
    fs::create_dir_all(dir.path().join("data/state")).unwrap();
    fs::create_dir_all(dir.path().join("run")).unwrap();
    let reset = at(dir.path());
    (dir, reset)
}

#[test]
fn only_a_runkit_that_says_so_supports_the_reset() {
    let (dir, reset) = device();
    assert!(!reset.supported(), "no file");
    for (text, want) in [
        (r#"{"factoryReset":true}"#, true),
        (r#"{"factoryReset":false}"#, false),
        (r#"{"factoryReset":"true"}"#, false),
        ("{", false),
    ] {
        fs::write(dir.path().join("run/runkit.json"), text).unwrap();
        assert_eq!(reset.supported(), want, "{text}");
    }
}

#[test]
fn wrong_passwords_back_off_and_a_right_one_clears_it() {
    let (_dir, reset) = device();
    for _ in 0..FREE_FAILURES {
        reset.failed();
        assert_eq!(reset.throttled(), None);
    }
    reset.failed();
    let refusal = reset.throttled().expect("backed off");
    assert_eq!(refusal.code, "login_throttled");
    reset.succeeded();
    assert_eq!(reset.throttled(), None);
}

#[test]
fn a_staged_reset_is_the_runkit_s_request() {
    let (dir, reset) = device();
    reset.stage(Requester::Cli, "uid 0", 1_700_000_000).unwrap();
    let request: Request = serde_json::from_slice(
        &fs::read(dir.path().join("data/meta").join(erase::REQUEST)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        request,
        Request {
            v: 1,
            requested: 1_700_000_000,
            by: Requester::Cli,
            source: "uid 0".into(),
            deployment: None,
        }
    );
}

#[test]
fn the_first_boot_after_a_reset_records_it_once() {
    let (dir, reset) = device();
    assert_eq!(reset.first_boot().unwrap(), None, "no reset yet");

    // The runkit's erase, as it leaves DATA.
    reset.stage(Requester::Console, "192.0.2.7", 10).unwrap();
    let done = mica_deploy::factory_reset::apply(
        &dir.path().join("data"),
        20,
        &mut || Ok(()),
        &mut |_| {},
    )
    .unwrap()
    .unwrap();

    assert_eq!(reset.first_boot().unwrap(), Some(done.clone()));
    assert_eq!(reset.first_boot().unwrap(), None, "recorded already");
    assert_eq!(reset.last(), Some(done.clone()));
    assert_eq!(status(&reset), json!({ "supported": false, "last": done }));
    // The discard is best effort: a directory on a filesystem that does not
    // discard, or a caller without the privilege, is skipped quietly.
    reset.discard_free_space();
}

#[test]
fn a_requester_is_one_of_three() {
    assert_eq!(requester("console"), Some(Requester::Console));
    assert_eq!(requester("api"), Some(Requester::Api));
    assert_eq!(requester("cli"), Some(Requester::Cli));
    assert_eq!(requester("ble"), None);
    assert_eq!(requester("fleet"), None);
}

/// A fake mica-deploy at `dir/deploy` that appends its arguments to
/// `dir/calls` and exits with `status`.
fn fake_deploy(dir: &std::path::Path, status: u8) -> FactoryReset {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("deploy");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\necho '{{\"finished\":true}}'\nexit {status}\n",
            dir.join("calls").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    at(dir).with_deploy(script, dir.join("run/factory-reset.json"))
}

fn calls(dir: &std::path::Path) -> String {
    fs::read_to_string(dir.join("calls")).unwrap_or_default()
}

#[test]
fn a_runkit_of_version_2_has_mica_deploy_stage_the_reset() {
    let (dir, _) = device();
    let reset = fake_deploy(dir.path(), 0);
    fs::write(
        dir.path().join("run/runkit.json"),
        r#"{"factoryReset":true,"factoryResetVersion":2}"#,
    )
    .unwrap();
    assert!(reset.supported());
    reset.stage(Requester::Api, "192.0.2.7", 10).unwrap();
    assert_eq!(
        calls(dir.path()),
        "factory-reset --by api --source 192.0.2.7\n"
    );
    assert!(
        !dir.path().join("data/meta").join(erase::REQUEST).exists(),
        "no request of version 1"
    );

    let refusing = fake_deploy(dir.path(), 1);
    assert!(refusing.stage(Requester::Console, "x", 10).is_err());
}

#[test]
fn only_a_boot_that_carried_out_a_reset_finishes_it() {
    let (dir, _) = device();
    let reset = fake_deploy(dir.path(), 0);
    reset.finish().unwrap();
    assert_eq!(calls(dir.path()), "", "no progress, no finish");

    fs::write(
        dir.path().join("run/factory-reset.json"),
        r#"{"v":2,"requested":10,"formatted":true,"unremoved":0}"#,
    )
    .unwrap();
    reset.finish().unwrap();
    assert_eq!(calls(dir.path()), "factory-reset --finish\n");
    assert!(fake_deploy(dir.path(), 1).finish().is_err());
}

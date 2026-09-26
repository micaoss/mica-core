//! `micad` is one executable for two daemons: started as `mica-apid` it is the
//! HTTPS API daemon, under any other name the settings and reconciliation
//! daemon. `/usr/bin/mica-apid` is a symlink to it; the units, not the files,
//! keep the two apart.
use std::{os::unix::fs::symlink, process::Command};

fn run(name: &str, args: &[&str]) -> (bool, String) {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join(name);
    symlink(env!("CARGO_BIN_EXE_micad"), &link).unwrap();
    let output = Command::new(&link).args(args).env_clear().output().unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

#[test]
fn micad_answers_as_micad() {
    let (success, stdout) = run("micad", &["--version"]);
    assert!(success);
    assert!(stdout.starts_with("micad "), "{stdout}");
}

#[test]
fn under_the_name_mica_apid_it_is_the_api_daemon() {
    let (success, stdout) = run("mica-apid", &["--version"]);
    assert!(success);
    assert!(stdout.starts_with("mica-apid "), "{stdout}");

    // The OpenAPI document answers through the link as it did from apid's own
    // executable: the committed document is regenerated this way.
    let (success, stdout) = run("mica-apid", &["--openapi"]);
    assert!(success);
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("the document is JSON");
    assert!(document["openapi"].is_string(), "{document}");
}

/// Only the exact name selects the API daemon: a near miss is micad, which is
/// the answer that cannot open a listener by accident.
#[test]
fn any_other_name_is_micad() {
    for name in ["apid", "mica-apid.old", "micad-apid"] {
        let (success, stdout) = run(name, &["--version"]);
        assert!(success, "{name}");
        assert!(stdout.starts_with("micad "), "{name}: {stdout}");
    }
}

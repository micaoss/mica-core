//! `micad` is one executable for two daemons and a command: `micad` is the
//! settings and reconciliation daemon, `micad api` the HTTPS API daemon and
//! `micad update` the operator's update command. The units, not the files,
//! keep the two daemons apart.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

fn run(args: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_micad"))
        .args(args)
        .env_clear()
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

#[test]
fn micad_answers_as_micad() {
    let (success, stdout) = run(&["--version"]);
    assert!(success);
    assert!(stdout.starts_with("micad "), "{stdout}");
}

#[test]
fn micad_api_is_the_api_daemon() {
    let (success, stdout) = run(&["api", "--version"]);
    assert!(success);
    assert!(stdout.starts_with("mica-apid "), "{stdout}");

    // The OpenAPI document answers through the subcommand as it does from
    // apid's own executable: the committed document is regenerated that way.
    let (success, stdout) = run(&["api", "--openapi"]);
    assert!(success);
    let document: serde_json::Value = serde_json::from_str(&stdout).expect("the document is JSON");
    assert!(document["openapi"].is_string(), "{document}");
}

#[test]
fn micad_update_is_the_update_command() {
    let (success, stdout) = run(&["update", "--help"]);
    assert!(success);
    assert!(stdout.contains("micad update install"), "{stdout}");
    // A misused command line is refused before the bus is dialled.
    let (success, _) = run(&["update", "upgrade"]);
    assert!(!success);
}

/// A mistyped subcommand is refused rather than started as a second daemon.
#[test]
fn an_unknown_command_starts_nothing() {
    for command in ["apid", "install", "serve"] {
        let (success, stdout) = run(&[command]);
        assert!(!success, "{command}");
        assert!(stdout.is_empty(), "{command}: {stdout}");
    }
}

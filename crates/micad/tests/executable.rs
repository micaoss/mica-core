//! `micad` is one executable for two daemons and a command: `micad` is the
//! settings and reconciliation daemon, `mica-apid` (or `micad apid`) the HTTPS
//! API daemon and `mica-update` (or `micad update`) the operator's update
//! command. `/usr/bin/mica-apid` and `/usr/bin/mica-update` are symlinks to
//! it; the units, not the files, keep the two daemons apart.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::{os::unix::fs::symlink, process::Command};

/// Run the executable under `name` with `args`.
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

/// The link and the subcommand are the same API daemon.
#[test]
fn mica_apid_and_micad_apid_are_the_api_daemon() {
    for (name, args) in [
        ("mica-apid", &[][..]),
        ("micad", &["apid"][..]),
        ("micad", &["api"][..]),
    ] {
        let version = [args, &["--version"]].concat();
        let (success, stdout) = run(name, &version);
        assert!(success, "{name} {args:?}");
        assert!(
            stdout.starts_with("mica-apid "),
            "{name} {args:?}: {stdout}"
        );

        // The OpenAPI document answers as it does from apid's own
        // executable: the committed document is regenerated that way.
        let openapi = [args, &["--openapi"]].concat();
        let (success, stdout) = run(name, &openapi);
        assert!(success, "{name} {args:?}");
        let document: serde_json::Value =
            serde_json::from_str(&stdout).expect("the document is JSON");
        assert!(document["openapi"].is_string(), "{document}");
    }
}

/// The link and the subcommand are the same update command.
#[test]
fn mica_update_and_micad_update_are_the_update_command() {
    for (name, args) in [("mica-update", &[][..]), ("micad", &["update"][..])] {
        let help = [args, &["--help"]].concat();
        let (success, stdout) = run(name, &help);
        assert!(success, "{name} {args:?}");
        assert!(stdout.contains("mica-update install"), "{stdout}");
        // A misused command line is refused before the bus is dialled.
        let (success, _) = run(name, &[args, &["upgrade"]].concat());
        assert!(!success, "{name} {args:?}");
    }
}

/// Only the exact link names select another program: a near miss is micad.
#[test]
fn any_other_name_is_micad() {
    for name in ["apid", "mica-apid.old", "micad-apid"] {
        let (success, stdout) = run(name, &["--version"]);
        assert!(success, "{name}");
        assert!(stdout.starts_with("micad "), "{name}: {stdout}");
    }
}

/// A mistyped subcommand is refused rather than started as a second daemon.
#[test]
fn an_unknown_command_starts_nothing() {
    for command in ["install", "serve", "deploy"] {
        let (success, stdout) = run("micad", &[command]);
        assert!(!success, "{command}");
        assert!(stdout.is_empty(), "{command}: {stdout}");
    }
}

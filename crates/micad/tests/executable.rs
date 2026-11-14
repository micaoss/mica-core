//! `micad` is one executable for two daemons and the operator's command:
//! `micad daemon` is the settings and reconciliation daemon, `mica-apid` (or
//! `micad apid`) the HTTPS API daemon, and `mica` the operator's command.
//! `/usr/bin/mica-apid` and `/usr/bin/mica` are symlinks to it; the units, not
//! the files, keep the two daemons apart.
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

/// Run the executable under `name` with `args`, with no system bus to reach:
/// the exit code, stdout and stderr.
fn run_full(name: &str, args: &[&str]) -> (Option<i32>, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join(name);
    symlink(env!("CARGO_BIN_EXE_micad"), &link).unwrap();
    let output = Command::new(&link)
        .args(args)
        .env_clear()
        .env(
            "DBUS_SYSTEM_BUS_ADDRESS",
            format!("unix:path={}", dir.path().join("no-bus").display()),
        )
        .output()
        .unwrap();
    (
        output.status.code(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

/// `mica help` lists the verbs; `mica help <verb>` and `mica <verb> help` are
/// that verb's usage.
#[test]
fn mica_lists_its_verbs_and_each_ones_usage() {
    let (code, stdout, _) = run_full("mica", &["help"]);
    assert_eq!(code, Some(0));
    for verb in ["status", "update", "container", "unit", "doctor", "reboot"] {
        assert!(stdout.contains(&format!("  {verb} ")), "{verb}: {stdout}");
    }
    for (args, usage) in [
        (&["help", "update"][..], "mica update install"),
        (&["update", "help"][..], "mica update install"),
        (&["container", "--help"][..], "mica container up"),
        (&["unit", "help"][..], "mica unit install"),
        (&["doctor", "help"][..], "mica doctor"),
        (&["internal", "board"][..], "mica internal board check"),
    ] {
        let (code, stdout, stderr) = run_full("mica", args);
        assert!(
            stdout.contains(usage) || stderr.contains(usage),
            "{args:?}: {stdout}{stderr}"
        );
        let _ = code;
    }
    // A bare mica is someone looking for a command: the list, exit 2.
    let (code, _, stderr) = run_full("mica", &[]);
    assert_eq!(code, Some(2));
    assert!(stderr.contains("  status "), "{stderr}");
}

/// Full words only: an abbreviation or another spelling is refused with exit
/// 2 before the bus is dialled, as is --json on a command that reads nothing.
#[test]
fn mica_refuses_short_forms_and_other_spellings() {
    for args in [
        &["ctr"][..],
        &["ps"][..],
        &["diag"][..],
        &["diagnostics"][..],
        &["daemon"][..],
        &["apid"][..],
        &["container", "ps"][..],
        &["container", "up", "-p", "app"][..],
        &["update", "-h"][..],
        &["update", "upgrade"][..],
        &["reboot", "now"][..],
        &["reboot", "--json"][..],
        &["internal", "frobnicate"][..],
    ] {
        let (code, stdout, _) = run_full("mica", args);
        assert_eq!(code, Some(2), "{args:?}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
    }
}

/// With no micad to reach, a command exits 3, and --json says so as JSON.
#[test]
fn mica_without_the_bus_exits_3() {
    let (code, _, stderr) = run_full("mica", &["status"]);
    assert_eq!(code, Some(3), "{stderr}");
    assert!(stderr.starts_with("mica status: "), "{stderr}");
    let (code, stdout, stderr) = run_full("mica", &["update", "status", "--json"]);
    assert_eq!(code, Some(3), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    let error: serde_json::Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(error["error"]["code"], "unreachable", "{error}");
}

/// The operator's verbs are mica's: micad points there and exits 2, and the
/// old link names are micad.
#[test]
fn micad_points_its_old_verbs_at_mica() {
    for verb in [
        "update",
        "ctr",
        "unit",
        "web",
        "diag",
        "factory-reset",
        "device-code",
    ] {
        let (code, stdout, stderr) = run_full("micad", &[verb]);
        assert_eq!(code, Some(2), "{verb}");
        assert!(stdout.is_empty(), "{verb}: {stdout}");
        assert!(
            stderr.contains(&format!("use mica {verb}")),
            "{verb}: {stderr}"
        );
    }
    for name in ["mica-update", "mica-ctr"] {
        let (code, stdout, _) = run_full(name, &["--version"]);
        assert_eq!(code, Some(0), "{name}");
        assert!(stdout.starts_with("micad "), "{name}: {stdout}");
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

/// A bare `micad`, someone looking for a command, prints the usage and exits
/// 2: only `micad daemon`, as the units run it, starts the daemon.
#[test]
fn a_bare_micad_prints_the_usage_and_starts_nothing() {
    for args in [&[][..], &["daemon", "now"][..]] {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("micad");
        symlink(env!("CARGO_BIN_EXE_micad"), &link).unwrap();
        let output = Command::new(&link).args(args).env_clear().output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("usage: micad daemon"), "{args:?}: {stderr}");
    }
}

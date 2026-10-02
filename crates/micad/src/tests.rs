use super::{service_scan_enabled, version_line, wants_version};

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}

/// The two spellings, and the positive control for the negatives below.
#[test]
fn both_spellings_of_the_one_flag_are_recognised() {
    assert!(wants_version(argv(&["--version"])));
    assert!(wants_version(argv(&["-V"])));
}

/// Driven from the failing side. Every one of these must fall through into
/// the daemon, and `-v` is the one that matters most: it is the spelling an
/// operator reaches for, it is NOT this flag, and a loose match on it would
/// silently stop micad from starting on any unit that passed it.
#[test]
fn nothing_else_is_this_flag() {
    for args in [
        vec![],
        argv(&["--help"]),
        argv(&["-h"]),
        argv(&["-v"]),
        argv(&["-VV"]),
        argv(&["--versions"]),
        argv(&["--version=1"]),
        argv(&["version"]),
        argv(&["--Version"]),
        argv(&[""]),
    ] {
        assert!(
            !wants_version(args.clone()),
            "argv {args:?} is not --version and must reach the daemon unchanged"
        );
    }
}

/// It is asked of the whole argv, not only of the first element -- the same
/// place clap answers it for `mica-mqttd` and `mica-mqtt-broker`.
#[test]
fn the_flag_is_found_wherever_it_appears() {
    assert!(wants_version(argv(&[
        "--config",
        "/etc/x.toml",
        "--version"
    ])));
}

/// The shape: the binary, one space, one version token, one line.
#[test]
fn the_version_line_names_the_binary_and_the_package_version() {
    let line = version_line("micad");
    let version = line
        .strip_prefix("micad ")
        .unwrap_or_else(|| panic!("got {line:?}"));
    assert!(!version.is_empty(), "an empty version in {line:?}");
    assert!(
        !version.contains(char::is_whitespace),
        "the version must be one token, got {version:?}"
    );
}

/// The pin on the asymmetry: in production `MICAD_SCAN` is inert. A gate
/// that read the variable symmetrically would let `MICAD_SCAN=0` — or any
/// typo — switch the service registry off on a real device.
#[test]
fn production_ignores_micad_scan_entirely() {
    for value in [
        None,
        Some("1"),
        Some("0"),
        Some(""),
        Some("true"),
        Some("no"),
    ] {
        assert!(
            service_scan_enabled(false, value),
            "production must scan whatever MICAD_SCAN holds; \
             got service_scan=false for MICAD_SCAN={value:?}"
        );
    }
}

/// Dry-run on its own constructs no scan, as
/// `tests/scan.rs::dry_run_constructs_no_scan` pins it end to end.
#[test]
fn dry_run_alone_constructs_no_scan() {
    for value in [None, Some("0"), Some(""), Some("true"), Some("yes")] {
        assert!(
            !service_scan_enabled(true, value),
            "dry-run must construct no scan unless MICAD_SCAN is exactly `1`; \
             got service_scan=true for MICAD_SCAN={value:?}"
        );
    }
}

/// The one combination that turns the scan back on.
#[test]
fn dry_run_plus_micad_scan_one_constructs_the_scan() {
    assert!(service_scan_enabled(true, Some("1")));
}

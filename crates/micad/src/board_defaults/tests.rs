use std::fs;

use micad_settings::IfaceSettings;
use tempfile::TempDir;

use super::*;

const CAN0: &str = r#""can0":{"kind":"can","can":{"bitrate":250000,"restartMs":100,"fd":false}}"#;

fn store_in(dir: &Path) -> Store {
    let config = dir.join("config");
    fs::create_dir_all(&config).expect("create the config namespace");
    Store::new(dir.join("settings.toml"), config)
}

fn defaults_with(dir: &Path, network: &str) -> PathBuf {
    let defaults = dir.join("defaults");
    fs::create_dir_all(&defaults).expect("defaults dir");
    fs::write(
        defaults.join("network.json"),
        format!(r#"{{"schema_version":1,"network":{{{network}}}}}"#),
    )
    .expect("defaults");
    defaults
}

fn recorded(dir: &Path) -> String {
    fs::read_to_string(dir.join(RECORD)).expect("record")
}

// A board default the tree lacks is added and saved; one the tree declares
// is the tree's. Both are recorded.
#[test]
fn board_defaults_join_the_tree_and_are_recorded() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let defaults = defaults_with(dir.path(), &format!(r#"{CAN0},"eth0":{{"dhcp":true}}"#));
    let mut settings = Settings::default();
    settings
        .network
        .insert("eth0".into(), IfaceSettings::default());

    let added = seed(&store, dir.path(), &mut settings, &defaults).expect("seed");

    assert_eq!(added, ["can0"]);
    let can = settings.network["can0"].can.clone().expect("can0 added");
    assert_eq!(
        (can.bitrate, can.restart_ms, can.fd),
        (250_000, Some(100), Some(false))
    );
    assert!(
        !settings.network["eth0"].dhcp,
        "the tree's own entry is kept"
    );
    assert_eq!(store.load().expect("reload").network, settings.network);
    assert_eq!(recorded(dir.path()), r#"{"network":["can0","eth0"]}"#);
}

// An entry deleted after it was added stays deleted, and a default the board
// gains later is added on the next boot.
#[test]
fn a_deleted_default_stays_deleted_and_a_new_one_is_added() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let defaults = defaults_with(dir.path(), CAN0);
    let mut settings = Settings::default();
    seed(&store, dir.path(), &mut settings, &defaults).expect("seed");
    settings.network.remove("can0");
    store.save(&settings).expect("delete can0");

    let defaults = defaults_with(
        dir.path(),
        &format!(r#"{CAN0},"can1":{{"kind":"can","can":{{"bitrate":500000}}}}"#),
    );
    let added = seed(&store, dir.path(), &mut settings, &defaults).expect("seed again");

    assert_eq!(added, ["can1"]);
    assert!(!settings.network.contains_key("can0"));
    assert_eq!(store.load().expect("reload").network, settings.network);
    assert_eq!(recorded(dir.path()), r#"{"network":["can0","can1"]}"#);
}

// A defaults document that does not parse or validate adds nothing and
// records nothing.
#[test]
fn refused_board_defaults_add_nothing() {
    for text in [
        "not json",
        r#"{"schema_version":2,"network":{"can0":{"kind":"can","can":{"bitrate":250000}}}}"#,
        r#"{"schema_version":1,"network":{"can0":{"kind":"can","can":{"bitrate":0}}}}"#,
    ] {
        let dir = TempDir::new().expect("tempdir");
        let store = store_in(dir.path());
        let defaults = dir.path().join("defaults");
        fs::create_dir_all(&defaults).expect("defaults dir");
        fs::write(defaults.join("network.json"), text).expect("defaults");
        let mut settings = Settings::default();

        let added = seed(&store, dir.path(), &mut settings, &defaults).expect("seed");

        assert!(added.is_empty(), "{text}");
        assert!(settings.network.is_empty(), "{text}");
        assert!(!dir.path().join(RECORD).exists(), "{text}");
    }
}

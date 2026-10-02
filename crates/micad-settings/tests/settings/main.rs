//! Integration tests for the micad-settings public API.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use micad_settings::{
    DEFAULT_CONFIG_DIR, DEFAULT_PATH, IfaceSettings, NETWORK_SCHEMA_VERSION, Settings,
    StaticConfig, Store,
};
use serde_json::json;
use std::fs;

mod bluetooth;
mod claim_reset;
mod containers;
mod documents;
mod keys;
mod network;
mod paths;
mod refusals;
mod tokens;
mod web;
use documents::*;

/// A store over a temporary tree.
fn store_at(dir: &tempfile::TempDir) -> Store {
    let config = dir.path().join("config");
    fs::create_dir_all(&config).unwrap();
    Store::new(dir.path().join("settings.toml"), config)
}

/// One `/mica/config/` document, parsed.
fn config_document(dir: &tempfile::TempDir, name: &str) -> serde_json::Value {
    let text = fs::read_to_string(dir.path().join("config").join(name)).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn populated() -> Settings {
    let mut settings = Settings::default();
    settings.network.insert(
        "eth0".to_string(),
        IfaceSettings {
            dhcp: false,
            static_: Some(StaticConfig {
                address: "192.168.1.10/24".to_string(),
                gateway: Some("192.168.1.1".to_string()),
                dns: vec!["1.1.1.1".to_string(), "9.9.9.9".to_string()],
            }),
            ..IfaceSettings::default()
        },
    );
    settings.network.insert(
        "wlan0".to_string(),
        IfaceSettings {
            dhcp: true,
            ..IfaceSettings::default()
        },
    );
    settings
}

// --- Store -----------------------------------------------------------------

#[test]
fn save_load_roundtrip_with_network() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    let settings = populated();
    store.save(&settings).unwrap();

    // The `network` subtree is one document with a version of its own.
    let doc = config_document(&dir, "network.json");
    assert_eq!(doc["schema_version"], json!(NETWORK_SCHEMA_VERSION));
    assert_eq!(doc["network"]["wlan0"]["dhcp"], json!(true));

    assert_eq!(store.load().unwrap(), settings);
}

#[test]
fn save_is_atomic_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    store.save(&Settings::default()).unwrap();

    let updated = Settings {
        hostname: "renamed".to_string(),
        ..Settings::default()
    };
    store.save(&updated).unwrap();

    for root in [dir.path().to_path_buf(), dir.path().join("config")] {
        for entry in fs::read_dir(&root).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(
                name == "config" || name == "settings.toml" || name.ends_with(".json"),
                "a temporary file survived the save: {name}"
            );
        }
    }
    assert_eq!(store.load().unwrap(), updated);
}

#[test]
fn load_missing_file_returns_defaults_without_creating_it() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    assert_eq!(store.load().unwrap(), Settings::default());
    assert!(!dir.path().join("settings.toml").exists());
    assert!(
        fs::read_dir(dir.path().join("config"))
            .unwrap()
            .next()
            .is_none(),
        "an absent document is a default, and reading one writes nothing"
    );
}

#[test]
fn default_path_is_the_state_location() {
    assert_eq!(DEFAULT_PATH, "/var/lib/mica/settings.toml");
    assert_eq!(DEFAULT_CONFIG_DIR, "/mica/config");
    let _store = Store::default_path();
}

// --- Dot-path get ----------------------------------------------------------

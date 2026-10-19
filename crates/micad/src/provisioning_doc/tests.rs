use super::*;
use micad_settings::encode_base64_nopad;
use micad_settings::{Settings, Store};
use std::fs;
use std::path::Path;
use std::path::PathBuf;

mod apply;
mod claim;
mod media;
mod refusals;

/// A password the sentinel test can find anywhere it leaked, and which is
/// long enough to be accepted.
const SECRET_PASSWORD: &str = "PW-SENTINEL-8a3f-do-not-log";

/// The same, for the WiFi pre-shared key. A DIFFERENT string, so a test
/// that finds one cannot be satisfied by the other.
const SECRET_PSK: &str = "PSK-SENTINEL-4c7e";

/// The same sentinel, padded past IEEE 802.11i's longest passphrase. Used
/// where a REFUSAL about a pre-shared key is wanted: the refusal must name
/// neither the value nor its length, and this is the string that proves it.
fn unusable_psk() -> String {
    format!("{SECRET_PSK}{}", "x".repeat(50))
}

fn settings_path(dir: &Path) -> PathBuf {
    dir.join("settings.toml")
}

fn store_in(dir: &Path) -> Store {
    // The `/mica/config/` namespace has to exist: an absent document is a
    // default, an absent namespace is the DATA medium being gone, and the
    // store refuses that rather than defaulting.
    let config = dir.join("config");
    fs::create_dir_all(&config).expect("create the config namespace");
    Store::new(settings_path(dir), config)
}

/// Write `body` as the document of `source`, under a fresh staging root.
fn stage(dir: &Path, source: Source, body: &str) -> PathBuf {
    let root = dir.join("staging");
    let source_dir = root.join(source.dir_name());
    fs::create_dir_all(&source_dir).expect("create staging dir");
    fs::write(source_dir.join(DOCUMENT_FILE_NAME), body).expect("write document");
    root
}

/// A structurally valid authorized-key line, built from the crate's own
/// encoder rather than pasted from anywhere.
fn key_line() -> String {
    let key_type = "ssh-ed25519";
    let mut bytes = Vec::new();
    let name = key_type.as_bytes();
    bytes.extend_from_slice(
        &u32::try_from(name.len())
            .expect("name length")
            .to_be_bytes(),
    );
    bytes.extend_from_slice(name);
    while bytes.len() < 64 {
        let index = u8::try_from(bytes.len()).expect("index fits");
        bytes.push(index.wrapping_mul(11).wrapping_add(5));
    }
    let mut encoded = encode_base64_nopad(&bytes);
    while !encoded.len().is_multiple_of(4) {
        encoded.push('=');
    }
    format!("{key_type} {encoded}")
}

/// A document exercising every section, secrets included.
fn full_document() -> String {
    format!(
        r#"
# A factory document, with the comments and blank lines a real one carries.
version = 1

[identity]
deviceId = "0123456789abcdef0123456789abcdef"

[admin]
password = "{SECRET_PASSWORD}"
authorizedKeys = ["{key}"]

[network.eth0]
dhcp = true

[wifi]
enabled = true
interface = "wlan0"

[[wifi.networks]]
ssid = "site-ap"
psk = "{SECRET_PSK}"
priority = 10

[time]
timezone = "Europe/Berlin"

[time.ntp]
servers = ["0.pool.ntp.org", "192.0.2.7"]
"#,
        key = key_line()
    )
}

/// The smallest document that does anything, and which claims nothing.
fn time_only_document() -> &'static str {
    r#"
version = 1

[time]
timezone = "Europe/Berlin"
"#
}

/// Import `body` from `source` against a fresh STATE, returning the store,
/// the resulting tree and the outcome.
fn import_body(dir: &Path, source: Source, body: &str) -> (Store, Settings, Outcome) {
    let store = store_in(dir);
    let root = stage(dir, source, body);
    let mut settings = Settings::default();
    let outcome = import(&store, &mut settings, &root).expect("import");
    (store, settings, outcome)
}

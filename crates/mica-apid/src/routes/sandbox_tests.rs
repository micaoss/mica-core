//! `dist/apid.service` runs apid under `ProtectSystem=strict`: every tree apid
//! writes must be one the unit makes writable, or the write fails with EROFS
//! on a device and nowhere else.

use std::path::{Path, PathBuf};

const UNIT: &str = include_str!("../../dist/apid.service");

/// Every directory apid writes into, on a device.
fn written() -> Vec<PathBuf> {
    vec![
        PathBuf::from(super::paths::UPDATE_UPLOADS_DIR),
        PathBuf::from(crate::bundle::DEFAULT_ROOT),
        PathBuf::from(crate::diagnostics::DEFAULT_ROOT),
        PathBuf::from(crate::config::DEFAULT_STATE_DIR),
        // A spent presence assertion is unlinked: a write on its directory.
        Path::new(micad_settings::DEFAULT_PRESENCE_MARKER_PATH)
            .parent()
            .unwrap()
            .to_path_buf(),
    ]
}

/// The trees the unit makes writable: `ReadWritePaths=` (a `-` prefix only
/// makes a missing path non-fatal), `StateDirectory=` under /var/lib and
/// `RuntimeDirectory=` under /run.
fn writable() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in UNIT.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let base = match key {
            "ReadWritePaths" => "/",
            "StateDirectory" => "/var/lib",
            "RuntimeDirectory" => "/run",
            _ => continue,
        };
        for item in value.split_whitespace() {
            out.push(Path::new(base).join(item.trim_start_matches('-').trim_start_matches('/')));
        }
    }
    out
}

#[test]
fn the_unit_runs_under_strict() {
    assert!(UNIT.lines().any(|l| l.trim() == "ProtectSystem=strict"));
}

#[test]
fn every_directory_apid_writes_is_writable_in_its_unit() {
    let writable = writable();
    let missing: Vec<_> = written()
        .into_iter()
        .filter(|dir| !writable.iter().any(|w| dir.starts_with(w)))
        .collect();
    assert!(
        missing.is_empty(),
        "read-only under apid.service: {missing:?}; writable: {writable:?}"
    );
}

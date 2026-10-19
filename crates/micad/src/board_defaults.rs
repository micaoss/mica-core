//! A board's default settings: network entries the board package ships, each
//! added to the tree once.
//!
//! `<defaults>/network.json` is a `network.json` document. Every boot, each
//! entry it declares that STATE's record does not name is added when the tree
//! lacks it and is then recorded, so a device that updates onto a board with
//! a new default gets it, an entry the tree already had is the tree's, and an
//! entry the operator deleted stays deleted. A configuration or full-factory
//! reset removes the record with the settings, and the defaults come back.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use micad_settings::{
    IfaceSettings, NETWORK_DOCUMENT, NETWORK_SCHEMA_VERSION, NetworkDocument, Settings, Store,
};
use serde::{Deserialize, Serialize};

/// STATE's record of the defaults already added, relative to the state
/// directory.
pub const RECORD: &str = "seeded-defaults.json";
const RECORD_MODE: u32 = 0o600;

/// The entries of each subtree whose board default has been added.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Record {
    network: BTreeSet<String>,
}

/// Where a board's default settings live: `$MICAD_DEFAULTS_DIR`, else
/// `/usr/share/mica/defaults`.
pub fn defaults_dir() -> PathBuf {
    std::env::var_os("MICAD_DEFAULTS_DIR")
        .map_or_else(|| "/usr/share/mica/defaults".into(), Into::into)
}

/// The board's network entries, or the reason the document is refused. None
/// when there is no document.
fn board_network(defaults: &Path) -> Result<BTreeMap<String, IfaceSettings>, String> {
    let path = defaults.join(NETWORK_DOCUMENT);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(err.to_string()),
    };
    let document: NetworkDocument = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    if document.schema_version != NETWORK_SCHEMA_VERSION {
        return Err(format!(
            "schema version {} is not {NETWORK_SCHEMA_VERSION}",
            document.schema_version
        ));
    }
    for (iface, cfg) in &document.network {
        micad_settings::check_iface_name("network interface", iface)?;
        if let Some(wireguard) = &cfg.wireguard {
            micad_settings::validate_peers(iface, &wireguard.peers)?;
        }
        micad_settings::validate_can(iface, cfg)?;
    }
    Ok(document.network)
}

/// Add the board's default entries from `defaults` that STATE's record in
/// `state_dir` does not name, save the tree and record them. Returns the
/// entries added. A refused defaults document or record is logged and adds
/// nothing.
pub fn seed(
    store: &Store,
    state_dir: &Path,
    settings: &mut Settings,
    defaults: &Path,
) -> Result<Vec<String>> {
    let board = match board_network(defaults) {
        Ok(board) => board,
        Err(detail) => {
            tracing::error!(path = %defaults.join(NETWORK_DOCUMENT).display(), detail, "board default settings refused; skipped");
            return Ok(Vec::new());
        }
    };
    let record_path = state_dir.join(RECORD);
    let mut record: Record = match std::fs::read_to_string(&record_path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(record) => record,
            Err(err) => {
                tracing::error!(path = %record_path.display(), error = %err, "record of board defaults refused; none added");
                return Ok(Vec::new());
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Record::default(),
        Err(err) => return Err(err).with_context(|| format!("read {}", record_path.display())),
    };
    let new: BTreeMap<_, _> = board
        .into_iter()
        .filter(|(iface, _)| !record.network.contains(iface))
        .collect();
    if new.is_empty() {
        return Ok(Vec::new());
    }

    let mut seeded = settings.clone();
    let mut added = Vec::new();
    for (iface, cfg) in &new {
        if !seeded.network.contains_key(iface) {
            seeded.network.insert(iface.clone(), cfg.clone());
            added.push(iface.clone());
        }
    }
    if let Err(detail) = micad_settings::validate_topology(&seeded.network) {
        tracing::error!(
            detail,
            "board default settings do not fit the tree; skipped"
        );
        return Ok(Vec::new());
    }
    if !added.is_empty() {
        store.save(&seeded).context("persist the board defaults")?;
        *settings = seeded;
    }
    record.network.extend(new.into_keys());
    let text = serde_json::to_string(&record).context("render the record of board defaults")?;
    crate::fswrite::write_config_if_changed(&record_path, &text, RECORD_MODE)?;
    tracing::info!(?added, "board defaults checked");
    Ok(added)
}

#[cfg(test)]
mod tests;

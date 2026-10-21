//! `micad diag`: a diagnostic snapshot made on the device, with no apid.
//!
//! The snapshot is the one `POST /api/v1/diagnostics` makes, collected over
//! the system bus and published to the same store, so it is there whatever
//! `access.web.mode` says.

use std::path::PathBuf;

use crate::diagnostics::{Collector, SnapshotStore};
use crate::settings_api::SettingsApi;

const USAGE: &str = "\
usage: micad diag

  collect a diagnostic snapshot, redacted as the API's, into /mica/diagnostics,
  and print its path";

/// `micad diag`'s entry point: exit 0 done, 1 failed, 2 misused.
pub fn main(args: &[String]) -> anyhow::Result<()> {
    match args {
        [] => {}
        [help] if matches!(help.as_str(), "help" | "-h" | "--help") => {
            println!("{USAGE}");
            return Ok(());
        }
        [other, ..] => {
            eprintln!("micad diag: unexpected argument: {other}\n{USAGE}");
            std::process::exit(2);
        }
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let api = crate::bus_client::BusSettings::new(crate::config::BusKind::System);
    match runtime.block_on(snapshot(&api, &SnapshotStore::at_default())) {
        Ok(path) => {
            println!("{}", path.display());
            Ok(())
        }
        Err(error) => {
            eprintln!("micad diag: {error:#}");
            std::process::exit(1);
        }
    }
}

/// Collect a snapshot through `api` and publish it to `store`; its path.
async fn snapshot(api: &dyn SettingsApi, store: &SnapshotStore) -> anyhow::Result<PathBuf> {
    let collected = Collector::new(api).collect().await;
    let summary = store.publish(&collected.snapshot)?;
    Ok(store.path_of(summary.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The snapshot lands in the store, and the path printed is its file.
    #[tokio::test]
    async fn a_snapshot_is_published_and_its_path_named() {
        let dir = tempfile::tempdir().unwrap();
        let store = SnapshotStore::new(dir.path());
        let api = crate::settings_api::FakeSettings::new(serde_json::json!({ "hostname": "mica" }));
        let path = snapshot(&api, &store).await.unwrap();
        assert_eq!(path, dir.path().join("1.json"));
        let written: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(written["schemaVersion"].is_number(), "{written}");
    }
}

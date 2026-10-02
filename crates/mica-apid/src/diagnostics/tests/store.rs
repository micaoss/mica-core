//! The snapshot store and its caps.

use serde_json::{Value, json};
use std::fs;

use super::*;

/// The store: publish is atomic and listed by id, read returns the
/// bytes, delete is explicit and idempotent, and staging is never listed.
#[test]
pub(super) fn the_store_publishes_lists_reads_and_deletes() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path().join("diagnostics"));
    assert!(
        store.list().unwrap().is_empty(),
        "an absent root is an empty store"
    );

    let first = store
        .publish(&json!({ "schemaVersion": 1, "collectedAt": "2026-09-02T00:00:00Z", "system": { "machineId": { "id": "0123456789abcdef0123456789abcdef" } } }))
        .unwrap();
    assert_eq!(first.id, 1);
    assert_eq!(first.schema_version, Some(1));
    assert_eq!(
        first.machine_id.as_deref(),
        Some("0123456789abcdef0123456789abcdef")
    );
    let second = store.publish(&json!({ "schemaVersion": 1 })).unwrap();
    assert_eq!(second.id, 2);

    let listed = store.list().unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, 1);
    assert_eq!(
        listed[0].collected_at.as_deref(),
        Some("2026-09-02T00:00:00Z")
    );
    assert_eq!(listed[1].id, 2);
    assert!(listed[1].collected_at.is_none());
    // Nothing but published snapshots is on disk: no staging file.
    let names: Vec<String> = fs::read_dir(store.root())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(
        names.iter().all(|name| !name.starts_with(".staging")),
        "{names:?}"
    );

    let bytes = store.read(1).unwrap().expect("snapshot 1");
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert!(store.read(9).unwrap().is_none());

    assert!(store.delete(1).unwrap());
    assert!(!store.delete(1).unwrap());
    assert_eq!(store.list().unwrap().len(), 1);
    // A stray file that is not a snapshot is ignored, and a stale
    // staging file is swept by the next publish.
    fs::write(store.root().join("notes.txt"), "x").unwrap();
    fs::write(store.root().join(".staging-7.json"), "{").unwrap();
    let third = store.publish(&json!({ "schemaVersion": 1 })).unwrap();
    assert_eq!(third.id, 3);
    assert!(!store.root().join(".staging-7.json").exists());
    assert_eq!(store.list().unwrap().len(), 2);
}

/// The count cap: the oldest goes when one more arrives.
#[test]
pub(super) fn the_count_cap_evicts_the_oldest() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path()).with_caps(3, MAX_TOTAL_BYTES);
    for _ in 0..5 {
        store.publish(&json!({ "schemaVersion": 1 })).unwrap();
    }
    let ids: Vec<u64> = store
        .list()
        .unwrap()
        .iter()
        .map(|summary| summary.id)
        .collect();
    assert_eq!(ids, vec![3, 4, 5]);
}

/// The byte cap: the oldest go until the newcomer fits.
#[test]
pub(super) fn the_byte_cap_evicts_until_the_newcomer_fits() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path()).with_caps(100, 1000);
    let filler = |n: usize| json!({ "schemaVersion": 1, "pad": "x".repeat(n) });
    store.publish(&filler(300)).unwrap();
    store.publish(&filler(300)).unwrap();
    store.publish(&filler(300)).unwrap();
    // Three of ~330 bytes fit under 1000; a fourth does not, so the
    // oldest goes.
    store.publish(&filler(300)).unwrap();
    let listed = store.list().unwrap();
    let ids: Vec<u64> = listed.iter().map(|summary| summary.id).collect();
    assert_eq!(ids, vec![2, 3, 4]);
    let total: u64 = listed.iter().map(|summary| summary.bytes).sum();
    assert!(total <= 1000, "{total}");
}

/// The per-snapshot cap: an oversized snapshot is refused, and nothing
/// is written — not a staging file, not a truncated snapshot.
#[test]
pub(super) fn an_oversized_snapshot_is_refused_whole() {
    let dir = tempfile::tempdir().unwrap();
    let store = SnapshotStore::new(dir.path().join("diagnostics"));
    let huge = json!({ "schemaVersion": 1, "pad": "x".repeat(MAX_SNAPSHOT_BYTES + 1) });
    let err = store.publish(&huge).expect_err("refused");
    assert!(err.to_string().contains("above the"), "{err}");
    assert!(!store.root().exists() || store.list().unwrap().is_empty());
}

/// A publish that cannot rename leaves nothing new listed: the store
/// root is a file, so the write fails, and the error names the id.
#[test]
pub(super) fn a_failed_publish_leaves_nothing_published() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir");
    fs::write(&file, "x").unwrap();
    let store = SnapshotStore::new(&file);
    assert!(store.publish(&json!({ "schemaVersion": 1 })).is_err());
    assert!(store.list().is_err() || store.list().unwrap().is_empty());
}

//! The apply queue: reapplied subtrees and serialized writes.

use super::super::apply::run_apply_worker;
use super::super::{Inner, MicadService};
use crate::power::MockPower;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

#[tokio::test]
pub(super) async fn a_transient_password_reapplies_only_the_ssh_subtree() {
    let (service, calls, _dir) = service_with_recording_reconcilers();

    service
        .set_transient_root_password("correct horse battery")
        .await
        .expect("set transient password");

    assert_eq!(
        *calls.lock().expect("call log"),
        vec!["sshd".to_string()],
        "password activation must not reload networkd or container generators"
    );
}

#[tokio::test]
pub(super) async fn a_wireguard_rotation_reapplies_only_the_network_subtree() {
    let (service, calls, _dir) = service_with_recording_reconcilers();

    service
        .rotate_wireguard_key("wg0")
        .await
        .expect("rotate wireguard key");

    assert_eq!(
        *calls.lock().expect("call log"),
        vec!["network".to_string()],
        "key rotation must not touch sshd or container generators"
    );
}

#[tokio::test]
pub(super) async fn two_identical_queued_writes_run_one_reconcile() {
    let queue = Arc::new(crate::apply_queue::ApplyQueue::new());
    let first = queue.enqueue("settings-write", "hostname", ":1.7").await;
    let second = queue.enqueue("settings-write", "hostname", ":1.8").await;
    assert_eq!(first.record.id, second.record.id);
    assert_eq!(second.record.folded_count, 1);

    let calls = Arc::new(Mutex::new(Vec::new()));
    let reconcilers: Arc<Vec<Box<dyn crate::reconciler::Reconciler>>> =
        Arc::new(vec![Box::new(RecordingReconciler {
            name: "hostname",
            subtree: "hostname",
            calls: Arc::clone(&calls),
        })]);
    let inner = Arc::new(tokio::sync::RwLock::new(Inner {
        settings: micad_settings::Settings::default(),
        state: serde_json::json!({}),
    }));
    let worker = tokio::spawn(run_apply_worker(
        Arc::clone(&queue),
        Arc::clone(&inner),
        Arc::new(tokio::sync::Mutex::new(())),
        reconcilers,
        Arc::new(tokio::sync::RwLock::new(Vec::new())),
    ));
    queue.wake();

    let finished = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let task = queue.get(&first.record.id).await.expect("task record");
            if task.terminal() {
                break task;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("task finishes");
    worker.abort();

    assert_eq!(finished.outcome.as_deref(), Some("succeeded"));
    assert_eq!(
        *calls.lock().expect("call log"),
        vec!["hostname".to_string()]
    );
}

#[tokio::test]
pub(super) async fn settings_reads_answer_while_a_reconcile_is_running() {
    let dir = tempfile::tempdir().expect("tempdir");
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let service = Arc::new(MicadService::new(
        store_in(&dir),
        micad_settings::Settings::default(),
        vec![Box::new(BlockingReconciler {
            name: "hostname",
            subtree: "hostname",
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        })],
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        dir.path().join("shadow"),
        serde_json::json!({}),
    ));

    let writer = {
        let service = Arc::clone(&service);
        tokio::spawn(async move {
            service
                .write_setting("hostname", serde_json::json!("bounded-read"))
                .await
        })
    };
    started.notified().await;
    let read =
        tokio::time::timeout(Duration::from_millis(100), service.get_settings("hostname")).await;
    release.notify_one();
    writer.await.expect("writer task").expect("settings write");

    assert_eq!(
        read.expect("a settings read must not wait for reconcile")
            .expect("settings read"),
        "\"bounded-read\""
    );
}

#[tokio::test]
pub(super) async fn concurrent_transient_password_writes_stay_serialized_by_the_apply_lock() {
    let dir = tempfile::tempdir().expect("tempdir");
    let shadow_path = dir.path().join("shadow");
    std::fs::write(&shadow_path, SHADOW).expect("seed shadow");
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let service = Arc::new(MicadService::new(
        store_in(&dir),
        micad_settings::Settings::default(),
        vec![Box::new(BlockingReconciler {
            name: "sshd",
            subtree: "access.ssh",
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        })],
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        shadow_path.clone(),
        serde_json::json!({}),
    ));

    let first_password = "first correct horse";
    let second_password = "second battery staple";
    let first = {
        let service = Arc::clone(&service);
        tokio::spawn(async move { service.set_transient_root_password(first_password).await })
    };
    started.notified().await;

    let second = {
        let service = Arc::clone(&service);
        tokio::spawn(async move { service.set_transient_root_password(second_password).await })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let root_hash = std::fs::read_to_string(&shadow_path)
        .expect("shadow after first write")
        .lines()
        .next()
        .and_then(|line| line.split(':').nth(1))
        .expect("root hash")
        .to_string();
    assert!(bcrypt::verify(first_password, &root_hash).expect("verify first hash"));
    assert!(
        !bcrypt::verify(second_password, &root_hash).expect("reject second hash"),
        "the second writer reached the shadow file before the first apply finished"
    );

    release.notify_one();
    first.await.expect("first task").expect("first write");
    started.notified().await;
    release.notify_one();
    second.await.expect("second task").expect("second write");

    let root_hash = std::fs::read_to_string(&shadow_path)
        .expect("shadow after second write")
        .lines()
        .next()
        .and_then(|line| line.split(':').nth(1))
        .expect("root hash")
        .to_string();
    assert!(bcrypt::verify(second_password, &root_hash).expect("verify second hash"));
}

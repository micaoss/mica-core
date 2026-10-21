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

/// micad claims its bus name while the container reconciler still waits on
/// mica-containerd: the startup apply leaves that reconciler to the deferred
/// apply, and the deferred apply holds no lock the name claim needs.
#[tokio::test(flavor = "multi_thread")]
pub(super) async fn the_bus_name_is_claimed_while_the_container_reconciler_waits() {
    use std::io::BufRead;
    // A private bus: this asserts real name ownership, and must not skip.
    let mut bus = std::process::Command::new("dbus-daemon")
        .args(["--session", "--print-address=1", "--nofork"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("dbus-daemon is needed for this test");
    let mut address = String::new();
    std::io::BufReader::new(bus.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();

    let dir = tempfile::tempdir().expect("tempdir");
    let calls = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let service = MicadService::new(
        store_in(&dir),
        micad_settings::Settings::default(),
        vec![
            Box::new(RecordingReconciler {
                name: "hostname",
                subtree: "hostname",
                calls: Arc::clone(&calls),
            }),
            Box::new(BlockingReconciler {
                name: "container",
                subtree: "container",
                started: Arc::clone(&started),
                release: Arc::clone(&release),
            }),
        ],
        Box::new(MockPower {
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
        dir.path().join("shadow"),
        serde_json::json!({}),
    );
    let inner = Arc::clone(&service.inner);

    // The startup apply returns without the container reconciler.
    let deferred = tokio::time::timeout(Duration::from_secs(5), service.apply_startup())
        .await
        .expect("the startup apply must not wait on the container reconciler");
    assert_eq!(*calls.lock().expect("call log"), ["hostname"]);

    let connection = zbus::connection::Builder::address(address.trim())
        .serve_at(super::super::OBJECT_PATH, service)
        .build()
        .await
        .expect("serve micad on the private bus");
    let applying = tokio::spawn(deferred.run());
    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("the deferred apply reaches the container reconciler");

    // The reconciler is waiting, and the name is claimed regardless.
    tokio::time::timeout(
        Duration::from_secs(5),
        connection.request_name(super::super::BUS_NAME),
    )
    .await
    .expect("claiming the name must not wait on the container reconciler")
    .expect("request the bus name");
    assert!(inner.read().await.state.get("container").is_none());

    // Its outcome is recorded once it answers, as every apply's is.
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), applying)
        .await
        .expect("the deferred apply finishes")
        .expect("deferred apply task");
    assert_eq!(
        inner.read().await.state["container"],
        serde_json::json!({"applied": true})
    );
    drop(connection);
    let _ = bus.kill();
    let _ = bus.wait();
}

/// Fails its first `failures` applies, as the container reconciler does while
/// mica-containerd is not ready yet, then succeeds.
struct NotReadyReconciler {
    failures: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl crate::reconciler::Reconciler for NotReadyReconciler {
    fn name(&self) -> &'static str {
        "container"
    }

    fn subtree(&self) -> &'static str {
        "container"
    }

    async fn apply(
        &self,
        _settings: &micad_settings::Settings,
    ) -> anyhow::Result<serde_json::Value> {
        let left = self.failures.load(std::sync::atomic::Ordering::SeqCst);
        if left > 0 {
            self.failures
                .store(left - 1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("mica-containerd did not answer within 0 seconds of starting");
        }
        Ok(serde_json::json!({"applied": true}))
    }
}

/// A deferred apply that failed because the daemon was not ready is tried
/// again, and its outcome replaces the failure once the daemon answers; one
/// that keeps failing stops retrying, with the failure recorded.
#[tokio::test]
pub(super) async fn a_deferred_apply_is_retried_while_the_daemon_is_not_ready() {
    let service_with = |failures| {
        let dir = tempfile::tempdir().expect("tempdir");
        let service = MicadService::new(
            store_in(&dir),
            micad_settings::Settings::default(),
            vec![Box::new(NotReadyReconciler {
                failures: std::sync::atomic::AtomicUsize::new(failures),
            })],
            Box::new(MockPower {
                calls: Arc::new(Mutex::new(Vec::new())),
            }),
            dir.path().join("shadow"),
            serde_json::json!({}),
        );
        (service, dir)
    };

    let (service, _dir) = service_with(2);
    service
        .apply_startup()
        .await
        .with_retry(Duration::from_millis(10), Duration::from_secs(5))
        .run()
        .await;
    assert_eq!(
        service.inner.read().await.state["container"],
        serde_json::json!({"applied": true})
    );

    let (service, _dir) = service_with(usize::MAX);
    service
        .apply_startup()
        .await
        .with_retry(Duration::from_millis(10), Duration::from_millis(50))
        .run()
        .await;
    assert!(
        service.inner.read().await.state["container"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("did not answer")),
        "{}",
        service.inner.read().await.state
    );
}

/// On either init, only the reconcilers that render files and make bus calls
/// run before micad claims its name; every one that starts or restarts a
/// service is deferred, and every deferred name is a reconciler's.
#[test]
pub(super) fn only_the_reconcilers_that_start_no_service_run_before_the_name() {
    use super::super::apply::DEFERRED;
    for init in [micad_settings::Init::Systemd, micad_settings::Init::Openrc] {
        let names: Vec<&str> = crate::reconciler::for_features(
            &micad_settings::Features::all(),
            init,
            micad_settings::ContainerEngine::Podman,
        )
        .iter()
        .map(|reconciler| reconciler.name())
        .collect();
        let before: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| !DEFERRED.contains(name))
            .collect();
        assert_eq!(before, ["hostname", "network", "web"], "{init:?}");
        for name in DEFERRED {
            assert!(names.contains(&name), "{name} under {init:?}");
        }
    }
}

/// Several paths are one write: every value lands, or a refused one leaves
/// the tree as it was.
#[tokio::test]
pub(super) async fn several_paths_land_together_or_not_at_all() {
    let (service, _calls, _dir) = service_with_mock();

    service
        .persist_settings(&[
            ("wifi.client.enabled".to_string(), serde_json::json!(false)),
            ("wifi.ap.mode".to_string(), serde_json::json!("always")),
        ])
        .await
        .expect("both land");
    let settings = service.inner.read().await.settings.clone();
    assert!(!settings.wifi.client.enabled);
    assert_eq!(settings.wifi.ap.mode, micad_settings::ApMode::Always);

    let error = service
        .persist_settings(&[
            ("wifi.client.enabled".to_string(), serde_json::json!(true)),
            ("wifi.ap.mode".to_string(), serde_json::json!("sometimes")),
        ])
        .await
        .expect_err("the second value is not a mode");
    assert!(format!("{error}").contains("wifi.ap.mode"), "{error}");
    let settings = service.inner.read().await.settings.clone();
    assert!(
        !settings.wifi.client.enabled,
        "the first value landed alone"
    );
}

/// The task of a write to several paths applies what they share.
#[test]
pub(super) fn several_paths_apply_the_subtree_they_share() {
    use super::super::interface::shared_path;
    let paths = |paths: &[&str]| {
        paths
            .iter()
            .map(|path| (*path).to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shared_path(&paths(&["wifi.client.enabled", "wifi.ap.mode"])),
        "wifi"
    );
    assert_eq!(shared_path(&paths(&["dns.global"])), "dns.global");
    assert_eq!(shared_path(&paths(&["wifi.ap.mode", "network.wlan0"])), "");
    assert_eq!(
        shared_path(&paths(&[
            "services.units.\"a.service\".enabled",
            "services.units.\"a.service\".text"
        ])),
        "services.units.\"a.service\""
    );
}

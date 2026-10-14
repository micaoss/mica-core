use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::containerd::{ContainerdError, Verb};
use crate::reconciler::systemd::mock::MockUnitControl;

/// A daemon that records what it was asked and answers from its fields.
#[derive(Default)]
struct FakeDaemon {
    calls: Mutex<Vec<String>>,
    /// Status calls that fail before one answers.
    silent: Mutex<usize>,
    /// What `GET /v1/containers` lists.
    listed: Vec<&'static str>,
    refuse: bool,
}

#[async_trait::async_trait]
impl Containerd for FakeDaemon {
    async fn status(&self) -> Result<Value, ContainerdError> {
        self.calls.lock().unwrap().push("status".into());
        let mut silent = self.silent.lock().unwrap();
        if *silent > 0 {
            *silent -= 1;
            return Err(ContainerdError::Unavailable("not yet".into()));
        }
        Ok(json!({ "version": "test" }))
    }

    async fn declare(
        &self,
        units: &BTreeMap<String, ContainerUnit>,
    ) -> Result<Value, ContainerdError> {
        let names: Vec<&str> = units.keys().map(String::as_str).collect();
        self.calls
            .lock()
            .unwrap()
            .push(format!("declare {}", names.join(",")));
        if self.refuse {
            return Err(ContainerdError::Refused {
                status: 400,
                rule: "image".into(),
                detail: "no image".into(),
            });
        }
        Ok(
            json!({ "containers": names.iter().map(|n| json!({ "spec": { "name": n }, "phase": "running" })).collect::<Vec<_>>() }),
        )
    }

    async fn containers(&self) -> Result<Value, ContainerdError> {
        self.calls.lock().unwrap().push("containers".into());
        Ok(
            json!({ "containers": self.listed.iter().map(|n| json!({ "spec": { "name": n } })).collect::<Vec<_>>() }),
        )
    }

    async fn act(&self, _name: &str, _verb: Verb) -> Result<Value, ContainerdError> {
        unreachable!("the reconciler does not act on one container")
    }
}

/// An engine whose `podman ps` lists `names` for the first `polls` reads.
struct FakeEngine {
    names: Vec<&'static str>,
    polls: Mutex<usize>,
}

#[async_trait::async_trait]
impl ContainerEngine for FakeEngine {
    async fn containers(&self) -> Value {
        let mut polls = self.polls.lock().unwrap();
        let names: Vec<Value> = if *polls > 0 {
            *polls -= 1;
            self.names.iter().map(|n| json!({ "Names": [n] })).collect()
        } else {
            Vec::new()
        };
        json!({ "available": true, "entries": names })
    }
}

fn reconciler(
    active: &str,
    file: &str,
    daemon: FakeDaemon,
    engine_polls: usize,
    leftovers: PathBuf,
) -> ContainerReconciler<MockUnitControl> {
    ContainerReconciler::new(
        MockUnitControl::new(active, file),
        Arc::new(daemon),
        Arc::new(FakeEngine {
            names: vec!["web"],
            polls: Mutex::new(engine_polls),
        }),
        leftovers,
    )
    .with_waits(
        Duration::from_millis(200),
        Duration::from_millis(200),
        Duration::from_millis(5),
    )
}

fn settings(enabled: bool) -> Settings {
    let mut settings = Settings::default();
    settings.container.enabled = enabled;
    settings.container.units.insert(
        "web".into(),
        ContainerUnit {
            image: "docker.io/library/nginx:1.27".into(),
            command: Vec::new(),
            environment: BTreeMap::new(),
            publish: Vec::new(),
            volumes: Vec::new(),
            restart: micad_settings::RestartPolicy::default(),
            auto_start: true,
            pids: None,
            memory: None,
            cpu: None,
        },
    );
    settings
}

/// On: the service enabled and started, then every declared container in one PUT.
#[tokio::test]
async fn turning_on_starts_the_daemon_then_declares_every_container() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Arc::new(FakeDaemon {
        silent: Mutex::new(2),
        ..FakeDaemon::default()
    });
    let r = ContainerReconciler::new(
        MockUnitControl::new("inactive", "disabled"),
        daemon.clone(),
        Arc::new(FakeEngine {
            names: vec![],
            polls: Mutex::new(0),
        }),
        dir.path().to_path_buf(),
    )
    .with_waits(
        Duration::from_millis(200),
        Duration::from_millis(200),
        Duration::from_millis(5),
    );

    let state = r.apply(&settings(true)).await.unwrap();

    assert_eq!(
        r.control.calls(),
        [
            format!("enable {SERVICE_UNIT}"),
            format!("start {SERVICE_UNIT}")
        ]
    );
    let calls = daemon.calls.lock().unwrap().clone();
    assert_eq!(calls, ["status", "status", "status", "declare web"]);
    assert_eq!(state["containers"][0]["spec"]["name"], json!("web"));
    assert_eq!(state["declaredCount"], json!(1));
}

/// A running daemon is not restarted; the declaration is still sent, so a
/// settings change reaches it.
#[tokio::test]
async fn a_running_daemon_is_only_declared_to() {
    let dir = tempfile::tempdir().unwrap();
    let r = reconciler(
        "active",
        "enabled",
        FakeDaemon::default(),
        0,
        dir.path().into(),
    );
    r.apply(&settings(true)).await.unwrap();
    assert!(r.control.calls().is_empty(), "{:?}", r.control.calls());
}

/// A daemon that never answers is an error naming it, not an empty success.
#[tokio::test]
async fn a_daemon_that_never_answers_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon {
        silent: Mutex::new(usize::MAX),
        ..FakeDaemon::default()
    };
    let r = reconciler("active", "enabled", daemon, 0, dir.path().into());
    let err = r.apply(&settings(true)).await.unwrap_err().to_string();
    assert!(err.contains("did not answer"), "{err}");
}

/// A refusal of the declaration is the pass's error, with the daemon's rule.
#[tokio::test]
async fn a_refused_declaration_is_the_passes_error() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon {
        refuse: true,
        ..FakeDaemon::default()
    };
    let r = reconciler("active", "enabled", daemon, 0, dir.path().into());
    let err = format!("{:#}", r.apply(&settings(true)).await.unwrap_err());
    assert!(err.contains("refused `image`"), "{err}");
}

/// Off: nothing declared, every container gone, and only then the service
/// stopped and disabled -- a daemon stopped first leaves them running.
#[tokio::test]
async fn turning_off_removes_every_container_before_stopping_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = Arc::new(FakeDaemon {
        listed: vec!["web"],
        ..FakeDaemon::default()
    });
    let r = ContainerReconciler::new(
        MockUnitControl::new("active", "enabled"),
        daemon.clone(),
        Arc::new(FakeEngine {
            names: vec!["web"],
            polls: Mutex::new(3),
        }),
        dir.path().to_path_buf(),
    )
    .with_waits(
        Duration::from_millis(200),
        Duration::from_millis(500),
        Duration::from_millis(5),
    );

    let state = r.apply(&settings(false)).await.unwrap();

    assert_eq!(
        daemon.calls.lock().unwrap().clone(),
        ["status", "containers", "declare "]
    );
    assert_eq!(
        r.control.calls(),
        [
            format!("stop {SERVICE_UNIT}"),
            format!("disable {SERVICE_UNIT}")
        ]
    );
    assert_eq!(state["removedContainers"], json!(["web"]));
    assert_eq!(state["containers"], Value::Null);
}

/// A container that does not go keeps the daemon running, and says so.
#[tokio::test]
async fn a_container_that_does_not_go_keeps_the_daemon_running() {
    let dir = tempfile::tempdir().unwrap();
    let daemon = FakeDaemon {
        listed: vec!["web"],
        ..FakeDaemon::default()
    };
    let r = reconciler("active", "enabled", daemon, usize::MAX, dir.path().into());
    let err = r.apply(&settings(false)).await.unwrap_err().to_string();
    assert!(err.contains("did not remove"), "{err}");
    assert!(
        !r.control.calls().iter().any(|c| c.starts_with("stop")),
        "{:?}",
        r.control.calls()
    );
}

/// Off and already off: nothing is touched.
#[tokio::test]
async fn off_and_already_off_touches_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let r = reconciler(
        "inactive",
        "disabled",
        FakeDaemon::default(),
        0,
        dir.path().into(),
    );
    r.apply(&settings(false)).await.unwrap();
    assert!(r.control.calls().is_empty());
}

/// The Quadlet files named as micad's go; an integrator's stay.
#[tokio::test]
async fn micads_quadlet_files_are_removed_and_an_integrators_kept() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("50-mica-web.container"), "x").unwrap();
    std::fs::write(dir.path().join("vendor.container"), "x").unwrap();
    let r = reconciler(
        "active",
        "enabled",
        FakeDaemon::default(),
        0,
        dir.path().into(),
    );
    let state = r.apply(&settings(true)).await.unwrap();
    assert_eq!(
        state["removedQuadletFiles"],
        json!(["50-mica-web.container"])
    );
    assert!(!dir.path().join("50-mica-web.container").exists());
    assert!(dir.path().join("vendor.container").exists());
}

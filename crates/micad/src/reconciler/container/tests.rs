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
    /// The containers of the projects `GET` and `PUT /v1/projects` answer.
    project_containers: Vec<&'static str>,
    refuse: bool,
}

impl FakeDaemon {
    fn projects_answer(&self) -> Value {
        json!({ "projects": [{
            "name": "app",
            "services": self.project_containers.iter().map(|n| json!({ "spec": { "name": n }, "phase": "running" })).collect::<Vec<_>>(),
        }] })
    }
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

    async fn act(&self, name: &str, verb: Verb) -> Result<Value, ContainerdError> {
        self.calls.lock().unwrap().push(format!("{verb:?} {name}"));
        Ok(json!({}))
    }

    async fn declare_projects(
        &self,
        container: &ContainerSettings,
    ) -> Result<Value, ContainerdError> {
        let names: Vec<&str> = container.projects.keys().map(String::as_str).collect();
        self.calls
            .lock()
            .unwrap()
            .push(format!("projects {}", names.join(",")));
        if self.refuse {
            return Err(ContainerdError::Refused {
                status: 400,
                rule: "compose".into(),
                detail: "web: privileged is not supported".into(),
            });
        }
        Ok(self.projects_answer())
    }

    async fn projects(&self) -> Result<Value, ContainerdError> {
        self.calls.lock().unwrap().push("list projects".into());
        Ok(self.projects_answer())
    }

    async fn logs(&self, _name: &str, _tail: u32) -> Result<String, ContainerdError> {
        unreachable!("the reconciler reads no log")
    }
}

/// An engine whose `podman ps` lists `names` running for the first `polls`
/// reads, and then exited.
struct FakeEngine {
    names: Vec<&'static str>,
    polls: Mutex<usize>,
}

impl FakeEngine {
    fn new(names: Vec<&'static str>, polls: usize) -> Self {
        Self {
            names,
            polls: Mutex::new(polls),
        }
    }
}

#[async_trait::async_trait]
impl ContainerEngine for FakeEngine {
    async fn containers(&self) -> Value {
        let mut polls = self.polls.lock().unwrap();
        let state = if *polls > 0 {
            *polls -= 1;
            "running"
        } else {
            "exited"
        };
        let names: Vec<Value> = self
            .names
            .iter()
            .map(|n| json!({ "Names": [n], "State": state }))
            .collect();
        json!({ "available": true, "entries": names })
    }
}

fn reconciler(
    active: &str,
    file: &str,
    daemon: Arc<FakeDaemon>,
    engine: FakeEngine,
) -> ContainerReconciler<MockUnitControl> {
    ContainerReconciler::new(MockUnitControl::new(active, file), daemon, Arc::new(engine))
        .with_waits(
            Duration::from_millis(200),
            Duration::from_millis(500),
            Duration::from_millis(5),
        )
}

fn settings(enabled: bool) -> Settings {
    let mut settings = Settings::default();
    settings.container.enabled = enabled;
    settings.container.projects.insert(
        "app".into(),
        "services:\n  web:\n    image: nginx:1\n".into(),
    );
    settings
}

/// On: the service enabled and started, then the projects as one set, and
/// every container started, which a switch turned off left stopped.
#[tokio::test]
async fn projects_are_declared_and_started_by_the_pass_that_starts_the_service() {
    let daemon = Arc::new(FakeDaemon {
        silent: Mutex::new(2),
        project_containers: vec!["app-web-1", "app-db-1"],
        ..FakeDaemon::default()
    });
    let r = reconciler(
        "inactive",
        "disabled",
        daemon.clone(),
        FakeEngine::new(vec![], 0),
    );

    let state = r.apply(&settings(true)).await.unwrap();

    assert_eq!(
        r.control.calls(),
        [
            format!("enable {SERVICE_UNIT}"),
            format!("start {SERVICE_UNIT}")
        ]
    );
    assert_eq!(
        daemon.calls.lock().unwrap().clone(),
        [
            "status",
            "status",
            "status",
            "projects app",
            "Start app-db-1",
            "Start app-web-1"
        ]
    );
    assert_eq!(state["declaredProjects"], json!(["app"]));
    assert_eq!(state["projects"][0]["name"], json!("app"));
    assert_eq!(state["startedContainers"], json!(["app-db-1", "app-web-1"]));
}

/// A running service is only declared to: a container a person stopped stays
/// stopped across a settings change.
#[tokio::test]
async fn a_running_service_is_declared_to_and_nothing_started() {
    let daemon = Arc::new(FakeDaemon {
        project_containers: vec!["app-web-1"],
        ..FakeDaemon::default()
    });
    let r = reconciler(
        "active",
        "enabled",
        daemon.clone(),
        FakeEngine::new(vec![], 0),
    );
    let state = r.apply(&settings(true)).await.unwrap();
    assert!(r.control.calls().is_empty(), "{:?}", r.control.calls());
    assert_eq!(
        daemon.calls.lock().unwrap().clone(),
        ["status", "projects app"]
    );
    assert_eq!(state["startedContainers"], json!([]));
}

/// A daemon that never answers is an error naming it, not an empty success.
#[tokio::test]
async fn a_daemon_that_never_answers_is_an_error() {
    let daemon = Arc::new(FakeDaemon {
        silent: Mutex::new(usize::MAX),
        ..FakeDaemon::default()
    });
    let r = reconciler("active", "enabled", daemon, FakeEngine::new(vec![], 0));
    let err = r.apply(&settings(true)).await.unwrap_err().to_string();
    assert!(err.contains("did not answer"), "{err}");
}

/// A refusal of the declaration is the pass's error, with the daemon's rule.
#[tokio::test]
async fn a_refused_declaration_is_the_passes_error() {
    let daemon = Arc::new(FakeDaemon {
        refuse: true,
        ..FakeDaemon::default()
    });
    let r = reconciler("active", "enabled", daemon, FakeEngine::new(vec![], 0));
    let err = format!("{:#}", r.apply(&settings(true)).await.unwrap_err());
    assert!(err.contains("refused `compose`"), "{err}");
}

/// Off: every project's container stopped, and still declared, and only once
/// none runs is the service stopped and disabled -- a daemon stopped first
/// leaves them running.
#[tokio::test]
async fn turning_off_stops_every_project_container_before_the_daemon() {
    let daemon = Arc::new(FakeDaemon {
        project_containers: vec!["app-web-1"],
        ..FakeDaemon::default()
    });
    let r = reconciler(
        "active",
        "enabled",
        daemon.clone(),
        FakeEngine::new(vec!["app-web-1"], 3),
    );

    let state = r.apply(&settings(false)).await.unwrap();

    assert_eq!(
        daemon.calls.lock().unwrap().clone(),
        ["status", "list projects", "Stop app-web-1"]
    );
    assert_eq!(
        r.control.calls(),
        [
            format!("stop {SERVICE_UNIT}"),
            format!("disable {SERVICE_UNIT}")
        ]
    );
    assert_eq!(state["stoppedContainers"], json!(["app-web-1"]));
    assert_eq!(state["projects"], Value::Null);
}

/// A project container that keeps running keeps the daemon running, and says so.
#[tokio::test]
async fn a_project_container_that_keeps_running_keeps_the_daemon() {
    let daemon = Arc::new(FakeDaemon {
        project_containers: vec!["app-web-1"],
        ..FakeDaemon::default()
    });
    let r = reconciler(
        "active",
        "enabled",
        daemon,
        FakeEngine::new(vec!["app-web-1"], usize::MAX),
    );
    let err = r.apply(&settings(false)).await.unwrap_err().to_string();
    assert!(err.contains("did not stop"), "{err}");
    assert!(!r.control.calls().iter().any(|c| c.starts_with("stop")));
}

/// Off and already off: nothing is touched.
#[tokio::test]
async fn off_and_already_off_touches_nothing() {
    let daemon = Arc::new(FakeDaemon::default());
    let r = reconciler(
        "inactive",
        "disabled",
        daemon.clone(),
        FakeEngine::new(vec![], 0),
    );
    r.apply(&settings(false)).await.unwrap();
    assert!(r.control.calls().is_empty());
    assert!(daemon.calls.lock().unwrap().is_empty());
}

/// On docker only dockerd's service is switched: on, it is enabled, started
/// and waited for, and nothing is declared or started; off, it is stopped and
/// disabled with no container stopped first, since dockerd stops its own.
#[tokio::test]
async fn on_docker_only_the_service_is_switched() {
    let daemon = Arc::new(FakeDaemon {
        project_containers: vec!["app-web-1"],
        ..FakeDaemon::default()
    });
    let r = reconciler(
        "inactive",
        "disabled",
        daemon.clone(),
        FakeEngine::new(vec![], 0),
    )
    .service_only(crate::docker::SERVICE_UNIT, Duration::from_millis(200));
    let state = r.apply(&settings(true)).await.unwrap();
    assert_eq!(
        r.control.calls(),
        ["enable docker.service", "start docker.service"]
    );
    assert_eq!(daemon.calls.lock().unwrap().clone(), ["status"]);
    assert_eq!(state["service"], json!("docker.service"));
    assert_eq!(state["startedContainers"], json!([]));

    let daemon = Arc::new(FakeDaemon {
        project_containers: vec!["app-web-1"],
        ..FakeDaemon::default()
    });
    let r = reconciler(
        "active",
        "enabled",
        daemon.clone(),
        FakeEngine::new(vec!["app-web-1"], 1000),
    )
    .service_only(crate::docker::SERVICE_UNIT, Duration::from_millis(200));
    let state = r.apply(&settings(false)).await.unwrap();
    assert_eq!(
        r.control.calls(),
        ["stop docker.service", "disable docker.service"]
    );
    assert!(daemon.calls.lock().unwrap().is_empty());
    assert_eq!(state["stoppedContainers"], json!([]));

    // A dockerd that never answers fails the pass, naming its service.
    let never = Arc::new(FakeDaemon {
        silent: Mutex::new(usize::MAX),
        ..FakeDaemon::default()
    });
    let r = reconciler("inactive", "disabled", never, FakeEngine::new(vec![], 0))
        .service_only(crate::docker::SERVICE_UNIT, Duration::from_millis(50));
    let err = r.apply(&settings(true)).await.unwrap_err().to_string();
    assert!(
        err.starts_with("docker.service did not answer within"),
        "{err}"
    );
}

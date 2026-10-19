//! The Docker Engine, on a product whose `CONTAINER_ENGINE` is docker.
//!
//! There is no mica-containerd on such a product, and micad is a client of
//! dockerd's API on `/run/docker.sock` and nothing more: it lists and inspects
//! every container dockerd has, starts, stops and restarts them, and reads
//! their logs and the images. Containers are made with `docker` and `docker
//! compose` themselves; restart, boot start and health are dockerd's. A
//! container is answered in mica-containerd's shape so the bus, the API, the
//! console and `micad ctr` read one answer on either engine: a compose
//! container under its project and service, from the labels compose puts on
//! it, and any other container as a project of its own name. Its phase is the
//! state dockerd reports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use micad_settings::ContainerSettings;
use serde_json::{Value, json};

use crate::containerd::{Client, Containerd, ContainerdError, Verb, split_response};
use crate::containers::ContainerEngine;

/// dockerd's socket.
pub const SOCKET_PATH: &str = "/run/docker.sock";
/// The service that runs dockerd: a systemd unit, or the OpenRC script of the
/// same name. It pulls in containerd itself.
pub const SERVICE_UNIT: &str = "docker.service";
/// What a write of the projects, and `micad ctr up`, `down` and `check`, are
/// told on a docker product.
pub const USE_DOCKER_COMPOSE: &str = "this is a docker product: containers are made with docker and docker compose, not declared to micad";
/// The API version every call names: the oldest Docker 29 serves, so a newer
/// dockerd answers in the shapes read here.
const API: &str = "/v1.44";

/// The labels docker compose puts on a project's containers.
const PROJECT_LABEL: &str = "com.docker.compose.project";
const SERVICE_LABEL: &str = "com.docker.compose.service";
const DIR_LABEL: &str = "com.docker.compose.project.working_dir";

/// How long a stopping container gets before dockerd kills it, as `docker
/// stop` gives it.
const STOP_GRACE_SECONDS: u32 = 10;

/// dockerd, through its API.
pub struct Docker {
    api: Client,
}

impl Docker {
    /// The installed engine.
    #[must_use]
    pub fn production() -> Self {
        Self::at(SOCKET_PATH)
    }

    /// An engine on `socket`: the tests' fake.
    #[must_use]
    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self {
            api: Client::with_host(socket, "docker"),
        }
    }

    /// One call; its status and body.
    async fn exchange(&self, method: &str, path: &str) -> Result<(u16, Vec<u8>), ContainerdError> {
        let raw = self.api.raw(method, &format!("{API}{path}"), None).await;
        split_response(&raw.map_err(engine_error)?).map_err(engine_error)
    }

    /// One call; its JSON answer, `Null` when it has no body.
    async fn call(&self, method: &str, path: &str) -> Result<Value, ContainerdError> {
        let (status, body) = self.exchange(method, path).await?;
        answer(status, &body)
    }

    /// Every container dockerd has, inspected: one call for the list and one
    /// for each, for its restart count, exit code and health.
    async fn inspected(&self) -> Result<Vec<Value>, ContainerdError> {
        let listed = self.call("GET", "/containers/json?all=1").await?;
        let mut inspected = Vec::new();
        for id in listed
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry["Id"].as_str())
        {
            inspected.push(self.call("GET", &format!("/containers/{id}/json")).await?);
        }
        Ok(inspected)
    }
}

/// A transport failure, said as dockerd's.
fn engine_error(err: ContainerdError) -> ContainerdError {
    match err {
        ContainerdError::Unavailable(detail) => {
            ContainerdError::Docker(format!("not answering: {detail}"))
        }
        other => other,
    }
}

/// The JSON of one answer, or dockerd's refusal (`{"message"}`). A 304, a
/// container already in the state asked for, is done.
fn answer(status: u16, body: &[u8]) -> Result<Value, ContainerdError> {
    let value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(body)
            .map_err(|_| ContainerdError::Docker("not an API answer: body is not JSON".into()))?
    };
    if (200..300).contains(&status) || status == 304 {
        return Ok(value);
    }
    Err(ContainerdError::Docker(format!(
        "refused ({status}): {}",
        value["message"].as_str().unwrap_or("no reason given")
    )))
}

/// One inspected container as a mica-containerd container entry, with its
/// project and that project's directory: compose's, or its own name and none.
fn entry(inspected: &Value) -> (String, String, Value) {
    let labels = &inspected["Config"]["Labels"];
    let name = inspected["Name"]
        .as_str()
        .unwrap_or_default()
        .trim_start_matches('/')
        .to_string();
    let (project, service) = match (
        labels[PROJECT_LABEL].as_str(),
        labels[SERVICE_LABEL].as_str(),
    ) {
        (Some(project), Some(service)) => (project.to_string(), service.to_string()),
        _ => (name.clone(), name.clone()),
    };
    let dir = labels[DIR_LABEL].as_str().unwrap_or_default().to_string();
    let state = &inspected["State"];
    let entry = json!({
        "project": project,
        "service": service,
        "spec": { "name": name, "image": inspected["Config"]["Image"] },
        "phase": state["Status"],
        "health": state["Health"]["Status"].as_str().unwrap_or_default(),
        "restarts": inspected["RestartCount"],
        "exit_code": state["ExitCode"],
    });
    (project, dir, entry)
}

/// Inspected containers as `GET /v1/projects` answers them.
fn project_answer(inspected: &[Value]) -> Value {
    let mut projects: BTreeMap<String, (String, Vec<Value>)> = BTreeMap::new();
    for (project, dir, entry) in inspected.iter().map(entry) {
        let slot = projects.entry(project).or_insert_with(|| (dir, Vec::new()));
        slot.1.push(entry);
    }
    let projects: Vec<Value> = projects
        .into_iter()
        .map(|(name, (dir, mut services))| {
            services.sort_by(|a, b| {
                a["service"]
                    .as_str()
                    .cmp(&b["service"].as_str())
                    .then(a["spec"]["name"].as_str().cmp(&b["spec"]["name"].as_str()))
            });
            json!({ "name": name, "projectDir": dir, "services": services })
        })
        .collect();
    json!({ "projects": projects })
}

/// A log as dockerd sends it: multiplexed into frames of an 8-byte header and
/// a payload when the container has no terminal, raw when it has one.
fn demultiplex(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut rest = body;
    while rest.len() >= 8 {
        let size = u32::from_be_bytes([rest[4], rest[5], rest[6], rest[7]]) as usize;
        let Some(payload) = rest.get(8..8 + size) else {
            break;
        };
        out.extend_from_slice(payload);
        rest = &rest[8 + size..];
    }
    out
}

/// Whether an answer's headers name a multiplexed stream.
fn multiplexed(response: &[u8]) -> bool {
    let end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap_or(response.len());
    String::from_utf8_lossy(&response[..end])
        .split("\r\n")
        .any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("content-type")
                    && value.contains("application/vnd.docker.multiplexed-stream")
            })
        })
}

#[async_trait::async_trait]
impl Containerd for Docker {
    /// `GET /_ping`: whether dockerd answers.
    async fn status(&self) -> Result<Value, ContainerdError> {
        let (status, body) = self.exchange("GET", "/_ping").await?;
        if status == 200 {
            return Ok(json!({ "ping": String::from_utf8_lossy(&body).trim() }));
        }
        answer(status, &body)
    }

    async fn act(&self, name: &str, verb: Verb) -> Result<Value, ContainerdError> {
        // A container's name, as dockerd's own list gives it.
        let path = match verb {
            Verb::Start => format!("/containers/{name}/start"),
            Verb::Stop => format!("/containers/{name}/stop?t={STOP_GRACE_SECONDS}"),
            Verb::Restart => format!("/containers/{name}/restart?t={STOP_GRACE_SECONDS}"),
        };
        self.call("POST", &path).await
    }

    /// Nothing is declared to dockerd.
    async fn declare_projects(
        &self,
        _container: &ContainerSettings,
    ) -> Result<Value, ContainerdError> {
        Err(ContainerdError::Docker(USE_DOCKER_COMPOSE.into()))
    }

    async fn projects(&self) -> Result<Value, ContainerdError> {
        Ok(project_answer(&self.inspected().await?))
    }

    async fn logs(&self, name: &str, tail: u32) -> Result<String, ContainerdError> {
        let raw = self
            .api
            .raw(
                "GET",
                &format!("{API}/containers/{name}/logs?stdout=1&stderr=1&tail={tail}"),
                None,
            )
            .await
            .map_err(engine_error)?;
        let (status, body) = split_response(&raw).map_err(engine_error)?;
        if !(200..300).contains(&status) {
            return answer(status, &body).map(|_| String::new());
        }
        let text = if multiplexed(&raw) {
            demultiplex(&body)
        } else {
            body
        };
        Ok(String::from_utf8_lossy(&text).into_owned())
    }
}

#[async_trait::async_trait]
impl ContainerEngine for Docker {
    async fn images(&self) -> Value {
        match self.call("GET", "/images/json").await {
            Ok(listed) => {
                let entries: Vec<Value> = listed
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|image| {
                        json!({
                            "Id": image["Id"],
                            "Names": image["RepoTags"],
                            "Size": image["Size"],
                            "Created": image["Created"],
                        })
                    })
                    .collect();
                json!({ "available": true, "entries": entries })
            }
            Err(err) => json!({ "available": false, "detail": err.to_string() }),
        }
    }
}

/// Whether the dockerd named by `pid_file` is running.
#[must_use]
pub fn running(pid_file: &Path) -> bool {
    std::fs::read_to_string(pid_file)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok())
        .is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists())
}

#[cfg(test)]
mod tests;

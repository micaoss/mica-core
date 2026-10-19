//! mica-containerd's API, over its UNIX socket.
//!
//! mica-podman's supervisor owns every container: it stores the declarations
//! on STATE, starts, restarts and health-checks them, and brings them back at
//! boot on its own. micad declares the settings' Compose projects and asks it to start,
//! stop and restart; it runs no container itself. The contract is mica-podman's
//! README, section *mica-containerd*.
//!
//! HTTP/1.1 over `/run/mica-containerd/api.sock`, one request per connection,
//! JSON both ways, bounded in time and in size.

use std::path::PathBuf;
use std::time::Duration;

use micad_settings::ContainerSettings;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The daemon's socket, 0600 root.
pub const SOCKET_PATH: &str = "/run/mica-containerd/api.sock";
/// The service that runs the daemon: a systemd unit, or the OpenRC script of
/// the same name.
pub const SERVICE_UNIT: &str = "mica-containerd.service";

/// How long a stopping container gets before the daemon kills it: its
/// `--stop-timeout`, 10 s unless the service sets another.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// How long one call may take. A declaration is stored and acted on by the
/// daemon's own loop, but the loop holds every call while it stops a
/// container, so a call can wait out [`STOP_GRACE`], the kill and podman's
/// cleanup. The grace and 10 s more outlast that and still answer within a
/// D-Bus caller's default 25 s.
const CALL_TIMEOUT: Duration = Duration::from_secs(STOP_GRACE.as_secs() + 10);
/// The largest answer read.
const MAX_RESPONSE: usize = 1024 * 1024;

/// Why a call did not do what it asked.
#[derive(Debug, thiserror::Error)]
pub enum ContainerdError {
    /// Nothing answered, or what answered was not the API.
    #[error("mica-containerd is not answering: {0}")]
    Unavailable(String),
    /// The daemon refused, naming its rule.
    #[error("mica-containerd refused `{rule}`: {detail}")]
    Refused {
        status: u16,
        rule: String,
        detail: String,
    },
    /// dockerd did not answer, refused, or docker compose failed
    /// (`crate::docker`).
    #[error("dockerd: {0}")]
    Docker(String),
}

/// A verb on one container, persisted by the daemon as its desired state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Start,
    Stop,
    Restart,
}

impl Verb {
    fn path(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// What micad asks of the daemon.
#[async_trait::async_trait]
pub trait Containerd: Send + Sync {
    /// `GET /v1/status`: whether the daemon answers.
    async fn status(&self) -> Result<Value, ContainerdError>;
    /// `POST /v1/containers/{name}/{verb}`.
    async fn act(&self, name: &str, verb: Verb) -> Result<Value, ContainerdError>;
    /// `PUT /v1/projects`: declare exactly `container`'s projects, each
    /// with its project directory when it has one; every other project goes.
    async fn declare_projects(
        &self,
        container: &ContainerSettings,
    ) -> Result<Value, ContainerdError>;
    /// `GET /v1/projects`: every project with its containers' observed state.
    async fn projects(&self) -> Result<Value, ContainerdError>;
    /// `GET /v1/containers/{name}/logs?tail=`: the last `tail` lines, as text.
    async fn logs(&self, name: &str, tail: u32) -> Result<String, ContainerdError>;
}

/// The daemon on its socket.
pub struct Client {
    socket: PathBuf,
    host: &'static str,
}

impl Client {
    /// The installed daemon.
    #[must_use]
    pub fn production() -> Self {
        Self::at(SOCKET_PATH)
    }

    /// A daemon on `socket`: the tests' fake server.
    #[must_use]
    pub fn at(socket: impl Into<PathBuf>) -> Self {
        Self::with_host(socket, "mica-containerd")
    }

    /// Another HTTP API on `socket`, named `host` in its requests: dockerd's.
    #[must_use]
    pub(crate) fn with_host(socket: impl Into<PathBuf>, host: &'static str) -> Self {
        Self {
            socket: socket.into(),
            host,
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ContainerdError> {
        parse_response(&self.raw(method, path, body).await?)
    }

    /// One call, its answer's bytes as read.
    pub(crate) async fn raw(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Vec<u8>, ContainerdError> {
        tokio::time::timeout(CALL_TIMEOUT, self.exchange(method, path, body))
            .await
            .map_err(|_| {
                ContainerdError::Unavailable(format!(
                    "{method} {path} did not answer within {} seconds",
                    CALL_TIMEOUT.as_secs()
                ))
            })?
    }

    async fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Vec<u8>, ContainerdError> {
        let unavailable = |what: &str, err: std::io::Error| {
            ContainerdError::Unavailable(format!("{what} {}: {err}", self.socket.display()))
        };
        let mut stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .map_err(|err| unavailable("connect to", err))?;
        let payload = body.map(Value::to_string).unwrap_or_default();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nAccept: application/json\r\n",
            self.host
        );
        if body.is_some() {
            request.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                payload.len()
            ));
        }
        request.push_str("\r\n");
        request.push_str(&payload);
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|err| unavailable("write to", err))?;
        let mut response = Vec::new();
        (&mut stream)
            .take(MAX_RESPONSE as u64 + 1)
            .read_to_end(&mut response)
            .await
            .map_err(|err| unavailable("read from", err))?;
        if response.len() > MAX_RESPONSE {
            return Err(ContainerdError::Unavailable(format!(
                "{method} {path} answered more than {MAX_RESPONSE} bytes"
            )));
        }
        Ok(response)
    }
}

/// The JSON body of one HTTP/1.1 response, or the refusal it carries.
pub fn parse_response(response: &[u8]) -> Result<Value, ContainerdError> {
    let (status, body) = split_response(response)?;
    let value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(&body).map_err(|_| {
            ContainerdError::Unavailable("not an API answer: body is not JSON".into())
        })?
    };
    if (200..300).contains(&status) {
        return Ok(value);
    }
    Err(refusal(status, &value))
}

/// The text body of one HTTP/1.1 response (a log), or the refusal it carries.
pub fn parse_text_response(response: &[u8]) -> Result<String, ContainerdError> {
    let (status, body) = split_response(response)?;
    if (200..300).contains(&status) {
        return Ok(String::from_utf8_lossy(&body).into_owned());
    }
    Err(refusal(
        status,
        &serde_json::from_slice(&body).unwrap_or(Value::Null),
    ))
}

/// A non-2xx answer, as the rule and detail it names.
fn refusal(status: u16, value: &Value) -> ContainerdError {
    ContainerdError::Refused {
        status,
        rule: value["error"].as_str().unwrap_or("unknown").to_string(),
        detail: match &value["detail"] {
            Value::String(detail) => detail.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        },
    }
}

/// The status and the (dechunked) body of one HTTP/1.1 response.
pub(crate) fn split_response(response: &[u8]) -> Result<(u16, Vec<u8>), ContainerdError> {
    let malformed = |what: &str| ContainerdError::Unavailable(format!("not an API answer: {what}"));
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| malformed("no end of headers"))?;
    let head = std::str::from_utf8(&response[..split]).map_err(|_| malformed("headers"))?;
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.strip_prefix("HTTP/1.1 "))
        .and_then(|rest| rest.get(..3))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| malformed("status line"))?;
    let chunked = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked")
        })
    });
    let raw = &response[split + 4..];
    let body = if chunked {
        dechunk(raw).ok_or_else(|| malformed("chunked body"))?
    } else {
        raw.to_vec()
    };
    Ok((status, body))
}

/// A chunked body, reassembled.
fn dechunk(mut raw: &[u8]) -> Option<Vec<u8>> {
    let mut body = Vec::new();
    loop {
        let end = raw.windows(2).position(|window| window == b"\r\n")?;
        let size_text = std::str::from_utf8(&raw[..end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        raw = &raw[end + 2..];
        if size == 0 {
            return Some(body);
        }
        body.extend_from_slice(raw.get(..size)?);
        raw = raw.get(size + 2..)?;
    }
}

#[async_trait::async_trait]
impl Containerd for Client {
    async fn status(&self) -> Result<Value, ContainerdError> {
        self.call("GET", "/v1/status", None).await
    }

    async fn act(&self, name: &str, verb: Verb) -> Result<Value, ContainerdError> {
        // A container is `<project>-<service>-1`, of letters, digits, `-` and
        // `_`: nothing in one needs escaping.
        self.call(
            "POST",
            &format!("/v1/containers/{name}/{}", verb.path()),
            None,
        )
        .await
    }

    async fn declare_projects(
        &self,
        container: &ContainerSettings,
    ) -> Result<Value, ContainerdError> {
        self.call("PUT", "/v1/projects", Some(&project_set(container)))
            .await
    }

    async fn projects(&self) -> Result<Value, ContainerdError> {
        self.call("GET", "/v1/projects", None).await
    }

    async fn logs(&self, name: &str, tail: u32) -> Result<String, ContainerdError> {
        // A container is `<project>-<service>-1`, of letters, digits, `-` and
        // `_`: nothing in one needs escaping.
        let raw = self
            .raw(
                "GET",
                &format!("/v1/containers/{name}/logs?tail={tail}"),
                None,
            )
            .await?;
        parse_text_response(&raw)
    }
}

/// The body of `PUT /v1/projects`: each project its Compose text, or
/// `{"compose", "projectDir"}` when it has a project directory.
fn project_set(container: &ContainerSettings) -> Value {
    let projects: serde_json::Map<String, Value> = container
        .projects
        .iter()
        .map(|(name, text)| {
            let entry = match container.project_dirs.get(name) {
                Some(dir) => json!({ "compose": text, "projectDir": dir }),
                None => json!(text),
            };
            (name.clone(), entry)
        })
        .collect();
    json!({ "projects": projects })
}

/// A daemon that is not there, for the dry run and for tests that ask for none.
pub struct NoContainerd;

#[async_trait::async_trait]
impl Containerd for NoContainerd {
    async fn status(&self) -> Result<Value, ContainerdError> {
        Err(ContainerdError::Unavailable(
            "this build drives no mica-containerd".into(),
        ))
    }

    async fn act(&self, _name: &str, _verb: Verb) -> Result<Value, ContainerdError> {
        self.status().await
    }

    async fn declare_projects(
        &self,
        _container: &ContainerSettings,
    ) -> Result<Value, ContainerdError> {
        self.status().await
    }

    async fn projects(&self) -> Result<Value, ContainerdError> {
        self.status().await
    }

    async fn logs(&self, _name: &str, _tail: u32) -> Result<String, ContainerdError> {
        self.status().await.map(|_| String::new())
    }
}

#[cfg(test)]
mod tests;

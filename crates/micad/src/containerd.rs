//! mica-containerd's API, over its UNIX socket.
//!
//! mica-podman's supervisor owns every container: it stores the declarations
//! on STATE, starts, restarts and health-checks them, and brings them back at
//! boot on its own. micad declares what the settings say and asks it to start,
//! stop and restart; it runs no container itself. The contract is mica-podman's
//! README, section *mica-containerd*.
//!
//! HTTP/1.1 over `/run/mica-containerd/api.sock`, one request per connection,
//! JSON both ways, bounded in time and in size.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use micad_settings::ContainerUnit;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The daemon's socket, 0600 root.
pub const SOCKET_PATH: &str = "/run/mica-containerd/api.sock";
/// The service that runs the daemon: a systemd unit, or the OpenRC script of
/// the same name.
pub const SERVICE_UNIT: &str = "mica-containerd.service";

/// How long one call may take. A declaration is stored and acted on by the
/// daemon's own loop, so even a `PUT` of every container returns at once.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
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
    /// `PUT /v1/containers`: declare exactly `units`; the rest are removed.
    async fn declare(
        &self,
        units: &BTreeMap<String, ContainerUnit>,
    ) -> Result<Value, ContainerdError>;
    /// `GET /v1/containers`: every declaration with its observed state.
    async fn containers(&self) -> Result<Value, ContainerdError>;
    /// `POST /v1/containers/{name}/{verb}`.
    async fn act(&self, name: &str, verb: Verb) -> Result<Value, ContainerdError>;
}

/// The spec the API takes for a declared unit, field for field and nothing
/// more: the daemon refuses a field it does not know.
#[must_use]
pub fn spec(name: &str, unit: &ContainerUnit) -> Value {
    let mut limits = serde_json::Map::new();
    if let Some(pids) = unit.pids {
        limits.insert("pids".into(), json!(pids));
    }
    if let Some(memory) = &unit.memory {
        limits.insert("memory".into(), json!(memory));
    }
    if let Some(cpu) = &unit.cpu {
        limits.insert("cpu".into(), json!(cpu));
    }
    json!({
        "name": name,
        "image": unit.image,
        "command": unit.command,
        "environment": unit.environment,
        "publish": unit.publish.iter().map(|port| json!({
            "host": port.host,
            "container": port.container,
            "protocol": port.protocol.as_str(),
        })).collect::<Vec<_>>(),
        "volumes": unit.volumes.iter().map(|volume| json!({
            "host": volume.host,
            "container": volume.container,
            "read_only": volume.read_only,
        })).collect::<Vec<_>>(),
        "limits": limits,
        "restart": { "policy": unit.restart.as_str() },
        "autostart": unit.auto_start,
    })
}

/// The daemon on its socket.
pub struct Client {
    socket: PathBuf,
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
        Self {
            socket: socket.into(),
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ContainerdError> {
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
    ) -> Result<Value, ContainerdError> {
        let unavailable = |what: &str, err: std::io::Error| {
            ContainerdError::Unavailable(format!("{what} {}: {err}", self.socket.display()))
        };
        let mut stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .map_err(|err| unavailable("connect to", err))?;
        let payload = body.map(Value::to_string).unwrap_or_default();
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: mica-containerd\r\nConnection: close\r\nAccept: application/json\r\n"
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
        parse_response(&response)
    }
}

/// The JSON body of one HTTP/1.1 response, or the refusal it carries.
pub fn parse_response(response: &[u8]) -> Result<Value, ContainerdError> {
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
    let value = if body.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(&body).map_err(|_| malformed("body is not JSON"))?
    };
    if (200..300).contains(&status) {
        return Ok(value);
    }
    Err(ContainerdError::Refused {
        status,
        rule: value["error"].as_str().unwrap_or("unknown").to_string(),
        detail: match &value["detail"] {
            Value::String(detail) => detail.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        },
    })
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

    async fn declare(
        &self,
        units: &BTreeMap<String, ContainerUnit>,
    ) -> Result<Value, ContainerdError> {
        let specs: Vec<Value> = units.iter().map(|(name, unit)| spec(name, unit)).collect();
        self.call(
            "PUT",
            "/v1/containers",
            Some(&json!({ "containers": specs })),
        )
        .await
    }

    async fn containers(&self) -> Result<Value, ContainerdError> {
        self.call("GET", "/v1/containers", None).await
    }

    async fn act(&self, name: &str, verb: Verb) -> Result<Value, ContainerdError> {
        // A declared name is letters, digits, `-` and `_`
        // (`validate_container_units`): nothing in one needs escaping.
        self.call(
            "POST",
            &format!("/v1/containers/{name}/{}", verb.path()),
            None,
        )
        .await
    }
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

    async fn declare(
        &self,
        _units: &BTreeMap<String, ContainerUnit>,
    ) -> Result<Value, ContainerdError> {
        self.status().await
    }

    async fn containers(&self) -> Result<Value, ContainerdError> {
        self.status().await
    }

    async fn act(&self, _name: &str, _verb: Verb) -> Result<Value, ContainerdError> {
        self.status().await
    }
}

#[cfg(test)]
mod tests;

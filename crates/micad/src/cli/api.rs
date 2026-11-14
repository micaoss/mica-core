//! apid's API on its local socket, for the verbs whose rules are apid's.
//!
//! A call here is an API call: apid checks it, audits it (source `local`)
//! and counts a wrong password in its login backoff, as for any client. The
//! socket is root's; apid takes the caller's uid from the kernel and asks for
//! no credential.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};

use super::{EXIT_CREDENTIAL, EXIT_FAILED, EXIT_UNREACHABLE, Failure};

/// Bound on each read or write; apid bounds its own calls to micad below it.
const TIMEOUT: Duration = Duration::from_secs(120);
/// The largest answer read.
const MAX_ANSWER: u64 = 16 << 20;
/// How often a task is read while micad applies it.
const POLL: Duration = Duration::from_secs(1);
/// How long a task is waited for.
const TASK_TIMEOUT: Duration = Duration::from_secs(120);

/// The socket apid serves the API on, as apid resolves it.
fn socket() -> PathBuf {
    PathBuf::from(
        std::env::var("APID_LOCAL_SOCKET")
            .unwrap_or_else(|_| micad_settings::APID_LOCAL_SOCKET_PATH.to_string()),
    )
}

/// One answer: its status and its JSON body (`null` when it has none).
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub body: Value,
}

/// `GET` `path` (under `/api`), the answer's body on a 2xx.
pub fn get(path: &str) -> Result<Value> {
    ok(call("GET", path, None)?)
}

/// `method` `path` with `body`, the answer's body on a 2xx.
pub fn send(method: &str, path: &str, body: &Value) -> Result<Value> {
    ok(call(method, path, Some(body))?)
}

/// `DELETE` `path`.
pub fn delete(path: &str) -> Result<()> {
    ok(call("DELETE", path, None)?).map(drop)
}

/// The body of a 2xx answer; any other is the API's refusal as a [`Failure`].
pub fn ok(answer: Answer) -> Result<Value> {
    if (200..300).contains(&answer.status) {
        return Ok(answer.body);
    }
    Err(refusal(&answer).into())
}

/// The API's refusal, with the exit code its code and status mean.
fn refusal(answer: &Answer) -> Failure {
    let error = &answer.body["error"];
    let code = error["code"].as_str().unwrap_or("failed").to_string();
    let mut message = error["message"].as_str().map_or_else(
        || format!("apid answered {}", answer.status),
        str::to_string,
    );
    if let Some(path) = error["path"].as_str() {
        message = format!("{path}: {message}");
    }
    let exit = match answer.status {
        429 => EXIT_CREDENTIAL,
        503 | 504 => EXIT_UNREACHABLE,
        _ => super::exit_for(&code),
    };
    Failure {
        exit,
        code,
        message,
    }
}

/// Make one request on apid's socket and read its answer.
pub fn call(method: &str, path: &str, body: Option<&Value>) -> Result<Answer> {
    let socket = socket();
    let mut stream = UnixStream::connect(&socket).map_err(|error| unreachable(&socket, &error))?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;
    let body = body.map(Value::to_string).unwrap_or_default();
    let request = format!(
        "{method} /api{path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .with_context(|| format!("send to apid at {}", socket.display()))?;
    let mut raw = Vec::new();
    stream
        .take(MAX_ANSWER + 1)
        .read_to_end(&mut raw)
        .with_context(|| format!("read apid's answer at {}", socket.display()))?;
    if raw.len() as u64 > MAX_ANSWER {
        anyhow::bail!("apid's answer is longer than {MAX_ANSWER} bytes");
    }
    parse(&raw)
}

/// apid not answering: off by its setting, or not running. Both are exit 3.
fn unreachable(socket: &std::path::Path, error: &std::io::Error) -> anyhow::Error {
    let off = super::block_on(async {
        let connection = super::system_bus().await?;
        let micad = crate::unit_cli::MicadProxy::new(&connection)
            .await
            .map_err(super::bus_error)?;
        let mode = micad
            .get_settings("access.web.mode")
            .await
            .map_err(super::bus_error)?;
        Ok(serde_json::from_str::<Value>(&mode)? == json!("off"))
    })
    .unwrap_or(false);
    Failure {
        exit: EXIT_UNREACHABLE,
        code: "unreachable".to_string(),
        message: if off {
            "apid is off (access.web.mode off); mica web local serves the local socket only"
                .to_string()
        } else {
            format!("apid is not answering at {}: {error}", socket.display())
        },
    }
    .into()
}

/// An HTTP/1.1 answer: the status line, the headers and the body, whole or
/// chunked.
fn parse(raw: &[u8]) -> Result<Answer> {
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .context("apid's answer has no end of headers")?;
    let head = std::str::from_utf8(&raw[..split]).context("apid's headers are not UTF-8")?;
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .context("apid's answer has no status")?;
    let chunked = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("transfer-encoding") && value.trim() == "chunked"
        })
    });
    let rest = &raw[split + 4..];
    let bytes = if chunked {
        dechunk(rest)?
    } else {
        rest.to_vec()
    };
    let body = if bytes.iter().all(u8::is_ascii_whitespace) {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).context("apid's answer is not JSON")?
    };
    Ok(Answer { status, body })
}

fn dechunk(mut rest: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let end = rest
            .windows(2)
            .position(|window| window == b"\r\n")
            .context("a chunk has no size line")?;
        let size = std::str::from_utf8(&rest[..end])?
            .split(';')
            .next()
            .map(str::trim)
            .and_then(|size| usize::from_str_radix(size, 16).ok())
            .context("a chunk's size is not hexadecimal")?;
        rest = &rest[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        anyhow::ensure!(rest.len() >= size + 2, "a chunk is cut short");
        out.extend_from_slice(&rest[..size]);
        rest = &rest[size + 2..];
    }
}

/// Wait for the task an accepted write answered with, until micad has applied
/// it.
pub fn wait(accepted: &Value) -> Result<()> {
    let Some(task) = accepted["taskId"].as_str() else {
        return Ok(());
    };
    let deadline = std::time::Instant::now() + TASK_TIMEOUT;
    loop {
        let record = get(&format!("/v1/tasks/{}", segment(task)))?;
        if record["status"] == json!("finished") {
            if record["outcome"] == json!("succeeded") {
                return Ok(());
            }
            return Err(Failure {
                exit: EXIT_FAILED,
                code: "apply_failed".to_string(),
                message: format!(
                    "stored, but applying it failed: {}",
                    record["message"].as_str().unwrap_or("no reason given")
                ),
            }
            .into());
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!("stored; micad is still applying it (task {task})");
        }
        std::thread::sleep(POLL);
    }
}

/// `text` as one URL path segment.
#[must_use]
pub fn segment(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_read_whole_or_chunked() {
        let whole = parse(
            b"HTTP/1.1 422 Unprocessable Entity\r\ncontent-type: application/json\r\ncontent-length: 61\r\n\r\n{\"error\":{\"code\":\"validation_failed\",\"message\":\"too short\"}}",
        )
        .unwrap();
        assert_eq!(whole.status, 422);
        assert_eq!(whole.body["error"]["code"], "validation_failed");
        let chunked = parse(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\n{\"a\"\r\n3;x=y\r\n:1}\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!((chunked.status, chunked.body), (200, json!({ "a": 1 })));
        let empty = parse(b"HTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert_eq!((empty.status, empty.body), (204, Value::Null));
        assert!(parse(b"HTTP/1.1 200 OK\r\n").is_err());
    }

    #[test]
    fn a_refusal_carries_the_apis_code_and_exit() {
        for (status, code, exit) in [
            (403, "wrong_password", EXIT_CREDENTIAL),
            (403, "device_code_incorrect", EXIT_CREDENTIAL),
            (429, "login_throttled", EXIT_CREDENTIAL),
            (503, "micad_unreachable", EXIT_UNREACHABLE),
            (422, "validation_failed", EXIT_FAILED),
            (409, "settings_read_only", EXIT_FAILED),
        ] {
            let failure = refusal(&Answer {
                status,
                body: json!({ "error": { "code": code, "message": "no", "path": "hostname" } }),
            });
            assert_eq!(
                (failure.exit, failure.code.as_str()),
                (exit, code),
                "{code}"
            );
            assert_eq!(failure.message, "hostname: no");
        }
    }

    #[test]
    fn a_segment_is_escaped() {
        assert_eq!(segment("SHA256:ab/c+d"), "SHA256%3Aab%2Fc%2Bd");
        assert_eq!(segment("My Wi-Fi"), "My%20Wi-Fi");
    }
}

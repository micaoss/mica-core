use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// A daemon on a socket in `dir` that answers every connection with `answer`
/// and records each request it read.
fn serve(dir: &tempfile::TempDir, answer: &'static str) -> (Client, Arc<Mutex<Vec<String>>>) {
    let path = dir.path().join("api.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = vec![0_u8; 65536];
            let mut read = 0;
            // Headers, then as much body as Content-Length says.
            loop {
                let n = stream.read(&mut buffer[read..]).await.unwrap();
                read += n;
                let text = String::from_utf8_lossy(&buffer[..read]).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .map_or(0, |v| v.trim().parse::<usize>().unwrap());
                    if read >= end + 4 + length {
                        seen.lock().unwrap().push(text);
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            stream.write_all(answer.as_bytes()).await.unwrap();
        }
    });
    (Client::at(path), requests)
}

/// A refusal names the daemon's rule and detail.
#[tokio::test]
async fn a_refusal_names_the_rule() {
    let dir = tempfile::tempdir().unwrap();
    let (client, requests) = serve(
        &dir,
        "HTTP/1.1 404 Not Found\r\nContent-Length: 44\r\n\r\n{\"error\":\"not_found\",\"detail\":\"no web here\"}",
    );
    let err = client.act("web", Verb::Stop).await.unwrap_err();
    assert!(
        matches!(&err, ContainerdError::Refused { status: 404, rule, detail } if rule == "not_found" && detail == "no web here"),
        "{err}"
    );
    assert!(requests.lock().unwrap()[0].starts_with("POST /v1/containers/web/stop HTTP/1.1\r\n"));
}

/// A chunked answer is reassembled.
#[test]
fn a_chunked_answer_is_one_body() {
    let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\n{\"a\":1\r\n1\r\n}\r\n0\r\n\r\n";
    assert_eq!(parse_response(response).unwrap(), json!({ "a": 1 }));
    assert!(
        parse_response(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n").is_err()
    );
    assert!(parse_response(b"not http").is_err());
}

/// No daemon is unavailable, never an empty answer.
#[tokio::test]
async fn no_daemon_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let client = Client::at(dir.path().join("absent.sock"));
    assert!(matches!(
        client.projects().await,
        Err(ContainerdError::Unavailable(_))
    ));
    assert!(matches!(
        NoContainerd.status().await,
        Err(ContainerdError::Unavailable(_))
    ));
}

/// A call can wait behind a stop the daemon is running: its grace, the kill
/// and podman's cleanup. The call outlasts that by a margin, and answers
/// before a D-Bus caller's default 25 s runs out.
#[test]
fn a_call_outlasts_the_stop_grace_and_answers_within_a_bus_call() {
    assert!(CALL_TIMEOUT >= STOP_GRACE + Duration::from_secs(10));
    assert!(CALL_TIMEOUT < Duration::from_secs(25));
}

/// The projects go as one set, each name to its Compose text, or to the
/// text and its project directory.
#[tokio::test]
async fn projects_are_one_put_of_the_set() {
    let dir = tempfile::tempdir().unwrap();
    let (client, requests) = serve(
        &dir,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 15\r\n\r\n{\"projects\":[]}",
    );
    let text = "services:\n  web:\n    image: \"a:1\"\n";
    let container = ContainerSettings {
        enabled: true,
        projects: BTreeMap::from([
            ("app".to_string(), text.to_string()),
            ("site".to_string(), text.to_string()),
        ]),
        project_dirs: BTreeMap::from([("site".to_string(), "/srv/site".to_string())]),
    };
    assert_eq!(
        client.declare_projects(&container).await.unwrap(),
        json!({ "projects": [] })
    );
    let request = requests.lock().unwrap()[0].clone();
    assert!(
        request.starts_with("PUT /v1/projects HTTP/1.1\r\n"),
        "{request}"
    );
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body,
        json!({ "projects": {
            "app": text,
            "site": { "compose": text, "projectDir": "/srv/site" },
        } })
    );
}

/// A log is text, chunked as the daemon streams it, and a refusal of one is
/// still the rule it names.
#[tokio::test]
async fn a_log_is_text() {
    let dir = tempfile::tempdir().unwrap();
    let (client, requests) = serve(
        &dir,
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n6\r\nready\n\r\n0\r\n\r\n",
    );
    assert_eq!(client.logs("app-web-1", 50).await.unwrap(), "ready\n");
    assert!(
        requests.lock().unwrap()[0]
            .starts_with("GET /v1/containers/app-web-1/logs?tail=50 HTTP/1.1\r\n")
    );
    let refused = parse_text_response(
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 39\r\n\r\n{\"error\":\"not-found\",\"detail\":\"no web\"}",
    )
    .unwrap_err();
    assert!(
        matches!(refused, ContainerdError::Refused { status: 404, ref rule, .. } if rule == "not-found")
    );
}

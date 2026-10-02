use std::sync::{Arc, Mutex};

use micad_settings::{ContainerUnit, PortProtocol, PublishedPort, RestartPolicy, VolumeMount};
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

fn unit() -> ContainerUnit {
    ContainerUnit {
        image: "docker.io/library/nginx:1.27".into(),
        command: vec!["nginx".into(), "-g".into(), "daemon off;".into()],
        environment: [("TZ".to_string(), "UTC".to_string())].into(),
        publish: vec![PublishedPort {
            host: 8081,
            container: 80,
            protocol: PortProtocol::Tcp,
        }],
        volumes: vec![VolumeMount {
            host: "/mica/apps/web".into(),
            container: "/usr/share/nginx/html".into(),
            read_only: true,
        }],
        restart: RestartPolicy::OnFailure,
        auto_start: true,
        pids: Some(256),
        memory: Some("128m".into()),
        cpu: None,
    }
}

/// A declared unit maps field for field onto the API's spec, and an
/// undeclared limit is absent rather than zero.
#[test]
fn a_declared_unit_is_the_apis_spec() {
    assert_eq!(
        spec("web", &unit()),
        json!({
            "name": "web",
            "image": "docker.io/library/nginx:1.27",
            "command": ["nginx", "-g", "daemon off;"],
            "environment": { "TZ": "UTC" },
            "publish": [{ "host": 8081, "container": 80, "protocol": "tcp" }],
            "volumes": [{ "host": "/mica/apps/web", "container": "/usr/share/nginx/html", "read_only": true }],
            "limits": { "pids": 256, "memory": "128m" },
            "restart": { "policy": "on-failure" },
            "autostart": true,
        })
    );
}

/// One PUT carries every declared container.
#[tokio::test]
async fn a_declaration_is_one_put_of_every_container() {
    let dir = tempfile::tempdir().unwrap();
    let (client, requests) = serve(
        &dir,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 17\r\n\r\n{\"containers\":[]}",
    );
    let units = BTreeMap::from([("web".to_string(), unit())]);
    assert_eq!(
        client.declare(&units).await.unwrap(),
        json!({ "containers": [] })
    );
    let request = requests.lock().unwrap()[0].clone();
    assert!(
        request.starts_with("PUT /v1/containers HTTP/1.1\r\n"),
        "{request}"
    );
    let body: Value = serde_json::from_str(request.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body, json!({ "containers": [spec("web", &unit())] }));
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
        client.containers().await,
        Err(ContainerdError::Unavailable(_))
    ));
    assert!(matches!(
        NoContainerd.status().await,
        Err(ContainerdError::Unavailable(_))
    ));
}

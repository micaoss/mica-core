use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// One answer of the fake dockerd: the request line it is for, by prefix,
/// and the whole HTTP response.
type Route = (String, String);

/// A JSON answer.
fn ok(body: &Value) -> String {
    let body = body.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}

/// A dockerd on a socket in `dir` answering each request by the first route
/// its request line starts with (404 for none), recording each request line.
fn serve(dir: &tempfile::TempDir, routes: Vec<Route>) -> (Docker, Arc<Mutex<Vec<String>>>) {
    let path = dir.path().join("docker.sock");
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
            loop {
                let n = stream.read(&mut buffer[read..]).await.unwrap();
                read += n;
                if n == 0 || buffer[..read].windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&buffer[..read]).to_string();
            let line = text.lines().next().unwrap_or_default().to_string();
            seen.lock().unwrap().push(line.clone());
            let answer = routes
                .iter()
                .find(|(prefix, _)| line.starts_with(prefix.as_str()))
                .map_or_else(
                    || {
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 30\r\n\r\n{\"message\":\"no such container\"}"
                            .to_string()
                    },
                    |(_, answer)| answer.clone(),
                );
            stream.write_all(answer.as_bytes()).await.unwrap();
        }
    });
    (Docker::at(path), requests)
}

/// A container as `GET /containers/{id}/json` answers it, made by compose
/// when `compose` names its project and service.
fn inspected(name: &str, status: &str, compose: Option<(&str, &str)>) -> Value {
    let labels = match compose {
        Some((project, service)) => json!({
            PROJECT_LABEL: project,
            SERVICE_LABEL: service,
            DIR_LABEL: format!("/srv/{project}"),
        }),
        None => json!({}),
    };
    json!({
        "Id": name,
        "Name": format!("/{name}"),
        "RestartCount": 2,
        "State": { "Status": status, "ExitCode": 3, "Health": { "Status": "healthy" } },
        "Config": { "Image": "nginx:1", "Labels": labels },
    })
}

/// The routes of a dockerd holding `containers`, each listed and inspected.
fn holding(containers: &[Value]) -> Vec<Route> {
    let mut routes: Vec<Route> = vec![(
        "GET /v1.44/containers/json?all=1 ".into(),
        ok(&json!(
            containers
                .iter()
                .map(|c| json!({ "Id": c["Id"] }))
                .collect::<Vec<_>>()
        )),
    )];
    for container in containers {
        routes.push((
            format!(
                "GET /v1.44/containers/{}/json",
                container["Id"].as_str().unwrap()
            ),
            ok(container),
        ));
    }
    routes
}

/// Every container dockerd has is answered: a compose container under its
/// project and service, any other as a project of its own name, each with the
/// state dockerd reports.
#[tokio::test]
async fn every_container_is_answered_with_the_state_dockerd_reports() {
    let dir = tempfile::tempdir().unwrap();
    let (docker, requests) = serve(
        &dir,
        holding(&[
            inspected("site-web-1", "running", Some(("site", "web"))),
            inspected("app-web-1", "restarting", Some(("app", "web"))),
            inspected("db", "exited", Some(("app", "db"))),
            inspected("portainer", "paused", None),
        ]),
    );
    let answer = docker.projects().await.unwrap();
    let names: Vec<&str> = answer["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["app", "portainer", "site"]);
    assert_eq!(answer["projects"][0]["projectDir"], json!("/srv/app"));
    assert_eq!(
        answer["projects"][0]["services"][0],
        json!({
            "project": "app",
            "service": "db",
            "spec": { "name": "db", "image": "nginx:1" },
            "phase": "exited",
            "health": "healthy",
            "restarts": 2,
            "exit_code": 3,
        })
    );
    assert_eq!(
        answer["projects"][0]["services"][1]["phase"],
        json!("restarting")
    );
    let standalone = &answer["projects"][1];
    assert_eq!(standalone["projectDir"], json!(""));
    assert_eq!(standalone["services"][0]["service"], json!("portainer"));
    assert_eq!(standalone["services"][0]["phase"], json!("paused"));
    assert_eq!(
        requests.lock().unwrap()[0],
        "GET /v1.44/containers/json?all=1 HTTP/1.1"
    );
}

/// Ready is dockerd answering its ping.
#[tokio::test]
async fn ready_is_the_ping() {
    let dir = tempfile::tempdir().unwrap();
    let (docker, requests) = serve(
        &dir,
        vec![(
            "GET /v1.44/_ping ".into(),
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nOK".into(),
        )],
    );
    assert_eq!(docker.status().await.unwrap(), json!({ "ping": "OK" }));
    assert_eq!(requests.lock().unwrap()[0], "GET /v1.44/_ping HTTP/1.1");
}

/// The verbs are dockerd's own, with the stop grace; a container already in
/// the state asked for is done, and a refusal says dockerd's message.
#[tokio::test]
async fn the_verbs_are_dockerd_calls() {
    let dir = tempfile::tempdir().unwrap();
    let (docker, requests) = serve(
        &dir,
        vec![
            (
                "POST /v1.44/containers/app-web-1/start ".into(),
                "HTTP/1.1 304 Not Modified\r\nContent-Length: 0\r\n\r\n".into(),
            ),
            (
                "POST /v1.44/containers/app-web-1/".into(),
                "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n".into(),
            ),
        ],
    );
    for verb in [Verb::Start, Verb::Stop, Verb::Restart] {
        docker.act("app-web-1", verb).await.unwrap();
    }
    let err = docker.act("gone", Verb::Start).await.unwrap_err();
    assert_eq!(err.to_string(), "dockerd: refused (404): no such container");
    assert_eq!(
        requests.lock().unwrap()[..3],
        [
            "POST /v1.44/containers/app-web-1/start HTTP/1.1",
            "POST /v1.44/containers/app-web-1/stop?t=10 HTTP/1.1",
            "POST /v1.44/containers/app-web-1/restart?t=10 HTTP/1.1",
        ]
    );
}

/// A log without a terminal comes in frames, and with one as it is.
#[tokio::test]
async fn a_log_is_demultiplexed() {
    let mut frames = Vec::new();
    for (stream, line) in [(1_u8, "ready\n"), (2, "warn\n")] {
        frames.extend_from_slice(&[stream, 0, 0, 0]);
        frames.extend_from_slice(&u32::try_from(line.len()).unwrap().to_be_bytes());
        frames.extend_from_slice(line.as_bytes());
    }
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.multiplexed-stream\r\nContent-Length: {}\r\n\r\n",
        frames.len()
    )
    .into_bytes();
    response.extend_from_slice(&frames);
    let dir = tempfile::tempdir().unwrap();
    let (docker, requests) = serve(
        &dir,
        vec![
            (
                "GET /v1.44/containers/app-web-1/logs".into(),
                String::from_utf8(response).unwrap(),
            ),
            (
                "GET /v1.44/containers/tty/logs".into(),
                "HTTP/1.1 200 OK\r\nContent-Type: application/vnd.docker.raw-stream\r\nContent-Length: 6\r\n\r\nshell\n".into(),
            ),
        ],
    );
    assert_eq!(docker.logs("app-web-1", 50).await.unwrap(), "ready\nwarn\n");
    assert_eq!(docker.logs("tty", 50).await.unwrap(), "shell\n");
    assert!(docker.logs("gone", 50).await.is_err());
    assert_eq!(
        requests.lock().unwrap()[0],
        "GET /v1.44/containers/app-web-1/logs?stdout=1&stderr=1&tail=50 HTTP/1.1"
    );
}

/// No dockerd is unavailable in every read, never an empty list.
#[tokio::test]
async fn no_dockerd_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let docker = Docker::at(dir.path().join("absent.sock"));
    let err = docker.status().await.unwrap_err();
    assert!(
        err.to_string().starts_with("dockerd: not answering"),
        "{err}"
    );
    assert!(docker.projects().await.is_err());
    assert_eq!(docker.images().await["available"], json!(false));
}

/// The images, in the fields podman's carry.
#[tokio::test]
async fn the_images_are_podman_shaped() {
    let dir = tempfile::tempdir().unwrap();
    let (docker, _) = serve(
        &dir,
        vec![(
            "GET /v1.44/images/json".into(),
            ok(&json!([{ "Id": "sha256:1", "RepoTags": ["nginx:1"], "Size": 5, "Created": 7 }])),
        )],
    );
    assert_eq!(
        docker.images().await["entries"][0],
        json!({ "Id": "sha256:1", "Names": ["nginx:1"], "Size": 5, "Created": 7 })
    );
}

/// Nothing is declared to dockerd, and asking says how containers are made.
#[tokio::test]
async fn nothing_is_declared_to_dockerd() {
    let dir = tempfile::tempdir().unwrap();
    let (docker, requests) = serve(&dir, vec![]);
    let err = docker
        .declare_projects(&ContainerSettings::default())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("docker compose"), "{err}");
    assert!(requests.lock().unwrap().is_empty());
}

#[test]
fn a_live_pid_is_a_running_dockerd() {
    let dir = tempfile::tempdir().unwrap();
    let pid = dir.path().join("docker.pid");
    assert!(!running(&pid));
    std::fs::write(&pid, format!("{}\n", std::process::id())).unwrap();
    assert!(running(&pid));
    std::fs::write(&pid, "not a pid\n").unwrap();
    assert!(!running(&pid));
    std::fs::write(&pid, "4194304999\n").unwrap();
    assert!(!running(&pid));
}

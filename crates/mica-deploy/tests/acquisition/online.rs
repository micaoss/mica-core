//! The online catalog and resumed objects.

use mica_deploy::{acquisition::Acquisition, deployments::DeploymentStore};
use serde_json::json;
use std::fs;
use tempfile::TempDir;

use super::*;

#[test]
pub(super) fn online_catalog_and_resumed_objects_converge_on_the_offline_ready_format() {
    use std::net::TcpListener;
    let (archive, key, sha) = archive();
    let descriptor_length = u32::from_be_bytes(archive[8..12].try_into().unwrap()) as usize;
    let descriptor = String::from_utf8(archive[12..12 + descriptor_length].to_vec()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let source = format!("{origin}/update/");
    let catalog = catalog(&descriptor, &sha, "Resume test", &origin);
    let mut responses = catalog.responses(ok);
    responses.push((
        [
            b"HTTP/1.1 206 Partial Content\r\nContent-Length: 8192\r\nContent-Range: bytes 4096-12287/12288\r\nConnection: close\r\n\r\n".as_slice(),
            &[42; 8192],
        ]
        .concat(),
        NO_HOLD,
    ));
    let server = serve(listener, responses);
    let (dir, store) = fixture();
    let keys = [key];
    let acq = Acquisition {
        root: dir.path().join("updates"),
        store: &store,
        keys: &keys,
        board: "uefi-x64",
        arch: "amd64",
        product: "uefi-x64-dev",
        max_bytes: Some(2 * 1024 * 1024),
    };
    fs::create_dir_all(acq.root.join("downloads")).unwrap();
    fs::write(
        acq.root.join(format!("downloads/{sha}.partial")),
        vec![42; 4096],
    )
    .unwrap();
    let selected = acq.check(&source).unwrap().selected.unwrap();
    let ready = acq.fetch(selected).unwrap();
    let requests = server.join().unwrap();
    assert!(
        requests[0].starts_with("GET /update/v2/manifest.json "),
        "{}",
        requests[0]
    );
    assert!(
        requests[1].starts_with("GET /mica/uefi-x64-dev/1/index.json "),
        "{}",
        requests[1]
    );
    assert!(
        requests[2].starts_with("GET /updates/deployment.json "),
        "{}",
        requests[2]
    );
    assert!(
        requests[3].starts_with(&format!("GET /updates/objects/{sha} ")),
        "{}",
        requests[3]
    );
    assert!(
        requests[3]
            .to_ascii_lowercase()
            .contains("range: bytes=4096-")
    );
    assert!(ready.path.is_file());
    assert_eq!(fs::read(acq.objects().join(&sha)).unwrap(), vec![42; 12288]);
    assert!(store.meta.join("catalog.json").is_file());
    assert!(!acq.root.join(format!("downloads/{sha}.partial")).exists());
}

/// Answers each accepted connection with the next raw response, holds the
/// connection open for the paired duration, and returns the request heads.
/// A connection that does not arrive within ten seconds ends the server, so a
/// client that failed before connecting cannot hang the test.
pub(super) fn serve(
    listener: std::net::TcpListener,
    responses: Vec<(Vec<u8>, std::time::Duration)>,
) -> std::thread::JoinHandle<Vec<String>> {
    use std::io::{Read, Write};
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let mut requests = Vec::new();
        for (response, hold) in responses {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(_) if std::time::Instant::now() < deadline => {
                        std::thread::sleep(std::time::Duration::from_millis(10))
                    }
                    Err(_) => return requests,
                }
            };
            stream.set_nonblocking(false).unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            requests.push(String::from_utf8(header).unwrap());
            let _ = stream.write_all(&response);
            std::thread::sleep(hold);
        }
        requests
    })
}

/// What an origin serves for the single-object fixture: the manifest at
/// `/update/v2/manifest.json`, the release's document at
/// `/mica/uefi-x64-dev/1/index.json`, and, below the release's `baseUrl`
/// `<origin>/updates/`, the descriptor and the object at `objects/<sha256>`.
pub(super) struct Catalog {
    pub manifest: String,
    pub release: String,
    pub descriptor: String,
}

impl Catalog {
    /// The three answers a check asks for, in order: the manifest wrapped by
    /// `manifest`, the other two whole.
    pub(super) fn responses(
        &self,
        manifest: fn(&[u8]) -> Vec<u8>,
    ) -> Vec<(Vec<u8>, std::time::Duration)> {
        vec![
            (manifest(self.manifest.as_bytes()), NO_HOLD),
            (ok(self.release.as_bytes()), NO_HOLD),
            (ok(self.descriptor.as_bytes()), NO_HOLD),
        ]
    }
}

pub(super) fn catalog(descriptor: &str, sha: &str, notes: &str, origin: &str) -> Catalog {
    let digest = hex::encode(aws_lc_rs::digest::digest(
        &aws_lc_rs::digest::SHA256,
        descriptor.as_bytes(),
    ));
    Catalog {
        manifest: serde_json::to_string(&json!({"schema":"mica/catalog/v2","revision":1,
            "baseUrl":format!("{origin}/"),
            "products":[{"product":"uefi-x64-dev","board":"uefi-x64",
                "latest":{"id":"test","generation":1,"notes":notes,"path":"mica/uefi-x64-dev/1/index.json"}}]}))
        .unwrap(),
        release: serde_json::to_string(&json!({"schema":"mica/release/v1",
            "baseUrl":format!("{origin}/updates/"),
            "id":"test","product":"uefi-x64-dev","board":"uefi-x64","generation":1,"notes":notes,
            "deployment":{"path":"deployment.json","sha256":digest,"bytes":descriptor.len()},
            "objects":[{"sha256":sha,"bytes":12288,"path":format!("objects/{sha}")}]}))
        .unwrap(),
        descriptor: descriptor.to_owned(),
    }
}

pub(super) fn chunked(body: &[u8]) -> Vec<u8> {
    let mut response =
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
    for chunk in body.chunks(1000) {
        response.extend(format!("{:x}\r\n", chunk.len()).as_bytes());
        response.extend(chunk);
        response.extend(b"\r\n");
    }
    response.extend(b"0\r\n\r\n");
    response
}

pub(super) fn transfer_fixture(
    responses: impl FnOnce(&Catalog) -> Vec<(Vec<u8>, std::time::Duration)>,
    partial: Option<usize>,
) -> (
    TempDir,
    DeploymentStore,
    [u8; 32],
    String,
    String,
    std::thread::JoinHandle<Vec<String>>,
) {
    let (archive, key, sha) = archive();
    let descriptor_length = u32::from_be_bytes(archive[8..12].try_into().unwrap()) as usize;
    let descriptor = String::from_utf8(archive[12..12 + descriptor_length].to_vec()).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let catalog = catalog(
        &descriptor,
        &sha,
        "Transfer test",
        &format!("http://{address}"),
    );
    let server = serve(listener, responses(&catalog));
    let (dir, store) = fixture();
    let updates = dir.path().join("updates/downloads");
    fs::create_dir_all(&updates).unwrap();
    if let Some(bytes) = partial {
        fs::write(updates.join(format!("{sha}.partial")), vec![42; bytes]).unwrap();
    }
    (
        dir,
        store,
        key,
        sha,
        format!("http://{address}/update/"),
        server,
    )
}

pub(super) fn acquisition<'a>(
    dir: &TempDir,
    store: &'a DeploymentStore,
    keys: &'a [[u8; 32]],
) -> Acquisition<'a> {
    Acquisition {
        root: dir.path().join("updates"),
        store,
        keys,
        board: "uefi-x64",
        arch: "amd64",
        product: "uefi-x64-dev",
        max_bytes: Some(2 * 1024 * 1024),
    }
}

pub(super) fn ok(body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend(body);
    response
}

pub(super) const NO_HOLD: std::time::Duration = std::time::Duration::ZERO;

//! Delta acquisition: chunks already here are never fetched.

use aws_lc_rs::digest;
use mica_deploy::{acquisition::Acquisition, deployments::DeploymentStore};
use std::fs;
use tempfile::TempDir;

use super::*;

/// The three catalog answers an origin serves for the single-object fixture.
pub(super) fn delta_catalog(
    descriptor: &str,
    sha: &str,
    origin: &str,
) -> Vec<(Vec<u8>, std::time::Duration)> {
    catalog(descriptor, sha, "Delta test", origin).responses(ok)
}

pub(super) fn http(status: u16, body: &[u8]) -> Vec<u8> {
    let reason = if status == 200 { "OK" } else { "Not Found" };
    let mut response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

/// The object's chunks, cut by the shipped chunker, and the index bytes an
/// origin would publish beside it.
pub(super) fn delta_index(bytes: &[u8]) -> (Vec<(String, u64)>, Vec<u8>) {
    let mut chunks = Vec::new();
    mica_deploy::chunks::chunk(&mut &bytes[..], |_, length, sha| chunks.push((sha, length)))
        .unwrap();
    let object = mica_deploy::components::Artifact {
        sha256: hex::encode(digest::digest(&digest::SHA256, bytes)),
        bytes: bytes.len() as u64,
    };
    let index = mica_deploy::chunks::write_index(&object, &chunks);
    (chunks, index)
}

pub(super) fn delta_acquisition<'a>(
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

#[test]
pub(super) fn a_chunk_already_on_the_device_is_never_fetched() {
    let (archive, key, sha) = archive();
    let length = u32::from_be_bytes(archive[8..12].try_into().unwrap()) as usize;
    let descriptor = String::from_utf8(archive[12..12 + length].to_vec()).unwrap();
    let object = vec![42_u8; 12288];
    let (_, index) = delta_index(&object);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!("http://{address}/update/");
    let server = serve(
        listener,
        [
            delta_catalog(&descriptor, &sha, &format!("http://{address}")),
            vec![(http(200, &index), std::time::Duration::ZERO)],
        ]
        .concat(),
    );
    let (dir, store) = fixture();
    let keys = [key];
    let acq = delta_acquisition(&dir, &store, &keys);
    fs::create_dir_all(acq.objects()).unwrap();
    // A donor with the same content under a name that is not its digest: the
    // object is still missing, and every one of its chunks is already here.
    fs::write(acq.objects().join("donor.img"), &object).unwrap();
    let selected = acq.check(&source).unwrap().selected.unwrap();
    acq.fetch(selected).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(fs::read(acq.objects().join(&sha)).unwrap(), object);
    assert_eq!(
        requests.len(),
        4,
        "the origin was asked for more than the catalog and the index"
    );
    assert!(
        requests[3].starts_with(&format!("GET /updates/objects/{sha}.index ")),
        "{}",
        requests[3]
    );
}

#[test]
pub(super) fn a_chunk_that_is_not_here_is_fetched_from_the_chunk_store() {
    let (archive, key, sha) = archive();
    let length = u32::from_be_bytes(archive[8..12].try_into().unwrap()) as usize;
    let descriptor = String::from_utf8(archive[12..12 + length].to_vec()).unwrap();
    let object = vec![42_u8; 12288];
    let (chunks, index) = delta_index(&object);
    assert_eq!(chunks.len(), 1, "the fixture object is one chunk");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!("http://{address}/update/");
    let server = serve(
        listener,
        [
            delta_catalog(&descriptor, &sha, &format!("http://{address}")),
            vec![
                (http(200, &index), std::time::Duration::ZERO),
                (http(200, &object), std::time::Duration::ZERO),
            ],
        ]
        .concat(),
    );
    let (dir, store) = fixture();
    let keys = [key];
    let acq = delta_acquisition(&dir, &store, &keys);
    let selected = acq.check(&source).unwrap().selected.unwrap();
    acq.fetch(selected).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(fs::read(acq.objects().join(&sha)).unwrap(), object);
    assert_eq!(requests.len(), 5);
    assert!(
        requests[4].starts_with(&format!("GET /updates/chunks/{} ", chunks[0].0)),
        "{}",
        requests[4]
    );
}

#[test]
pub(super) fn an_origin_without_an_index_still_serves_the_whole_object() {
    for (name, index_response) in [
        ("no index", http(404, b"nothing here")),
        (
            "a truncated index",
            http(200, &delta_index(&vec![42_u8; 12288]).1[..40]),
        ),
        (
            "an index for another object",
            http(200, &delta_index(&vec![7_u8; 12288]).1),
        ),
        ("a corrupt index", http(200, b"not a transfer index at all")),
        (
            "an index beyond its bound",
            http(
                200,
                &vec![0_u8; mica_deploy::chunks::index_limit(12288) as usize + 1],
            ),
        ),
    ] {
        let (archive, key, sha) = archive();
        let length = u32::from_be_bytes(archive[8..12].try_into().unwrap()) as usize;
        let descriptor = String::from_utf8(archive[12..12 + length].to_vec()).unwrap();
        let object = vec![42_u8; 12288];
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let source = format!("http://{address}/update/");
        let server = serve(
            listener,
            [
                delta_catalog(&descriptor, &sha, &format!("http://{address}")),
                vec![
                    (index_response, std::time::Duration::ZERO),
                    (http(200, &object), std::time::Duration::ZERO),
                ],
            ]
            .concat(),
        );
        let (dir, store) = fixture();
        let keys = [key];
        let acq = delta_acquisition(&dir, &store, &keys);
        let selected = acq.check(&source).unwrap().selected.unwrap();
        acq.fetch(selected)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let requests = server.join().unwrap();
        assert_eq!(
            fs::read(acq.objects().join(&sha)).unwrap(),
            object,
            "{name}"
        );
        assert!(
            requests[4].starts_with(&format!("GET /updates/objects/{sha} ")),
            "{name}: {}",
            requests[4]
        );
    }
}

/// A chunk store that answers with the wrong bytes must not be able to put
/// them on the device: the chunk is refused against the index, the delta
/// fails, and the whole object is fetched instead.
#[test]
pub(super) fn a_substituted_chunk_is_refused_and_the_object_still_lands() {
    let (archive, key, sha) = archive();
    let length = u32::from_be_bytes(archive[8..12].try_into().unwrap()) as usize;
    let descriptor = String::from_utf8(archive[12..12 + length].to_vec()).unwrap();
    let object = vec![42_u8; 12288];
    let (_, index) = delta_index(&object);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let source = format!("http://{address}/update/");
    let server = serve(
        listener,
        [
            delta_catalog(&descriptor, &sha, &format!("http://{address}")),
            vec![
                (http(200, &index), std::time::Duration::ZERO),
                (http(200, &vec![7_u8; 12288]), std::time::Duration::ZERO),
                (http(200, &object), std::time::Duration::ZERO),
            ],
        ]
        .concat(),
    );
    let (dir, store) = fixture();
    let keys = [key];
    let acq = delta_acquisition(&dir, &store, &keys);
    let selected = acq.check(&source).unwrap().selected.unwrap();
    acq.fetch(selected).unwrap();
    server.join().unwrap();
    assert_eq!(fs::read(acq.objects().join(&sha)).unwrap(), object);
}

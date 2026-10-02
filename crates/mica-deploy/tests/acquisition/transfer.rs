//! The HTTP transfer: chunked bodies, redirects, stalls and TLS.

use std::fs;

use super::*;

#[test]
pub(super) fn transfer_accepts_chunked_catalog_and_close_delimited_object() {
    let mut object = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    object.extend(vec![42; 12288]);
    let (dir, store, key, sha, source, server) = transfer_fixture(
        // The client probes for a transfer index first; this origin publishes
        // none, so it answers 404 once and is asked for the whole object.
        |catalog| {
            let mut responses = catalog.responses(chunked);
            responses.extend([
                (
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        .to_vec(),
                    NO_HOLD,
                ),
                (object, NO_HOLD),
            ]);
            responses
        },
        None,
    );
    let keys = [key];
    let acq = acquisition(&dir, &store, &keys);
    let selected = acq.check(&source).unwrap().selected.unwrap();
    acq.fetch(selected).unwrap();
    let requests = server.join().unwrap();
    assert!(requests[0].starts_with("GET /update/v2/manifest.json HTTP/1.1\r\n"));
    assert!(requests[3].starts_with(&format!("GET /updates/objects/{sha}.index ")));
    assert!(!requests[4].to_ascii_lowercase().contains("range:"));
    assert_eq!(fs::read(acq.objects().join(&sha)).unwrap(), vec![42; 12288]);
}

#[test]
pub(super) fn transfer_refuses_errors_and_oversized_catalogs() {
    let limit = mica_deploy::catalog::MAX_CATALOG_BYTES;
    let mut unsized_body = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    unsized_body.extend(vec![b' '; limit + 1]);
    for response in [
        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            limit + 1
        )
        .into_bytes(),
        chunked(&vec![b' '; limit + 1]),
        unsized_body,
    ] {
        let (dir, store, key, _, source, server) =
            transfer_fixture(|_| vec![(response, NO_HOLD)], None);
        let keys = [key];
        assert!(acquisition(&dir, &store, &keys).check(&source).is_err());
        server.join().unwrap();
    }
}

pub(super) fn redirect(status: &str, location: Option<&str>) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status}\r\n{}Content-Length: 0\r\nConnection: close\r\n\r\n",
        location
            .map(|location| format!("Location: {location}\r\n"))
            .unwrap_or_default()
    )
    .into_bytes()
}

/// A CDN in front of the source: every object is signed, so where the bytes
/// come from is not what authenticates them, and redirects are followed.
#[test]
pub(super) fn transfer_follows_redirects_and_resumes_at_the_new_location() {
    let (dir, store, key, sha, source, server) = transfer_fixture(
        |catalog| {
            [
                vec![(redirect("302 Found", Some("/cdn/manifest.json")), NO_HOLD)],
                catalog.responses(ok),
                vec![
                (redirect("307 Temporary Redirect", Some("../cdn/object")), NO_HOLD),
                (
                    [
                        b"HTTP/1.1 206 Partial Content\r\nContent-Length: 8192\r\nContent-Range: bytes 4096-12287/12288\r\nConnection: close\r\n\r\n".as_slice(),
                        &[42; 8192],
                    ]
                    .concat(),
                    NO_HOLD,
                ),
                ],
            ]
            .concat()
        },
        Some(4096),
    );
    let keys = [key];
    let acq = acquisition(&dir, &store, &keys);
    let selected = acq.check(&source).unwrap().selected.unwrap();
    acq.fetch(selected).unwrap();
    let requests = server.join().unwrap();
    assert!(
        requests[1].starts_with("GET /cdn/manifest.json HTTP/1.1\r\n"),
        "{}",
        requests[1]
    );
    assert!(
        requests[5].starts_with("GET /updates/cdn/object HTTP/1.1\r\n"),
        "{}",
        requests[5]
    );
    assert!(
        requests[5]
            .to_ascii_lowercase()
            .contains("range: bytes=4096-")
    );
    assert_eq!(fs::read(acq.objects().join(&sha)).unwrap(), vec![42; 12288]);
}

#[test]
pub(super) fn transfer_refuses_endless_unlocated_and_non_http_redirects() {
    for (responses, reason) in [
        (
            vec![redirect("301 Moved Permanently", Some("/update/manifest.json")); 11],
            "redirected more than 10 times",
        ),
        (vec![redirect("302 Found", None)], "without a location"),
        (
            vec![redirect(
                "308 Permanent Redirect",
                Some("ftp://127.0.0.1/update/manifest.json"),
            )],
            "unsupported transfer URL",
        ),
        (
            vec![redirect(
                "302 Found",
                Some("http://user:secret@127.0.0.1/update/manifest.json"),
            )],
            "unsupported transfer URL",
        ),
    ] {
        let (dir, store, key, _, source, server) = transfer_fixture(
            |_| responses.into_iter().map(|r| (r, NO_HOLD)).collect(),
            None,
        );
        let keys = [key];
        let Err(error) = acquisition(&dir, &store, &keys).check(&source) else {
            panic!("a bad redirect was accepted: {reason}");
        };
        assert!(format!("{error:#}").contains(reason), "{reason}: {error:#}");
        server.join().unwrap();
    }
}

#[test]
pub(super) fn resumed_object_refuses_a_server_that_ignores_the_range() {
    let (dir, store, key, sha, source, server) = transfer_fixture(
        |catalog| {
            let mut responses = catalog.responses(ok);
            responses.push((ok(&vec![42; 12288]), NO_HOLD));
            responses
        },
        Some(4096),
    );
    let keys = [key];
    let acq = acquisition(&dir, &store, &keys);
    let selected = acq.check(&source).unwrap().selected.unwrap();
    assert!(acq.fetch(selected).is_err());
    assert!(
        server.join().unwrap()[3]
            .to_ascii_lowercase()
            .contains("range: bytes=4096-")
    );
    assert!(!acq.objects().join(&sha).exists());
}

#[test]
pub(super) fn stalled_transfer_is_abandoned() {
    let started = std::time::Instant::now();
    let (dir, store, key, _, source, _server) = transfer_fixture(
        |_| {
            vec![(
                b"HTTP/1.1 200 OK\r\nContent-Length: 4096\r\nConnection: close\r\n\r\n{".to_vec(),
                std::time::Duration::from_secs(60),
            )]
        },
        None,
    );
    let keys = [key];
    assert!(acquisition(&dir, &store, &keys).check(&source).is_err());
    let elapsed = started.elapsed().as_secs();
    assert!((29..45).contains(&elapsed), "abandoned after {elapsed}s");
}

#[test]
pub(super) fn trickled_response_head_is_abandoned() {
    use std::io::{Read, Write};
    let started = std::time::Instant::now();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let source = format!("http://{}/update/", listener.local_addr().unwrap());
    // One header byte every two seconds: never silent for a read timeout,
    // never a complete head, far below the low-speed floor.
    let _server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
        }
        for byte in b"HTTP/1.1 200 OK\r\nX-Padding: "
            .iter()
            .chain([b'a'; 64].iter())
        {
            if stream.write_all(&[*byte]).is_err() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
    });
    let (dir, store) = fixture();
    let keys = [[0; 32]];
    let Err(error) = acquisition(&dir, &store, &keys).check(&source) else {
        panic!("a trickled response head was accepted");
    };
    assert!(format!("{error:#}").contains("too slow"), "{error:#}");
    let elapsed = started.elapsed().as_secs();
    assert!((29..45).contains(&elapsed), "abandoned after {elapsed}s");
}

#[test]
pub(super) fn https_transfer_verifies_the_server_certificate() {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
    let certificate =
        CertificateDer::from_pem_slice(include_bytes!("../tls/untrusted-test-only.cert.pem"))
            .unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("../tls/untrusted-test-only.key.pem"))
        .unwrap();
    let config = std::sync::Arc::new(
        rustls::ServerConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], key)
        .unwrap(),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let source = format!("https://{}/update/", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut connection = rustls::ServerConnection::new(config).unwrap();
        connection.complete_io(&mut stream).map(|_| ())
    });
    let (dir, store) = fixture();
    let keys = [[0; 32]];
    let Err(error) = acquisition(&dir, &store, &keys).check(&source) else {
        panic!("an untrusted certificate was accepted");
    };
    assert!(format!("{error:#}").contains("UnknownIssuer"), "{error:#}");
    assert!(server.join().unwrap().is_err());
}

// --- delta transfer -------------------------------------------------------
//
// The index and the chunks are unsigned, so these tests are about two things:
// that the bytes that land are the signed ones however they were assembled,
// and that every way the delta path can fail ends in the whole object rather
// than in a failed update.

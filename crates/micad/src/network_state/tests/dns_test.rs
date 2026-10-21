//! Asking DNS servers directly.

use super::*;
use std::net::{Ipv4Addr, UdpSocket};

/// A server on loopback that answers every query it gets by echoing it back
/// with the response bit set, or with a wrong id when `wrong_id`.
fn fake_server(wrong_id: bool) -> u16 {
    let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = socket.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 512];
        while let Ok((received, peer)) = socket.recv_from(&mut buffer) {
            let mut reply = buffer[..received].to_vec();
            reply[2] |= 0x80;
            if wrong_id {
                reply[0] ^= 0xff;
            }
            let _ = socket.send_to(&reply, peer);
        }
    });
    port
}

#[test]
pub(super) fn the_query_asks_for_the_a_record_of_the_name() {
    assert_eq!(
        dns_query([0x12, 0x34], "a.bc"),
        [
            0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0, 1, b'a', 2, b'b', b'c', 0, 0, 1, 0, 1
        ]
    );
}

#[tokio::test]
pub(super) async fn a_server_that_answers_is_reachable_and_timed() {
    let port = fake_server(false);
    let results = test_dns_servers(&[Ipv4Addr::LOCALHOST], port, Duration::from_secs(3)).await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].server, "127.0.0.1");
    assert!(results[0].reachable, "{results:?}");
    assert!(
        results[0].latency_ms.is_some_and(|ms| ms < 3000),
        "{results:?}"
    );
}

#[tokio::test]
pub(super) async fn a_silent_server_or_a_reply_to_another_query_is_unreachable() {
    let wrong = fake_server(true);
    // Bound and never read: the query lands and nothing answers.
    let silent = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let silent_port = silent.local_addr().unwrap().port();
    for port in [wrong, silent_port] {
        let started = std::time::Instant::now();
        let results =
            test_dns_servers(&[Ipv4Addr::LOCALHOST], port, Duration::from_millis(300)).await;
        assert_eq!(
            results,
            [DnsServerTest {
                server: "127.0.0.1".to_string(),
                reachable: false,
                latency_ms: None,
            }]
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}

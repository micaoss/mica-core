//! The resolver's answer to a probe name.

use super::*;

/// What the DNS probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsOutcome {
    /// The name resolved to this many addresses.
    Resolved { addresses: usize },
    /// resolved answered with an error, named.
    Failed(String),
    /// The probe crossed [`DNS_PROBE_TIMEOUT`].
    TimedOut,
}

/// What resolved reported.
#[derive(Debug, Clone, Default)]
pub struct DnsEvidence {
    /// Whether `org.freedesktop.resolve1` answered at all.
    pub resolver_reachable: bool,
    /// The servers resolved currently uses, formatted.
    pub servers: Vec<String>,
    /// The probe's name and outcome, when the probe ran.
    pub probe: Option<(String, DnsOutcome)>,
}

/// resolved's `DNS` property and `ResolveHostname`, each soft.
pub(super) async fn observe_dns() -> DnsEvidence {
    let mut evidence = DnsEvidence::default();
    let Ok(connection) = zbus::Connection::system().await else {
        return evidence;
    };
    let Ok(manager) = zbus::Proxy::new(
        &connection,
        "org.freedesktop.resolve1",
        "/org/freedesktop/resolve1",
        "org.freedesktop.resolve1.Manager",
    )
    .await
    else {
        return evidence;
    };
    if let Ok(servers) = manager
        .get_property::<Vec<(i32, i32, Vec<u8>)>>("DNS")
        .await
    {
        evidence.resolver_reachable = true;
        evidence.servers = servers
            .iter()
            .filter_map(|(_, family, bytes)| format_address(Some(i64::from(*family)), bytes))
            .collect();
    }
    let probe = tokio::time::timeout(
        DNS_PROBE_TIMEOUT,
        manager.call::<_, _, (Vec<(i32, i32, Vec<u8>)>, String, u64)>(
            "ResolveHostname",
            &(0i32, DNS_PROBE_NAME, 0i32, 0u64),
        ),
    )
    .await;
    let outcome = match probe {
        Err(_) => DnsOutcome::TimedOut,
        Ok(Ok((addresses, _, _))) => {
            evidence.resolver_reachable = true;
            DnsOutcome::Resolved {
                addresses: addresses.len(),
            }
        }
        Ok(Err(err)) => {
            if let Some((name, message)) = crate::bus::error_reply(&err) {
                evidence.resolver_reachable = true;
                DnsOutcome::Failed(format!("{name}: {}", message.unwrap_or_default()))
            } else {
                DnsOutcome::Failed(err.to_string())
            }
        }
    };
    evidence.probe = Some((DNS_PROBE_NAME.to_string(), outcome));
    evidence
}

/// What asking one server directly found.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DnsServerTest {
    /// The server asked.
    pub server: String,
    /// Whether it answered the query, whatever the answer was.
    pub reachable: bool,
    /// How long the answer took, in milliseconds, when there was one.
    #[serde(rename = "latencyMs")]
    pub latency_ms: Option<u64>,
}

/// One standard query with recursion desired: an `A` record for `name`,
/// under `id`.
pub(super) fn dns_query(id: [u8; 2], name: &str) -> Vec<u8> {
    let mut packet = Vec::with_capacity(18 + name.len());
    packet.extend_from_slice(&id);
    packet.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        packet.push(u8::try_from(label.len()).unwrap_or(0));
        packet.extend_from_slice(label.as_bytes());
    }
    packet.extend_from_slice(&[0, 0, 1, 0, 1]);
    packet
}

/// Send one query for [`DNS_PROBE_NAME`] to `server` on `port` and wait up to
/// `timeout` for a reply carrying its id; blocking.
fn ask_server(server: std::net::Ipv4Addr, port: u16, timeout: Duration) -> DnsServerTest {
    let id = micad_settings::random_bytes::<2>();
    let started = std::time::Instant::now();
    let answered = (|| -> std::io::Result<bool> {
        let socket = std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))?;
        socket.connect((server, port))?;
        socket.send(&dns_query(id, DNS_PROBE_NAME))?;
        let mut buffer = [0u8; 512];
        loop {
            let left = timeout.saturating_sub(started.elapsed());
            if left.is_zero() {
                return Ok(false);
            }
            socket.set_read_timeout(Some(left))?;
            let received = socket.recv(&mut buffer)?;
            // A reply to this query: the id, and the response bit.
            if received >= 12 && buffer[..2] == id && buffer[2] & 0x80 != 0 {
                return Ok(true);
            }
        }
    })()
    .unwrap_or(false);
    DnsServerTest {
        server: server.to_string(),
        reachable: answered,
        latency_ms: answered
            .then(|| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
    }
}

/// Ask each of `servers` on `port` at once, each bounded by `timeout`, and
/// answer in their order.
pub async fn test_dns_servers(
    servers: &[std::net::Ipv4Addr],
    port: u16,
    timeout: Duration,
) -> Vec<DnsServerTest> {
    let asks: Vec<_> = servers
        .iter()
        .map(|&server| tokio::task::spawn_blocking(move || ask_server(server, port, timeout)))
        .collect();
    let mut results = Vec::with_capacity(asks.len());
    for (ask, server) in asks.into_iter().zip(servers) {
        results.push(ask.await.unwrap_or_else(|_| DnsServerTest {
            server: server.to_string(),
            reachable: false,
            latency_ms: None,
        }));
    }
    results
}

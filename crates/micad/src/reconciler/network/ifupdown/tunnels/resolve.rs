//! Host-name endpoints. The kernel takes numeric endpoints only, so micad
//! resolves a name itself, off the apply's path: a task tries each name at
//! once and again after each of [`RETRY_DELAYS`], the last one repeating,
//! until every name has resolved or the next apply replaces the task.
//!
//! Until then the kernel holds no endpoint for the peer, which the
//! observation reports as `pending_endpoint_resolution`. A resolved address
//! is set all the same when it lies inside a prefix the tunnel routes; the
//! observation reports that one as `endpoint_route_loop`, and so does the log.

use std::io;
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use micad_settings::WireguardPeer;

use crate::netlink::tunnel::TunnelKernel;

/// The waits between tries to resolve the endpoints still unresolved.
pub const RETRY_DELAYS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
];

/// How long one resolution may take.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Name resolution.
#[async_trait::async_trait]
pub trait Resolve: Send + Sync {
    /// The addresses `host` names, with `port`.
    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>>;
}

/// The system's resolver, bounded by [`RESOLVE_TIMEOUT`].
pub struct SystemResolver;

#[async_trait::async_trait]
impl Resolve for SystemResolver {
    async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        let host = host.to_string();
        let lookup = tokio::task::spawn_blocking(move || {
            (host.as_str(), port)
                .to_socket_addrs()
                .map(Iterator::collect)
        });
        match tokio::time::timeout(RESOLVE_TIMEOUT, lookup).await {
            Ok(Ok(found)) => found,
            Ok(Err(err)) => Err(io::Error::other(err)),
            Err(_) => Err(io::ErrorKind::TimedOut.into()),
        }
    }
}

/// The host and port of an endpoint that names a host, and `None` for a
/// numeric one, which the apply sets itself.
pub(super) fn host_endpoint(endpoint: &str) -> Option<(String, u16)> {
    if endpoint.parse::<SocketAddr>().is_ok() {
        return None;
    }
    let (host, port) = endpoint.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    Some((host.to_string(), port.parse().ok()?))
}

/// A peer whose endpoint names a host.
pub(super) struct HostEndpoint {
    pub iface: String,
    pub public_key: [u8; 32],
    pub host: String,
    pub port: u16,
    /// The tunnel's peers and this one's place among them, for the loop rule.
    pub peers: Arc<[WireguardPeer]>,
    pub peer: usize,
}

/// Resolve each of `pending` and set it, trying the ones left after each of
/// `delays`, the last one repeating.
pub(super) async fn resolve_until_set(
    kernel: Arc<dyn TunnelKernel>,
    resolver: Arc<dyn Resolve>,
    delays: Vec<Duration>,
    mut pending: Vec<HostEndpoint>,
) {
    let last = delays.last().copied().unwrap_or(Duration::from_secs(60));
    let mut waits = delays.into_iter().chain(std::iter::repeat(last));
    let mut first = true;
    loop {
        let mut left = Vec::new();
        for endpoint in pending {
            if !set(&*kernel, &*resolver, &endpoint, first).await {
                left.push(endpoint);
            }
        }
        pending = left;
        first = false;
        let Some(wait) = waits.next().filter(|_| !pending.is_empty()) else {
            return;
        };
        tokio::time::sleep(wait).await;
    }
}

/// Resolve `endpoint` and set it, answering whether that is done. Only the
/// first try's failure is logged as a warning.
async fn set(
    kernel: &dyn TunnelKernel,
    resolver: &dyn Resolve,
    endpoint: &HostEndpoint,
    first: bool,
) -> bool {
    let HostEndpoint {
        iface, host, port, ..
    } = endpoint;
    let resolved = match resolver.resolve(host, *port).await {
        Ok(found) => found.into_iter().next(),
        Err(err) => {
            if first {
                tracing::warn!(%iface, %host, %err, "a WireGuard endpoint did not resolve; retrying");
            }
            return false;
        }
    };
    let Some(address) = resolved else {
        if first {
            tracing::warn!(%iface, %host, "a WireGuard endpoint resolved to nothing; retrying");
        }
        return false;
    };
    if let Err(err) = kernel.set_endpoint(iface, &endpoint.public_key, address) {
        tracing::warn!(%iface, %host, %err, "a resolved WireGuard endpoint could not be set");
        return false;
    }
    let state = micad_settings::wireguard_peer_state(
        &endpoint.peers,
        &endpoint.peers[endpoint.peer],
        Some(&address.to_string()),
    );
    if state == "endpoint_route_loop" {
        tracing::warn!(%iface, %host, %address, "a WireGuard endpoint resolved inside a prefix the tunnel routes");
    } else {
        tracing::info!(%iface, %host, %address, "set a resolved WireGuard endpoint");
    }
    true
}

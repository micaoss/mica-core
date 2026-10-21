//! The resolution check: the probe name looked up through the device's own
//! resolver every [`DNS_PROBE_INTERVAL`], never from its cache.

use std::sync::Arc;

use super::*;

/// How often the resolution check runs.
pub const DNS_PROBE_INTERVAL: Duration = Duration::from_secs(60);
/// resolved's `SD_RESOLVED_NO_CACHE`: answer from the network, not the cache.
const SD_RESOLVED_NO_CACHE: u64 = 1 << 12;

/// The last resolution check.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DnsProbeResult {
    /// Whether the name resolved to at least one address.
    pub ok: bool,
    /// The name looked up.
    pub name: String,
    /// How long the lookup took, in milliseconds, when it resolved.
    pub latency_ms: Option<u64>,
    /// When the lookup finished, RFC 3339 UTC.
    pub checked_at: String,
    /// Why it did not resolve, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Looks one name up; answers the number of addresses or why there are none.
#[async_trait::async_trait]
pub trait ResolveName: Send + Sync {
    async fn resolve(&self, name: &str) -> Result<usize, String>;
}

/// resolved's `ResolveHostname` with `SD_RESOLVED_NO_CACHE`.
pub struct ResolvedLookup;

#[async_trait::async_trait]
impl ResolveName for ResolvedLookup {
    async fn resolve(&self, name: &str) -> Result<usize, String> {
        let connection = zbus::Connection::system()
            .await
            .map_err(|err| format!("system bus: {err}"))?;
        let manager = zbus::Proxy::new(
            &connection,
            "org.freedesktop.resolve1",
            "/org/freedesktop/resolve1",
            "org.freedesktop.resolve1.Manager",
        )
        .await
        .map_err(|err| format!("systemd-resolved: {err}"))?;
        let (addresses, _, _) = manager
            .call::<_, _, (Vec<(i32, i32, Vec<u8>)>, String, u64)>(
                "ResolveHostname",
                &(0i32, name, 0i32, SD_RESOLVED_NO_CACHE),
            )
            .await
            .map_err(|err| match crate::bus::error_reply(&err) {
                Some((name, message)) => format!("{name}: {}", message.unwrap_or_default()),
                None => err.to_string(),
            })?;
        Ok(addresses.len())
    }
}

/// The C library's resolver, which `resolv.conf` names and which keeps no
/// cache on an OpenRC root.
pub struct SystemLookup;

#[async_trait::async_trait]
impl ResolveName for SystemLookup {
    async fn resolve(&self, name: &str) -> Result<usize, String> {
        tokio::net::lookup_host((name, 0))
            .await
            .map(Iterator::count)
            .map_err(|err| err.to_string())
    }
}

/// The resolution check and its last result.
pub struct DnsProbe {
    resolver: Arc<dyn ResolveName>,
    name: String,
    timeout: Duration,
    last: tokio::sync::Mutex<Option<DnsProbeResult>>,
}

impl DnsProbe {
    /// A check of `name` through `resolver`, each lookup bounded by `timeout`.
    pub fn new(resolver: Arc<dyn ResolveName>, name: &str, timeout: Duration) -> Self {
        Self {
            resolver,
            name: name.to_string(),
            timeout,
            last: tokio::sync::Mutex::new(None),
        }
    }

    /// The production check of [`DNS_PROBE_NAME`] through `resolver`.
    pub fn production(resolver: Arc<dyn ResolveName>) -> Self {
        Self::new(resolver, DNS_PROBE_NAME, DNS_PROBE_TIMEOUT)
    }

    /// Look the name up now, and keep the result as the last.
    pub async fn probe(&self) -> DnsProbeResult {
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(self.timeout, self.resolver.resolve(&self.name)).await;
        let (ok, detail) = match outcome {
            Err(_) => (false, Some(format!("no answer within {:?}", self.timeout))),
            Ok(Ok(0)) => (false, Some("no address".to_string())),
            Ok(Ok(_)) => (true, None),
            Ok(Err(err)) => (false, Some(err)),
        };
        let result = DnsProbeResult {
            ok,
            name: self.name.clone(),
            latency_ms: ok
                .then(|| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)),
            checked_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            detail,
        };
        *self.last.lock().await = Some(result.clone());
        result
    }

    /// The last result, if a check has run.
    pub async fn last(&self) -> Option<DnsProbeResult> {
        self.last.lock().await.clone()
    }

    /// Check now and then every [`DNS_PROBE_INTERVAL`], for as long as the
    /// daemon runs.
    pub async fn run(self: Arc<Self>) {
        let mut ticks = tokio::time::interval(DNS_PROBE_INTERVAL);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            self.probe().await;
        }
    }
}

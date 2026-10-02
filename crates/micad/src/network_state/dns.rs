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
            if let zbus::Error::MethodError(name, message, _) = &err {
                evidence.resolver_reachable = true;
                DnsOutcome::Failed(format!("{name}: {}", message.clone().unwrap_or_default()))
            } else {
                DnsOutcome::Failed(err.to_string())
            }
        }
    };
    evidence.probe = Some((DNS_PROBE_NAME.to_string(), outcome));
    evidence
}

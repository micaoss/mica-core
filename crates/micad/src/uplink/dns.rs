//! DNS following the active uplink: an uplink that is down while another
//! carries the traffic is not asked for names.
//!
//! - systemd: `SetLinkDefaultRoute` false on each down uplink and true again
//!   once it is not; resolved then sends each lookup to the remaining links'
//!   servers. resolved refuses it for a link networkd manages (`LinkBusy`),
//!   whose DNS settings are networkd's: there networkd's own
//!   `SetLinkDefaultRoute` sets it, as `resolvectl` does, with no reload and
//!   no DHCP restart. The value is runtime state, which networkd may reset
//!   when it reconfigures the link, so a down uplink's is set again every
//!   [`REASSERT`].
//! - OpenRC: [`ORDER_PATH`], the active uplink first and the down ones last,
//!   which Base's `mica-resolv` follows when it builds `resolv.conf`; micad
//!   writes it through a sibling and a rename, and runs `mica-resolv` after
//!   each change. With nothing to steer the file is removed and the order is
//!   Base's own. Each uplink is two lines, its `resolv.d` file names:
//!   `00-<iface>`, the servers its entry declares beside DHCP, then `<iface>`,
//!   a lease's (`mica-udhcpc`) or a static entry's. Base skips a name with no
//!   file.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{Candidate, State, select};
use crate::openrc::Commands;

/// The order file Base's `mica-resolv` reads.
pub const ORDER_PATH: &str = "/run/mica/resolv.order";
/// Base's program building `resolv.conf`.
const MICA_RESOLV: &str = "/usr/lib/mica/mica-resolv";
/// How often a down uplink's resolved setting is set again.
pub const REASSERT: Duration = Duration::from_secs(10);
/// resolved's answer for a link whose DNS settings networkd manages.
const LINK_BUSY: &str = "org.freedesktop.resolve1.LinkBusy";

/// Which uplinks' servers are asked, and in what order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnsOrder {
    /// The active uplink, then the other uplinks by metric, the down ones
    /// last; empty when there is nothing to steer.
    pub order: Vec<String>,
    /// The uplinks that are down while another is active.
    pub down: BTreeSet<String>,
}

/// The order `candidates` call for: none while every uplink is down or the
/// natural one is active with none down, as without the check.
#[must_use]
pub fn order(candidates: &[Candidate]) -> DnsOrder {
    let (natural, active) = select(candidates);
    let Some(active) = active else {
        return DnsOrder::default();
    };
    let mut others: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.name != active.name)
        .collect();
    others.sort_by_key(|c| (c.state == State::Down, c.route.metric, c.name.clone()));
    let down: BTreeSet<String> = others
        .iter()
        .filter(|c| c.state == State::Down)
        .map(|c| c.name.clone())
        .collect();
    if down.is_empty() && natural.is_some_and(|n| n.name == active.name) {
        return DnsOrder::default();
    }
    let order = std::iter::once(active.name.clone())
        .chain(others.into_iter().map(|c| c.name.clone()))
        .collect();
    DnsOrder { order, down }
}

/// The text of [`ORDER_PATH`] for `order`: per uplink its two `resolv.d`
/// file names, `00-<iface>` then `<iface>`.
#[must_use]
pub fn order_text(order: &DnsOrder) -> String {
    order
        .order
        .iter()
        .map(|name| format!("00-{name}\n{name}\n"))
        .collect()
}

/// Brings the resolver to an order.
#[async_trait::async_trait]
pub trait Follow: Send + Sync {
    async fn follow(&self, order: &DnsOrder);
}

/// The resolved calls that bring it from `was` down to `now` down: true for
/// each that is no longer down, false for each newly down, and false again
/// for every down one when `due`.
#[must_use]
pub fn resolved_calls(
    was: &BTreeSet<String>,
    now: &BTreeSet<String>,
    due: bool,
) -> Vec<(String, bool)> {
    was.difference(now)
        .map(|name| (name.clone(), true))
        .chain(
            now.iter()
                .filter(|name| due || !was.contains(*name))
                .map(|name| (name.clone(), false)),
        )
        .collect()
}

/// systemd-resolved.
#[derive(Default)]
pub struct Resolved {
    state: tokio::sync::Mutex<(BTreeSet<String>, Option<Instant>)>,
}

impl Resolved {
    async fn set(name: &str, enable: bool) -> Result<(), String> {
        let index: i32 = std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex"))
            .ok()
            .and_then(|text| text.trim().parse().ok())
            .ok_or_else(|| format!("{name} has no interface index"))?;
        let connection = zbus::Connection::system()
            .await
            .map_err(|err| format!("system bus: {err}"))?;
        match connection
            .call_method(
                Some("org.freedesktop.resolve1"),
                "/org/freedesktop/resolve1",
                Some("org.freedesktop.resolve1.Manager"),
                "SetLinkDefaultRoute",
                &(index, enable),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(zbus::Error::MethodError(error, _, _)) if error.as_str() == LINK_BUSY => connection
                .call_method(
                    Some("org.freedesktop.network1"),
                    "/org/freedesktop/network1",
                    Some("org.freedesktop.network1.Manager"),
                    "SetLinkDefaultRoute",
                    &(index, enable),
                )
                .await
                .map(drop)
                .map_err(|err| format!("networkd: {err}")),
            Err(err) => Err(err.to_string()),
        }
    }
}

#[async_trait::async_trait]
impl Follow for Resolved {
    async fn follow(&self, order: &DnsOrder) {
        let mut state = self.state.lock().await;
        let due = state.1.is_none_or(|at| at.elapsed() >= REASSERT);
        let calls = resolved_calls(&state.0, &order.down, due);
        if calls.is_empty() {
            return;
        }
        for (name, enable) in &calls {
            if let Err(err) = Self::set(name, *enable).await {
                tracing::warn!(interface = %name, enable, error = %err, "uplink: resolved SetLinkDefaultRoute");
            }
        }
        state.0.clone_from(&order.down);
        state.1 = Some(Instant::now());
    }
}

/// Base's `mica-resolv` with micad's order file.
pub struct ResolvOrder {
    path: PathBuf,
    rebuild: String,
    commands: Arc<dyn Commands>,
}

impl ResolvOrder {
    #[must_use]
    pub fn production() -> Self {
        Self::new(
            PathBuf::from(ORDER_PATH),
            MICA_RESOLV,
            Arc::new(crate::openrc::Host),
        )
    }

    #[must_use]
    pub fn new(path: PathBuf, rebuild: &str, commands: Arc<dyn Commands>) -> Self {
        Self {
            path,
            rebuild: rebuild.to_string(),
            commands,
        }
    }

    /// Write or remove the file; true when it changed.
    fn write(&self, order: &DnsOrder) -> anyhow::Result<bool> {
        if order.order.is_empty() {
            return match std::fs::remove_file(&self.path) {
                Ok(()) => Ok(true),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(err) => Err(err.into()),
            };
        }
        crate::fswrite::write_config_if_changed(&self.path, &order_text(order), 0o644)
    }
}

#[async_trait::async_trait]
impl Follow for ResolvOrder {
    async fn follow(&self, order: &DnsOrder) {
        match self.write(order) {
            Ok(false) => {}
            Ok(true) => match self.commands.run(&self.rebuild, &[]).await {
                Ok(output) => {
                    if let Err(err) = output.success(&self.rebuild) {
                        tracing::warn!(error = %err, "uplink: rebuild resolv.conf");
                    }
                }
                Err(err) => tracing::warn!(error = %err, "uplink: rebuild resolv.conf"),
            },
            Err(err) => {
                tracing::warn!(error = %err, path = %self.path.display(), "uplink: write the DNS order")
            }
        }
    }
}

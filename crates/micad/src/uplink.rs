//! The uplink engine (`docs/mica-core.md` 3.4): reachability rounds out of
//! each uplink whose check is on, and one default route of micad's own toward
//! the best uplink that answers while the one the kernel would pick does not.
//!
//! micad never changes or removes a route networkd or `mica-udhcpc` owns.
//! Each uplink keeps its own default route at its own metric. When the
//! uplink with the lowest metric (the natural one) is down and one below it
//! is up, micad adds its failover route (protocol 109, metric 10) via the
//! best one that is up, and removes it when the natural one is up again or
//! none is up. So every uplink failing its checks leaves the device as it is
//! without them.

use std::collections::BTreeMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use micad_settings::{IfaceSettings, ProbeConfig, ProbeTarget};
use serde_json::{Value, json};

use crate::netlink::uplink::DefaultRoute;

pub mod dns;
pub mod probe;

/// An uplink's reachability, as the engine holds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    /// Its check is not on.
    Off,
    /// On, with no verdict yet.
    Unknown,
    Up,
    Down,
    /// No carrier, no lease or no default route.
    NoRoute,
    /// Strict reverse-path filtering drops its probes' answers.
    Blocked,
}

impl State {
    /// Whether the uplink may carry the traffic: up, or not known to be down.
    #[must_use]
    pub fn eligible(self) -> bool {
        matches!(self, Self::Up | Self::Unknown | Self::Off)
    }
}

/// One target's answer in a round.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetResult {
    pub address: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// One round: when it ended, and whether any target answered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Round {
    pub at: String,
    pub ok: bool,
    pub targets: Vec<TargetResult>,
}

/// An uplink's check: its state, how long it has held, and the counters
/// toward the next state.
#[derive(Debug, Clone)]
pub struct Tracker {
    pub state: State,
    pub since: String,
    pub failures: u32,
    pub successes: u32,
    pub last_round: Option<Round>,
    next_round: Option<Instant>,
    in_flight: bool,
}

impl Tracker {
    fn new(state: State) -> Self {
        Self {
            state,
            since: now_text(),
            failures: 0,
            successes: 0,
            last_round: None,
            next_round: None,
            in_flight: false,
        }
    }

    /// Move to `state`; the counters start again. True when it changed.
    pub fn set(&mut self, state: State) -> bool {
        if self.state == state {
            return false;
        }
        self.state = state;
        self.since = now_text();
        self.failures = 0;
        self.successes = 0;
        true
    }

    /// One round's verdict under `probe`'s thresholds. True when the state
    /// changed.
    pub fn verdict(&mut self, ok: bool, probe: &ProbeConfig) -> bool {
        if ok {
            self.failures = 0;
            self.successes += 1;
            if matches!(self.state, State::Unknown | State::Down)
                && self.successes >= probe.successes_to_up()
            {
                return self.set(State::Up);
            }
        } else {
            self.successes = 0;
            self.failures += 1;
            if matches!(self.state, State::Unknown | State::Up)
                && self.failures >= probe.failures_to_down()
            {
                return self.set(State::Down);
            }
        }
        false
    }
}

/// An uplink the kernel holds a default route for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub route: DefaultRoute,
    pub state: State,
}

/// Which uplink the kernel would pick (`natural`), and which one should
/// carry the traffic (`active`): the lowest-metric eligible one. None of
/// either when there is no candidate; no active one when none is eligible.
#[must_use]
pub fn select(candidates: &[Candidate]) -> (Option<&Candidate>, Option<&Candidate>) {
    let natural = candidates.iter().min_by_key(|c| (c.route.metric, &c.name));
    let active = candidates
        .iter()
        .filter(|c| c.state.eligible())
        .min_by_key(|c| (c.route.metric, &c.name));
    (natural, active)
}

/// The failover route wanted: via the active uplink when it is not the
/// natural one.
#[must_use]
pub fn wanted(candidates: &[Candidate]) -> Option<&Candidate> {
    match select(candidates) {
        (Some(natural), Some(active)) if natural.name != active.name => Some(active),
        _ => None,
    }
}

/// What the engine needs of the host: the kernel's routes, links and
/// addresses, the failover route's writes, and the probes.
#[async_trait::async_trait]
pub trait Host: Send + Sync {
    fn default_routes(&self) -> std::io::Result<Vec<DefaultRoute>>;
    fn link_name(&self, index: u32) -> Option<String>;
    fn set_override(&self, link: u32, gateway: Option<Ipv4Addr>) -> std::io::Result<()>;
    fn delete_override(&self, route: &DefaultRoute) -> std::io::Result<()>;
    /// The interface's first IPv4 address, the probes' source.
    fn source(&self, iface: &str) -> Option<Ipv4Addr>;
    /// Whether reverse-path filtering is strict on the interface.
    fn rp_filter_strict(&self, iface: &str) -> bool;
    /// One probe of `target` out of `iface` from `source`.
    async fn probe(
        &self,
        iface: &str,
        source: Ipv4Addr,
        target: probe::Target,
        timeout: Duration,
    ) -> Result<Duration, String>;
}

/// The running host.
pub struct Kernel;

#[async_trait::async_trait]
impl Host for Kernel {
    fn default_routes(&self) -> std::io::Result<Vec<DefaultRoute>> {
        crate::netlink::uplink::default_routes()
    }

    fn link_name(&self, index: u32) -> Option<String> {
        std::fs::read_dir("/sys/class/net").ok()?.find_map(|entry| {
            let entry = entry.ok()?;
            let found = std::fs::read_to_string(entry.path().join("ifindex")).ok()?;
            (found.trim().parse::<u32>().ok()? == index)
                .then(|| entry.file_name().to_string_lossy().into_owned())
        })
    }

    fn set_override(&self, link: u32, gateway: Option<Ipv4Addr>) -> std::io::Result<()> {
        crate::netlink::uplink::set_override(link, gateway)
    }

    fn delete_override(&self, route: &DefaultRoute) -> std::io::Result<()> {
        crate::netlink::uplink::delete_override(route)
    }

    fn source(&self, iface: &str) -> Option<Ipv4Addr> {
        crate::netlink::ipv4_addresses()
            .ok()?
            .get(iface)?
            .split('/')
            .next()?
            .parse()
            .ok()
    }

    fn rp_filter_strict(&self, iface: &str) -> bool {
        let read = |scope: &str| {
            std::fs::read_to_string(format!("/proc/sys/net/ipv4/conf/{scope}/rp_filter"))
                .ok()
                .and_then(|text| text.trim().parse::<u8>().ok())
                .unwrap_or(0)
        };
        read("all").max(read(iface)) == 1
    }

    async fn probe(
        &self,
        iface: &str,
        source: Ipv4Addr,
        target: probe::Target,
        timeout: Duration,
    ) -> Result<Duration, String> {
        let iface = iface.to_string();
        tokio::task::spawn_blocking(move || probe::run(&iface, source, target, timeout))
            .await
            .map_err(|err| err.to_string())?
    }
}

/// The last switch of the active uplink.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Change {
    pub at: String,
    pub from: Option<String>,
    pub to: Option<String>,
    /// `probe` (the natural uplink failed its checks), `link` (it lost its
    /// route), `recovered` (it is up again) or `none-up` (no uplink is
    /// eligible).
    pub reason: &'static str,
}

/// The engine and what it last saw.
pub struct Engine {
    host: Arc<dyn Host>,
    trackers: BTreeMap<String, Tracker>,
    candidates: Vec<Candidate>,
    active: Option<String>,
    natural: Option<String>,
    /// Whether an uplink has been active since the engine started: the
    /// first one is where it starts, every later change is a switch.
    started: bool,
    last_change: Option<Change>,
    rounds: (RoundSender, RoundReceiver),
}

/// Where a round's task sends its result, with the uplink's name.
type RoundSender = tokio::sync::mpsc::UnboundedSender<(String, Round)>;
type RoundReceiver = tokio::sync::mpsc::UnboundedReceiver<(String, Round)>;

impl Engine {
    #[must_use]
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self {
            host,
            trackers: BTreeMap::new(),
            candidates: Vec::new(),
            active: None,
            natural: None,
            started: false,
            last_change: None,
            rounds: tokio::sync::mpsc::unbounded_channel(),
        }
    }

    /// Remove every failover route a previous run left, so the engine
    /// decides afresh.
    pub fn clear(&self) {
        for route in self
            .host
            .default_routes()
            .unwrap_or_default()
            .iter()
            .filter(|route| route.is_override())
        {
            if let Err(err) = self.host.delete_override(route) {
                tracing::warn!(error = %err, "uplink: remove a stale failover route");
            }
        }
    }

    /// One pass over `network`: the rounds that ended, the states the kernel
    /// implies, the rounds now due, and the failover route. Answers the
    /// change of the active uplink, when there is one.
    pub fn tick(&mut self, network: &BTreeMap<String, IfaceSettings>) -> Option<Change> {
        while let Ok((iface, round)) = self.rounds.1.try_recv() {
            if let Some(tracker) = self.trackers.get_mut(&iface) {
                tracker.in_flight = false;
                if let Some(probe) = enabled_probe(network, &iface)
                    && matches!(tracker.state, State::Unknown | State::Up | State::Down)
                {
                    tracker.verdict(round.ok, probe);
                }
                tracker.last_round = Some(round);
            }
        }
        let routes = match self.host.default_routes() {
            Ok(routes) => routes,
            Err(err) => {
                tracing::warn!(error = %err, "uplink: read the default routes");
                return None;
            }
        };
        let mut own: BTreeMap<String, DefaultRoute> = BTreeMap::new();
        for route in routes.iter().filter(|route| !route.is_override()) {
            if let Some(name) = self.host.link_name(route.link) {
                let entry = own.entry(name).or_insert(*route);
                if route.metric < entry.metric {
                    *entry = *route;
                }
            }
        }
        self.trackers
            .retain(|iface, _| enabled_probe(network, iface).is_some());
        for (iface, _) in network
            .iter()
            .filter(|(iface, _)| enabled_probe(network, iface).is_some())
        {
            let tracker = self
                .trackers
                .entry(iface.clone())
                .or_insert_with(|| Tracker::new(State::Unknown));
            if !own.contains_key(iface) {
                tracker.set(State::NoRoute);
            } else if self.host.rp_filter_strict(iface) {
                tracker.set(State::Blocked);
            } else if matches!(tracker.state, State::NoRoute | State::Blocked) {
                tracker.set(State::Unknown);
            }
        }
        self.start_rounds(network, &own);
        let previous_natural = self.natural.clone();
        self.candidates = own
            .iter()
            .map(|(name, route)| Candidate {
                name: name.clone(),
                route: *route,
                state: self.trackers.get(name).map_or(State::Off, |t| t.state),
            })
            .collect();
        let (natural, active) = select(&self.candidates);
        let (natural, active) = (
            natural.map(|c| c.name.clone()),
            active.map(|c| c.name.clone()),
        );
        self.write_override(&routes);
        self.natural = natural;
        if active == self.active {
            return None;
        }
        // The first uplink the engine sees active is where it starts, not a
        // switch; once one has been, coming back from none is a change.
        if !self.started {
            self.started = active.is_some();
            self.active = active;
            return None;
        }
        let reason = match (&active, &self.natural) {
            (None, _) => "none-up",
            _ if previous_natural
                .as_ref()
                .is_some_and(|natural| !own.contains_key(natural)) =>
            {
                "link"
            }
            (Some(active), Some(natural)) if active == natural => "recovered",
            _ => "probe",
        };
        let change = Change {
            at: now_text(),
            from: self.active.take(),
            to: active.clone(),
            reason,
        };
        self.active = active;
        self.last_change = Some(change.clone());
        Some(change)
    }

    /// Start the round of each uplink whose check is due.
    fn start_rounds(
        &mut self,
        network: &BTreeMap<String, IfaceSettings>,
        own: &BTreeMap<String, DefaultRoute>,
    ) {
        let now = Instant::now();
        for (iface, tracker) in &mut self.trackers {
            let Some(probe) = enabled_probe(network, iface) else {
                continue;
            };
            if tracker.in_flight
                || !matches!(tracker.state, State::Unknown | State::Up | State::Down)
                || tracker.next_round.is_some_and(|next| next > now)
            {
                continue;
            }
            tracker.next_round = Some(now + Duration::from_secs(probe.interval_s().into()));
            tracker.in_flight = true;
            let gateway = own.get(iface).and_then(|route| route.gateway);
            let (host, sender, iface, probe) = (
                Arc::clone(&self.host),
                self.rounds.0.clone(),
                iface.clone(),
                probe.clone(),
            );
            tokio::spawn(async move {
                let round = run_round(host.as_ref(), &iface, gateway, &probe).await;
                let _ = sender.send((iface, round));
            });
        }
    }

    /// Bring the kernel's failover route to what the candidates want: none,
    /// or one via the active uplink.
    fn write_override(&self, routes: &[DefaultRoute]) {
        let wanted = wanted(&self.candidates).map(|c| (c.route.link, c.route.gateway));
        let present: Vec<&DefaultRoute> = routes.iter().filter(|r| r.is_override()).collect();
        for route in &present {
            if Some((route.link, route.gateway)) != wanted
                && let Err(err) = self.host.delete_override(route)
            {
                tracing::warn!(error = %err, "uplink: remove the failover route");
            }
        }
        if let Some((link, gateway)) = wanted
            && !present
                .iter()
                .any(|route| (route.link, route.gateway) == (link, gateway))
            && let Err(err) = self.host.set_override(link, gateway)
        {
            tracing::warn!(error = %err, "uplink: add the failover route");
        }
    }

    /// Which uplinks' DNS servers are asked, and in what order.
    #[must_use]
    pub fn dns_order(&self) -> dns::DnsOrder {
        dns::order(&self.candidates)
    }

    /// The observation's `uplinks`.
    #[must_use]
    pub fn observation(&self, network: &BTreeMap<String, IfaceSettings>) -> Value {
        let names: std::collections::BTreeSet<&String> = self
            .candidates
            .iter()
            .map(|c| &c.name)
            .chain(self.trackers.keys())
            .collect();
        let list: Vec<Value> = names
            .into_iter()
            .map(|name| {
                let metric = self
                    .candidates
                    .iter()
                    .find(|c| &c.name == name)
                    .map(|c| c.route.metric);
                let probe = network.get(name).and_then(|cfg| cfg.probe.as_ref());
                let mut entry = json!({
                    "name": name,
                    "metric": metric,
                    "state": State::Off,
                    "probe": probe.map(probe_json),
                });
                if let Some(tracker) = self.trackers.get(name) {
                    entry["state"] = json!(tracker.state);
                    entry["since"] = json!(tracker.since);
                    entry["failures"] = json!(tracker.failures);
                    entry["successes"] = json!(tracker.successes);
                    entry["lastRound"] = json!(tracker.last_round);
                }
                entry
            })
            .collect();
        json!({
            "active": self.active,
            "natural": self.natural,
            "override": wanted(&self.candidates).is_some(),
            "lastChange": self.last_change,
            "uplinks": list,
        })
    }
}

#[cfg(test)]
impl Engine {
    /// Make every check's next round due now.
    fn due_now(&mut self) {
        for tracker in self.trackers.values_mut() {
            tracker.next_round = None;
        }
    }
}

/// The interface's check when it is on.
fn enabled_probe<'a>(
    network: &'a BTreeMap<String, IfaceSettings>,
    iface: &str,
) -> Option<&'a ProbeConfig> {
    network
        .get(iface)
        .filter(|cfg| cfg.internet())
        .and_then(|cfg| cfg.probe.as_ref())
        .filter(|probe| probe.enabled)
}

fn probe_json(probe: &ProbeConfig) -> Value {
    json!({
        "enabled": probe.enabled,
        "targets": probe.targets(),
        "intervalS": probe.interval_s(),
        "timeoutMs": probe.timeout_ms(),
        "failuresToDown": probe.failures_to_down(),
        "successesToUp": probe.successes_to_up(),
    })
}

/// Every target of `probe` at once out of `iface`, each within its timeout;
/// the round is good when any answered.
pub async fn run_round(
    host: &dyn Host,
    iface: &str,
    gateway: Option<Ipv4Addr>,
    probe: &ProbeConfig,
) -> Round {
    let timeout = Duration::from_millis(probe.timeout_ms().into());
    let source = host.source(iface);
    let targets = probe.targets();
    let results = futures_util::future::join_all(targets.iter().map(|target| async move {
        let answer = match (source, probe::Target::of(target, gateway)) {
            (None, _) => Err("the interface has no IPv4 address".to_string()),
            (_, Err(why)) => Err(why),
            (Some(source), Ok(resolved)) => {
                match tokio::time::timeout(
                    timeout + Duration::from_millis(500),
                    host.probe(iface, source, resolved, timeout),
                )
                .await
                {
                    Ok(answer) => answer,
                    Err(_) => Err(format!("no answer within {} ms", timeout.as_millis())),
                }
            }
        };
        target_result(target, answer)
    }))
    .await;
    Round {
        at: now_text(),
        ok: results.iter().any(|result| result.ok),
        targets: results,
    }
}

fn target_result(target: &ProbeTarget, answer: Result<Duration, String>) -> TargetResult {
    let port = (target.method == "tcp").then(|| target.port.unwrap_or(ProbeTarget::TCP_PORT));
    match answer {
        Ok(latency) => TargetResult {
            address: target.address.clone(),
            method: target.method.clone(),
            port,
            ok: true,
            latency_ms: Some(u64::try_from(latency.as_millis()).unwrap_or(u64::MAX)),
            detail: None,
        },
        Err(detail) => TargetResult {
            address: target.address.clone(),
            method: target.method.clone(),
            port,
            ok: false,
            latency_ms: None,
            detail: Some(detail),
        },
    }
}

fn now_text() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// How often the engine looks again with no route event.
pub const TICK: Duration = Duration::from_secs(1);

/// The shared engine and its observation, for the bus.
pub type Shared = Arc<tokio::sync::Mutex<Engine>>;

/// Run `engine` for as long as the daemon runs: a pass every [`TICK`] and on
/// every route, address or link event, over the settings `network` reads.
/// Each change of the active uplink goes to `changed`, and after each pass
/// `dns` follows the uplinks.
pub async fn run<F, N>(engine: Shared, network: N, changed: F, dns: Arc<dyn dns::Follow>)
where
    N: Fn() -> futures_util::future::BoxFuture<'static, BTreeMap<String, IfaceSettings>>
        + Send
        + Sync,
    F: Fn(Change) -> futures_util::future::BoxFuture<'static, ()> + Send + Sync,
{
    engine.lock().await.clear();
    let kick = Arc::new(tokio::sync::Notify::new());
    match crate::netlink::uplink::Events::open() {
        Ok(events) => {
            let kick = Arc::clone(&kick);
            std::thread::spawn(move || {
                loop {
                    match events.wait(Duration::from_secs(60)) {
                        Ok(true) => kick.notify_one(),
                        Ok(false) => {}
                        Err(err) => {
                            tracing::warn!(error = %err, "uplink: route events");
                            return;
                        }
                    }
                }
            });
        }
        Err(err) => tracing::warn!(error = %err, "uplink: route events unavailable"),
    }
    loop {
        tokio::select! {
            () = kick.notified() => {}
            () = tokio::time::sleep(TICK) => {}
        }
        let settings = network().await;
        let (change, order) = {
            let mut engine = engine.lock().await;
            let change = engine.tick(&settings);
            (change, engine.dns_order())
        };
        dns.follow(&order).await;
        if let Some(change) = change {
            tracing::info!(
                "MICA_UPLINK active={} previous={} reason={}",
                change.to.as_deref().unwrap_or("-"),
                change.from.as_deref().unwrap_or("-"),
                change.reason
            );
            changed(change).await;
        }
    }
}

#[cfg(test)]
mod tests;

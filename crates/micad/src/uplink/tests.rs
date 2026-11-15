use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use micad_settings::{IfaceSettings, ProbeConfig, ProbeTarget};

use super::*;

fn probe(failures: u32, successes: u32) -> ProbeConfig {
    ProbeConfig {
        enabled: true,
        targets: vec![ProbeTarget {
            address: "192.0.2.10".to_string(),
            method: "icmp".to_string(),
            port: None,
        }],
        failures_to_down: Some(failures),
        successes_to_up: Some(successes),
        ..ProbeConfig::default()
    }
}

// Unknown earns up or down by its thresholds; up and down move only after a
// full run of the opposite verdict, and every change starts the counters
// again.
#[test]
fn the_state_follows_the_thresholds() {
    let thresholds = probe(3, 5);
    let mut tracker = Tracker::new(State::Unknown);
    for _ in 0..4 {
        assert!(!tracker.verdict(true, &thresholds));
    }
    assert!(tracker.verdict(true, &thresholds));
    assert_eq!((tracker.state, tracker.successes), (State::Up, 0));
    assert!(!tracker.verdict(false, &thresholds));
    assert!(!tracker.verdict(true, &thresholds));
    assert!(!tracker.verdict(false, &thresholds));
    assert!(!tracker.verdict(false, &thresholds));
    assert!(tracker.verdict(false, &thresholds));
    assert_eq!((tracker.state, tracker.failures), (State::Down, 0));
    let mut fresh = Tracker::new(State::Unknown);
    let edge = probe(1, 1);
    assert!(fresh.verdict(false, &edge));
    assert_eq!(fresh.state, State::Down);
    assert!(fresh.verdict(true, &edge));
    assert_eq!(fresh.state, State::Up);
}

fn candidate(name: &str, link: u32, metric: u32, state: State) -> Candidate {
    Candidate {
        name: name.to_string(),
        route: DefaultRoute {
            link,
            gateway: Some(Ipv4Addr::new(10, 0, u8::try_from(link).unwrap(), 1)),
            metric,
            protocol: 16,
        },
        state,
    }
}

// The failover route goes via the best eligible uplink only while it is not
// the natural one: never when the natural one is eligible, never when none
// is, and an unprobed uplink counts as eligible.
#[test]
fn the_failover_route_is_wanted_only_past_a_failed_natural_uplink() {
    let name = |c: Option<&Candidate>| c.map(|c| c.name.clone());
    let cases = [
        (
            vec![("eth0", 100, State::Up), ("eth1", 110, State::Up)],
            None,
        ),
        (
            vec![("eth0", 100, State::Down), ("eth1", 110, State::Up)],
            Some("eth1"),
        ),
        (
            vec![("eth0", 100, State::Down), ("eth1", 110, State::Down)],
            None,
        ),
        (
            vec![("eth0", 100, State::Down), ("eth1", 110, State::Off)],
            Some("eth1"),
        ),
        (
            vec![("eth0", 100, State::Down), ("eth1", 110, State::Unknown)],
            Some("eth1"),
        ),
        (
            vec![("eth0", 100, State::Down), ("eth1", 110, State::Blocked)],
            None,
        ),
        (
            vec![
                ("eth0", 20, State::Down),
                ("eth1", 100, State::Down),
                ("wlan0", 600, State::Up),
            ],
            Some("wlan0"),
        ),
        (
            vec![("eth0", 20, State::Up), ("eth1", 100, State::Down)],
            None,
        ),
    ];
    for (links, expected) in cases {
        let candidates: Vec<Candidate> = links
            .iter()
            .enumerate()
            .map(|(index, (n, metric, state))| {
                candidate(n, u32::try_from(index).unwrap() + 2, *metric, *state)
            })
            .collect();
        assert_eq!(
            name(wanted(&candidates)),
            expected.map(str::to_string),
            "{links:?}"
        );
    }
}

/// The kernel and the probes, as the tests set them.
#[derive(Default)]
struct Fake {
    routes: Mutex<Vec<DefaultRoute>>,
    reachable: Mutex<BTreeMap<String, bool>>,
    strict: Mutex<Vec<String>>,
    writes: Mutex<Vec<String>>,
}

impl Fake {
    fn route(&self, link: u32, metric: u32) {
        self.routes.lock().unwrap().push(DefaultRoute {
            link,
            gateway: Some(Ipv4Addr::new(10, 0, u8::try_from(link).unwrap(), 1)),
            metric,
            protocol: 16,
        });
    }

    fn overrides(&self) -> Vec<(u32, Option<Ipv4Addr>)> {
        self.routes
            .lock()
            .unwrap()
            .iter()
            .filter(|route| route.is_override())
            .map(|route| (route.link, route.gateway))
            .collect()
    }
}

#[async_trait::async_trait]
impl Host for Fake {
    fn default_routes(&self) -> std::io::Result<Vec<DefaultRoute>> {
        Ok(self.routes.lock().unwrap().clone())
    }

    fn link_name(&self, index: u32) -> Option<String> {
        Some(format!("eth{}", index - 2))
    }

    fn set_override(&self, link: u32, gateway: Option<Ipv4Addr>) -> std::io::Result<()> {
        self.writes.lock().unwrap().push(format!("set {link}"));
        let mut routes = self.routes.lock().unwrap();
        routes.retain(|route| !route.is_override());
        routes.push(DefaultRoute {
            link,
            gateway,
            metric: 10,
            protocol: 109,
        });
        Ok(())
    }

    fn delete_override(&self, route: &DefaultRoute) -> std::io::Result<()> {
        self.writes
            .lock()
            .unwrap()
            .push(format!("delete {}", route.link));
        self.routes.lock().unwrap().retain(|other| other != route);
        Ok(())
    }

    fn source(&self, _iface: &str) -> Option<Ipv4Addr> {
        Some(Ipv4Addr::new(10, 0, 0, 2))
    }

    fn rp_filter_strict(&self, iface: &str) -> bool {
        self.strict.lock().unwrap().iter().any(|name| name == iface)
    }

    async fn probe(
        &self,
        iface: &str,
        _source: Ipv4Addr,
        _target: probe::Target,
        _timeout: Duration,
    ) -> Result<Duration, String> {
        if self
            .reachable
            .lock()
            .unwrap()
            .get(iface)
            .copied()
            .unwrap_or(false)
        {
            Ok(Duration::from_millis(3))
        } else {
            Err("no answer".to_string())
        }
    }
}

fn network(names: &[&str]) -> BTreeMap<String, IfaceSettings> {
    names
        .iter()
        .map(|name| {
            (
                (*name).to_string(),
                IfaceSettings {
                    dhcp: true,
                    probe: Some(probe(2, 2)),
                    ..IfaceSettings::default()
                },
            )
        })
        .collect()
}

/// Run rounds until none is in flight, then take their verdicts: `rounds`
/// full rounds of every probed uplink.
async fn rounds(engine: &mut Engine, settings: &BTreeMap<String, IfaceSettings>, rounds: usize) {
    for _ in 0..rounds {
        engine.due_now();
        engine.tick(settings);
        while engine.trackers.values().any(|t| t.in_flight) {
            tokio::task::yield_now().await;
            engine.tick(settings);
        }
    }
}

// eth0 (metric 100) fails its checks while eth1 (110) answers: the failover
// route goes via eth1, its change named `probe`. eth0 answering again takes
// it away (`recovered`); both failing leaves none (`none-up`); a strict
// rp_filter blocks an uplink, which is then not eligible.
#[tokio::test]
async fn the_engine_steers_past_a_dead_uplink_and_back() {
    let fake = Arc::new(Fake::default());
    fake.route(2, 100);
    fake.route(3, 110);
    fake.reachable
        .lock()
        .unwrap()
        .insert("eth1".to_string(), true);
    let mut engine = Engine::new(Arc::clone(&fake) as Arc<dyn Host>);
    let settings = network(&["eth0", "eth1"]);

    assert_eq!(engine.tick(&settings), None);
    rounds(&mut engine, &settings, 2).await;
    assert_eq!(engine.trackers["eth0"].state, State::Down);
    assert_eq!(engine.trackers["eth1"].state, State::Up);
    assert_eq!(fake.overrides(), [(3, Some(Ipv4Addr::new(10, 0, 3, 1)))]);
    let change = engine.last_change.clone().unwrap();
    assert_eq!(
        (change.from.as_deref(), change.to.as_deref(), change.reason),
        (Some("eth0"), Some("eth1"), "probe")
    );
    let seen = engine.observation(&settings);
    assert_eq!(seen["active"], "eth1");
    assert_eq!(seen["natural"], "eth0");
    assert_eq!(seen["override"], true);
    assert_eq!(seen["uplinks"][0]["state"], "down");
    assert_eq!(seen["uplinks"][0]["lastRound"]["targets"][0]["ok"], false);
    assert_eq!(
        seen["uplinks"][1]["lastRound"]["targets"][0]["latencyMs"],
        3
    );

    // A renewal's address flush takes the route; the next pass puts it back.
    fake.routes
        .lock()
        .unwrap()
        .retain(|route| !route.is_override());
    engine.tick(&settings);
    assert_eq!(fake.overrides().len(), 1);

    fake.reachable
        .lock()
        .unwrap()
        .insert("eth0".to_string(), true);
    rounds(&mut engine, &settings, 2).await;
    assert!(fake.overrides().is_empty());
    assert_eq!(engine.last_change.clone().unwrap().reason, "recovered");

    fake.reachable.lock().unwrap().clear();
    rounds(&mut engine, &settings, 2).await;
    assert!(fake.overrides().is_empty());
    assert_eq!(engine.last_change.clone().unwrap().reason, "none-up");

    fake.reachable
        .lock()
        .unwrap()
        .insert("eth1".to_string(), true);
    fake.strict.lock().unwrap().push("eth1".to_string());
    rounds(&mut engine, &settings, 2).await;
    assert_eq!(engine.trackers["eth1"].state, State::Blocked);
    assert!(fake.overrides().is_empty());
}

// An uplink that loses its default route is `noRoute` at once and must earn
// `up` again when the route comes back; the failover follows the loss as a
// `link` change. A stale failover route from an earlier run is cleared at
// start, and an unprobed uplink is shown `off`.
#[tokio::test]
async fn a_lost_route_is_no_route_and_a_stale_override_is_cleared() {
    let fake = Arc::new(Fake::default());
    fake.route(2, 100);
    fake.route(3, 110);
    fake.set_override(2, None).unwrap();
    let engine_host = Arc::clone(&fake) as Arc<dyn Host>;
    let mut engine = Engine::new(engine_host);
    engine.clear();
    assert!(fake.overrides().is_empty());

    let mut settings = network(&["eth0"]);
    settings.insert(
        "eth1".to_string(),
        IfaceSettings {
            dhcp: true,
            ..IfaceSettings::default()
        },
    );
    engine.tick(&settings);
    fake.routes.lock().unwrap().retain(|route| route.link != 2);
    let change = engine.tick(&settings).unwrap();
    assert_eq!(
        (change.to.as_deref(), change.reason),
        (Some("eth1"), "link")
    );
    assert_eq!(engine.trackers["eth0"].state, State::NoRoute);
    fake.route(2, 100);
    engine.tick(&settings);
    assert_eq!(engine.trackers["eth0"].state, State::Unknown);
    let seen = engine.observation(&settings);
    assert_eq!(seen["uplinks"][1]["name"], "eth1");
    assert_eq!(seen["uplinks"][1]["state"], "off");
}

// The echo request carries a valid checksum, and only the reply from the
// target with this probe's identifier and sequence is the answer.
#[test]
fn an_echo_is_answered_only_by_its_own_reply() {
    let request = probe::echo_request(0x1234, 7);
    assert_eq!(probe::checksum(&request), 0);
    let target = Ipv4Addr::new(192, 0, 2, 10);
    let mut reply = vec![0x45, 0, 0, 36, 0, 0, 0, 0, 64, 1, 0, 0];
    reply.extend(target.octets());
    reply.extend([10, 0, 0, 2]);
    reply.extend([0, 0, 0, 0, 0x12, 0x34, 0, 7]);
    assert!(probe::is_reply(&reply, target, 0x1234, 7));
    assert!(!probe::is_reply(&reply, target, 0x1234, 8));
    assert!(!probe::is_reply(
        &reply,
        Ipv4Addr::new(192, 0, 2, 11),
        0x1234,
        7
    ));
    assert!(!probe::is_reply(&reply[..20], target, 0x1234, 7));
    let gateway = ProbeTarget {
        address: "gateway".to_string(),
        method: "tcp".to_string(),
        port: None,
    };
    assert_eq!(
        probe::Target::of(&gateway, Some(Ipv4Addr::new(10, 0, 0, 1))),
        Ok(probe::Target::Tcp(std::net::SocketAddrV4::new(
            Ipv4Addr::new(10, 0, 0, 1),
            443
        )))
    );
    assert!(probe::Target::of(&gateway, None).is_err());
}

/// The keys of every object among `values`, together.
fn keys_of(values: &[&serde_json::Value]) -> BTreeSet<String> {
    values
        .iter()
        .filter_map(|value| value.as_object())
        .flat_map(|object| object.keys().cloned())
        .collect()
}

// The observation is what apid's OpenAPI document says `uplinks` is: the
// committed `openapi.json`'s schemas name exactly the keys the engine writes.
#[tokio::test]
async fn the_observation_matches_the_api_schema() {
    let document: serde_json::Value =
        serde_json::from_str(include_str!("../../../mica-apid/openapi.json")).unwrap();
    let schema = |name: &str| -> BTreeSet<String> {
        document["components"]["schemas"][name]["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("{name} is documented"))
            .keys()
            .cloned()
            .collect()
    };
    let fake = Arc::new(Fake::default());
    fake.route(2, 100);
    fake.route(3, 110);
    fake.route(4, 120);
    fake.reachable
        .lock()
        .unwrap()
        .insert("eth1".to_string(), true);
    let mut engine = Engine::new(Arc::clone(&fake) as Arc<dyn Host>);
    let mut settings = network(&["eth0", "eth1"]);
    for cfg in settings.values_mut() {
        cfg.probe.as_mut().unwrap().targets = vec![
            ProbeTarget {
                address: "gateway".to_string(),
                method: "icmp".to_string(),
                port: None,
            },
            ProbeTarget {
                address: "192.0.2.10".to_string(),
                method: "tcp".to_string(),
                port: Some(53),
            },
        ];
    }
    engine.tick(&settings);
    rounds(&mut engine, &settings, 2).await;
    let seen = engine.observation(&settings);
    let uplinks: Vec<&serde_json::Value> = seen["uplinks"].as_array().unwrap().iter().collect();
    assert_eq!(
        uplinks.len(),
        3,
        "two checked uplinks and one without a check"
    );
    let rounds: Vec<&serde_json::Value> = uplinks.iter().map(|u| &u["lastRound"]).collect();
    let targets: Vec<&serde_json::Value> = rounds
        .iter()
        .filter_map(|round| round["targets"].as_array())
        .flatten()
        .collect();
    assert_eq!(keys_of(&[&seen]), schema("UplinksStatus"));
    assert_eq!(keys_of(&[&seen["lastChange"]]), schema("UplinkChange"));
    assert_eq!(keys_of(&uplinks), schema("UplinkStatus"));
    assert_eq!(keys_of(&rounds), schema("UplinkRound"));
    assert_eq!(keys_of(&targets), schema("UplinkTargetResult"));
    let probes: Vec<&serde_json::Value> = uplinks.iter().map(|u| &u["probe"]).collect();
    assert_eq!(keys_of(&probes), schema("UplinkProbe"));
}

#[test]
fn dns_follows_the_active_uplink() {
    let names = |order: &dns::DnsOrder| order.order.join(",");
    let quiet = [
        candidate("eth0", 2, 100, State::Up),
        candidate("eth1", 3, 110, State::Off),
    ];
    assert_eq!(dns::order(&quiet), dns::DnsOrder::default());

    let failover = [
        candidate("eth0", 2, 100, State::Down),
        candidate("eth1", 3, 110, State::Up),
        candidate("wlan0", 4, 600, State::Unknown),
    ];
    let order = dns::order(&failover);
    assert_eq!(names(&order), "eth1,wlan0,eth0");
    assert_eq!(order.down.iter().collect::<Vec<_>>(), ["eth0"]);

    // A lower-ranked uplink down while the natural one carries the traffic.
    let backup_down = [
        candidate("eth0", 2, 100, State::Up),
        candidate("eth1", 3, 110, State::Down),
    ];
    assert_eq!(names(&dns::order(&backup_down)), "eth0,eth1");

    // Every uplink down: as without the check.
    let all_down = [
        candidate("eth0", 2, 100, State::Down),
        candidate("eth1", 3, 110, State::Down),
    ];
    assert_eq!(dns::order(&all_down), dns::DnsOrder::default());
}

#[test]
fn resolved_is_called_for_changes_and_again_when_due() {
    let set = |names: &[&str]| names.iter().map(|n| (*n).to_string()).collect();
    let calls = |was: &[&str], now: &[&str], due| {
        dns::resolved_calls(&set(was), &set(now), due)
            .into_iter()
            .map(|(name, on)| format!("{name}={on}"))
            .collect::<Vec<_>>()
    };
    assert_eq!(calls(&[], &["eth0"], false), ["eth0=false"]);
    assert!(calls(&["eth0"], &["eth0"], false).is_empty());
    assert_eq!(calls(&["eth0"], &["eth0"], true), ["eth0=false"]);
    assert_eq!(
        calls(&["eth0"], &["eth1"], false),
        ["eth0=true", "eth1=false"]
    );
    assert!(calls(&[], &[], true).is_empty());
}

#[tokio::test]
async fn the_order_file_is_written_and_removed_with_a_rebuild_each_time() {
    use dns::Follow as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resolv.order");
    let commands = Arc::new(crate::openrc::FakeCommands::default());
    let follow = dns::ResolvOrder::new(path.clone(), "/usr/lib/mica/mica-resolv", commands.clone());
    let order = dns::order(&[
        candidate("eth0", 2, 100, State::Down),
        candidate("eth1", 3, 110, State::Up),
    ]);
    follow.follow(&order).await;
    follow.follow(&order).await;
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "00-eth1\neth1\n00-eth0\neth0\n"
    );
    let staged: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert_eq!(staged.len(), 1, "nothing staged is left beside the file");
    assert_eq!(commands.calls(), ["/usr/lib/mica/mica-resolv"]);
    follow.follow(&dns::DnsOrder::default()).await;
    follow.follow(&dns::DnsOrder::default()).await;
    assert!(!path.exists());
    assert_eq!(commands.calls().len(), 2);
}

#[cfg(test)]
mod vm;

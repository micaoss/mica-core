use std::sync::Mutex;

use micad_settings::{StaticConfig, WireguardPeer};

use super::*;

/// A link of the fake kernel.
#[derive(Default)]
struct Link {
    index: u32,
    owned: bool,
    up: bool,
    private_key: Option<[u8; 32]>,
    listen_port: Option<u16>,
    peers: BTreeMap<[u8; 32], PeerSet>,
    addresses: Vec<Prefix>,
    routes: Vec<(TunnelRoute, u8)>,
}

/// A kernel holding links by name, and the log of every change made to it.
#[derive(Default)]
struct Fake {
    links: Mutex<BTreeMap<String, Link>>,
    log: Mutex<Vec<String>>,
}

impl Fake {
    fn record(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }

    fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    fn with_link<T>(&self, index: u32, change: impl FnOnce(&str, &mut Link) -> T) -> io::Result<T> {
        let mut links = self.links.lock().unwrap();
        let (name, link) = links
            .iter_mut()
            .find(|(_, link)| link.index == index)
            .ok_or_else(|| io::Error::from_raw_os_error(19))?;
        Ok(change(name, link))
    }

    fn link<T>(&self, name: &str, read: impl FnOnce(&Link) -> T) -> T {
        read(&self.links.lock().unwrap()[name])
    }
}

/// `k<n>` for the test key of byte `n`.
fn short(key: &[u8; 32]) -> String {
    format!("k{}", key[0])
}

fn prefix(prefix: &Prefix) -> String {
    format!("{}/{}", prefix.0, prefix.1)
}

impl TunnelKernel for Fake {
    fn owned_links(&self) -> io::Result<BTreeMap<String, u32>> {
        Ok(self
            .links
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, link)| link.owned)
            .map(|(name, link)| (name.clone(), link.index))
            .collect())
    }

    fn create(&self, name: &str) -> io::Result<u32> {
        let mut links = self.links.lock().unwrap();
        if links.contains_key(name) {
            return Err(io::Error::from_raw_os_error(17));
        }
        let index = 10 + u32::try_from(links.len()).unwrap();
        links.insert(
            name.to_string(),
            Link {
                index,
                owned: true,
                ..Link::default()
            },
        );
        drop(links);
        self.record(format!("create {name}"));
        Ok(index)
    }

    fn delete(&self, index: u32) -> io::Result<()> {
        let name = self.with_link(index, |name, _| name.to_string())?;
        self.links.lock().unwrap().remove(&name);
        self.record(format!("delete {name}"));
        Ok(())
    }

    fn up(&self, index: u32) -> io::Result<()> {
        self.with_link(index, |_, link| link.up = true)
    }

    fn addresses(&self, index: u32) -> io::Result<Vec<Prefix>> {
        self.with_link(index, |_, link| link.addresses.clone())
    }

    fn add_address(&self, index: u32, address: Prefix) -> io::Result<()> {
        let name = self.with_link(index, |name, link| {
            link.addresses.push(address);
            name.to_string()
        })?;
        self.record(format!("address add {name} {}", prefix(&address)));
        Ok(())
    }

    fn delete_address(&self, index: u32, address: Prefix) -> io::Result<()> {
        let name = self.with_link(index, |name, link| {
            link.addresses.retain(|held| *held != address);
            name.to_string()
        })?;
        self.record(format!("address del {name} {}", prefix(&address)));
        Ok(())
    }

    fn peers(&self, name: &str) -> io::Result<Vec<String>> {
        Ok(self.link(name, |link| {
            link.peers.keys().map(|key| BASE64.encode(key)).collect()
        }))
    }

    fn set_device(&self, name: &str, device: &DeviceSet) -> io::Result<()> {
        let mut links = self.links.lock().unwrap();
        let link = links
            .get_mut(name)
            .ok_or_else(|| io::Error::from_raw_os_error(19))?;
        link.private_key = Some(device.private_key);
        link.listen_port = device.listen_port;
        for key in &device.remove {
            link.peers.remove(key);
        }
        for peer in &device.peers {
            link.peers.insert(peer.public_key, peer.clone());
        }
        drop(links);
        let names =
            |keys: Vec<&[u8; 32]>| keys.into_iter().map(short).collect::<Vec<_>>().join(",");
        self.record(format!(
            "set {name} peers=[{}] remove=[{}]",
            names(device.peers.iter().map(|peer| &peer.public_key).collect()),
            names(device.remove.iter().collect()),
        ));
        Ok(())
    }

    fn routes(&self, index: u32) -> io::Result<Vec<(TunnelRoute, u8)>> {
        self.with_link(index, |_, link| link.routes.clone())
    }

    fn add_route(&self, index: u32, route: &TunnelRoute) -> io::Result<()> {
        let name = self.with_link(index, |name, link| {
            if link.routes.iter().any(|(held, _)| {
                held.destination == route.destination && held.metric == route.metric
            }) {
                return Err(io::Error::from_raw_os_error(17));
            }
            link.routes.push((route.clone(), RTPROT_MICA));
            Ok(name.to_string())
        })??;
        self.record(format!("route add {name} {}", prefix(&route.destination)));
        Ok(())
    }

    fn delete_route(&self, index: u32, route: &TunnelRoute) -> io::Result<()> {
        let name = self.with_link(index, |name, link| {
            link.routes
                .retain(|(held, protocol)| !(held == route && *protocol == RTPROT_MICA));
            name.to_string()
        })?;
        self.record(format!("route del {name} {}", prefix(&route.destination)));
        Ok(())
    }
}

fn key(byte: u8) -> String {
    BASE64.encode([byte; 32])
}

fn peer(byte: u8, allowed_ips: &[&str]) -> WireguardPeer {
    WireguardPeer {
        public_key: key(byte),
        allowed_ips: allowed_ips.iter().map(|ip| (*ip).to_string()).collect(),
        endpoint: None,
        persistent_keepalive: None,
        name: None,
        preshared_key: None,
    }
}

fn tunnel(address: &str, peers: Vec<WireguardPeer>) -> IfaceSettings {
    IfaceSettings {
        kind: IfaceKind::Wireguard,
        static_: Some(StaticConfig {
            address: address.to_string(),
            gateway: None,
            dns: Vec::new(),
        }),
        wireguard: Some(WireguardConfig {
            listen_port: Some(51820),
            peers,
        }),
        ..IfaceSettings::default()
    }
}

fn two_peers() -> IfaceSettings {
    tunnel(
        "10.8.0.1/24",
        vec![
            peer(2, &["10.8.0.2/32", "10.20.0.0/24"]),
            peer(3, &["10.8.0.3/32", "10.30.0.0/24"]),
        ],
    )
}

fn network(entries: &[(&str, IfaceSettings)]) -> BTreeMap<String, IfaceSettings> {
    entries
        .iter()
        .map(|(name, cfg)| ((*name).to_string(), cfg.clone()))
        .collect()
}

struct Setup {
    _dir: tempfile::TempDir,
    fake: Arc<Fake>,
    tunnels: Tunnels,
}

fn setup() -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let tunnels = Tunnels::new(fake.clone(), Keystore::under(dir.path(), None));
    Setup {
        _dir: dir,
        fake,
        tunnels,
    }
}

fn route_destinations(fake: &Fake, name: &str) -> Vec<(String, u8)> {
    fake.link(name, |link| {
        link.routes
            .iter()
            .map(|(route, protocol)| (prefix(&route.destination), *protocol))
            .collect()
    })
}

#[test]
fn the_first_apply_builds_the_tunnel_whole() {
    let setup = setup();
    let state = setup
        .tunnels
        .apply(&network(&[
            ("wg0", two_peers()),
            ("eth0", IfaceSettings::default()),
        ]))
        .unwrap();
    assert_eq!(
        setup.fake.take_log(),
        [
            "create wg0",
            "set wg0 peers=[k2,k3] remove=[]",
            "address add wg0 10.8.0.1/24",
            "route add wg0 10.20.0.0/24",
            "route add wg0 10.30.0.0/24",
        ]
    );
    setup.fake.link("wg0", |link| {
        assert!(link.up);
        assert_eq!(link.listen_port, Some(51820));
        assert!(link.private_key.is_some());
        assert_eq!(link.peers[&[2; 32]].allowed_ips.len(), 2);
        assert!(
            link.routes
                .iter()
                .all(|(route, _)| route.source == Some("10.8.0.1".parse().unwrap()))
        );
    });
    // Only the tunnel; its public key is the stored key's.
    assert_eq!(state.keys().collect::<Vec<_>>(), ["wg0"]);
    assert_eq!(state["wg0"]["kind"], "wireguard");
    assert_eq!(state["wg0"]["link"], 10);
    assert_eq!(
        state["wg0"]["publicKey"],
        setup.tunnels.keys.ensure("wg0").unwrap()
    );
}

#[test]
fn the_private_key_set_is_the_stored_one_and_stays_across_applies() {
    let setup = setup();
    let settings = network(&[("wg0", two_peers())]);
    setup.tunnels.apply(&settings).unwrap();
    let first = setup.fake.link("wg0", |link| link.private_key);
    setup.tunnels.apply(&settings).unwrap();
    assert_eq!(setup.fake.link("wg0", |link| link.private_key), first);
    let stored = setup
        .tunnels
        .keys
        .with_private_key("wg0", |key| *key)
        .unwrap();
    assert_eq!(first, Some(stored));
}

#[test]
fn a_restart_reapplies_without_recreating_anything() {
    let setup = setup();
    let settings = network(&[("wg0", two_peers())]);
    setup.tunnels.apply(&settings).unwrap();
    setup.fake.take_log();
    // A new reconciler against the same kernel and keys: micad restarted.
    let restarted = Tunnels::new(setup.fake.clone(), setup.tunnels.keys.clone());
    let state = restarted.apply(&settings).unwrap();
    assert_eq!(setup.fake.take_log(), ["set wg0 peers=[k2,k3] remove=[]"]);
    assert_eq!(state["wg0"]["link"], 10);
}

#[test]
fn a_peer_edit_is_applied_in_place() {
    let setup = setup();
    setup
        .tunnels
        .apply(&network(&[("wg0", two_peers())]))
        .unwrap();
    setup.fake.take_log();
    // Peer 3 moves to another prefix; peer 2 is untouched.
    let mut edited = two_peers();
    edited.wireguard.as_mut().unwrap().peers[1] = peer(3, &["10.8.0.3/32", "10.31.0.0/24"]);
    setup.tunnels.apply(&network(&[("wg0", edited)])).unwrap();
    assert_eq!(
        setup.fake.take_log(),
        [
            "set wg0 peers=[k2,k3] remove=[]",
            "route del wg0 10.30.0.0/24",
            "route add wg0 10.31.0.0/24",
        ]
    );
    setup.fake.link("wg0", |link| {
        assert_eq!(link.index, 10);
        assert_eq!(
            link.peers[&[3; 32]].allowed_ips[1],
            ("10.31.0.0".parse().unwrap(), 24)
        );
    });
}

#[test]
fn a_dropped_peer_is_removed_by_itself_with_its_routes() {
    let setup = setup();
    setup
        .tunnels
        .apply(&network(&[("wg0", two_peers())]))
        .unwrap();
    setup.fake.take_log();
    let mut dropped = two_peers();
    dropped.wireguard.as_mut().unwrap().peers.remove(1);
    setup.tunnels.apply(&network(&[("wg0", dropped)])).unwrap();
    assert_eq!(
        setup.fake.take_log(),
        [
            "set wg0 peers=[k2] remove=[k3]",
            "route del wg0 10.30.0.0/24",
        ]
    );
    setup.fake.link("wg0", |link| {
        assert_eq!(link.peers.keys().collect::<Vec<_>>(), [&[2; 32]]);
    });
}

#[test]
fn a_foreign_route_is_left_alone_even_in_a_wanted_routes_place() {
    let setup = setup();
    setup
        .tunnels
        .apply(&network(&[("wg0", tunnel("10.8.0.1/24", Vec::new()))]))
        .unwrap();
    // Someone else's routes on the link: one stale-looking, one where micad
    // is about to want its own.
    let foreign = |destination: &str| {
        (
            TunnelRoute {
                destination: (destination.parse().unwrap(), 24),
                gateway: None,
                source: None,
                metric: 0,
            },
            4,
        )
    };
    setup
        .fake
        .with_link(10, |_, link| {
            link.routes.push(foreign("10.99.0.0"));
            link.routes.push(foreign("10.20.0.0"));
        })
        .unwrap();
    setup.fake.take_log();
    setup
        .tunnels
        .apply(&network(&[("wg0", two_peers())]))
        .unwrap();
    assert_eq!(
        setup.fake.take_log(),
        [
            "set wg0 peers=[k2,k3] remove=[]",
            "route add wg0 10.30.0.0/24",
        ]
    );
    assert_eq!(
        route_destinations(&setup.fake, "wg0"),
        [
            ("10.99.0.0/24".to_string(), 4),
            ("10.20.0.0/24".to_string(), 4),
            ("10.30.0.0/24".to_string(), RTPROT_MICA),
        ]
    );
}

#[test]
fn a_new_address_replaces_the_old_one() {
    let setup = setup();
    setup
        .tunnels
        .apply(&network(&[("wg0", tunnel("10.8.0.1/24", Vec::new()))]))
        .unwrap();
    setup.fake.take_log();
    setup
        .tunnels
        .apply(&network(&[("wg0", tunnel("10.9.0.1/24", Vec::new()))]))
        .unwrap();
    assert_eq!(
        setup.fake.take_log(),
        [
            "set wg0 peers=[] remove=[]",
            "address del wg0 10.8.0.1/24",
            "address add wg0 10.9.0.1/24",
        ]
    );
}

#[test]
fn a_removed_entry_deletes_its_link_and_key_and_nothing_foreign() {
    let setup = setup();
    setup
        .tunnels
        .apply(&network(&[("wg0", two_peers())]))
        .unwrap();
    // A container's tunnel on the host network: not micad's.
    setup.fake.links.lock().unwrap().insert(
        "wg9".to_string(),
        Link {
            index: 99,
            ..Link::default()
        },
    );
    setup.fake.take_log();
    let state = setup.tunnels.apply(&BTreeMap::new()).unwrap();
    assert!(state.is_empty());
    assert_eq!(setup.fake.take_log(), ["delete wg0"]);
    assert!(setup.fake.links.lock().unwrap().contains_key("wg9"));
    assert!(!setup.tunnels.keys.key_path("wg0").exists());
}

#[test]
fn a_name_held_by_a_foreign_link_is_refused_by_name() {
    let setup = setup();
    setup.fake.links.lock().unwrap().insert(
        "wg0".to_string(),
        Link {
            index: 99,
            ..Link::default()
        },
    );
    let err = setup
        .tunnels
        .apply(&network(&[("wg0", two_peers())]))
        .unwrap_err();
    assert!(format!("{err:#}").starts_with("network.wg0: "), "{err:#}");
    assert!(setup.fake.take_log().is_empty());
}

#[test]
fn a_gateway_and_declared_routes_are_micads_routes_too() {
    let setup = setup();
    let mut cfg = tunnel("fd00:8::1/64", vec![peer(2, &["::/0"])]);
    cfg.static_.as_mut().unwrap().gateway = Some("fd00:8::2".to_string());
    cfg.routes = vec![micad_settings::RouteConfig {
        destination: "fd00:40::/64".to_string(),
        gateway: Some("fd00:8::2".to_string()),
        metric: Some(50),
    }];
    setup.tunnels.apply(&network(&[("wg0", cfg)])).unwrap();
    setup.fake.link("wg0", |link| {
        let routes: Vec<_> = link
            .routes
            .iter()
            .map(|(route, _)| (prefix(&route.destination), route.gateway, route.metric))
            .collect();
        let gateway = Some("fd00:8::2".parse().unwrap());
        assert_eq!(
            routes,
            [
                ("::/0".to_string(), gateway, 1024),
                ("fd00:40::/64".to_string(), gateway, 50),
            ]
        );
    });
}

#[test]
fn a_numeric_endpoint_is_set_and_a_host_name_is_not_yet() {
    let setup = setup();
    let mut numeric = peer(2, &["10.8.0.2/32"]);
    numeric.endpoint = Some("192.0.2.2:51820".to_string());
    numeric.persistent_keepalive = Some(25);
    numeric.preshared_key = Some(key(9));
    let mut named = peer(3, &["10.8.0.3/32"]);
    named.endpoint = Some("vpn.example.org:51820".to_string());
    setup
        .tunnels
        .apply(&network(&[(
            "wg0",
            tunnel("10.8.0.1/24", vec![numeric, named]),
        )]))
        .unwrap();
    setup.fake.link("wg0", |link| {
        let numeric = &link.peers[&[2; 32]];
        assert_eq!(numeric.endpoint, Some("192.0.2.2:51820".parse().unwrap()));
        assert_eq!(numeric.keepalive, Some(25));
        assert_eq!(numeric.preshared_key, Some([9; 32]));
        assert_eq!(link.peers[&[3; 32]].endpoint, None);
    });
}

use super::*;
use micad_settings::{BridgeConfig, StaticConfig, VlanConfig, WireguardPeer};
use micad_settings::{IfaceSettings, WireguardConfig};
use std::sync::{Arc, Mutex};

mod kinds;
mod render;
mod rules;
mod wireguard;

#[test]
fn the_production_delete_is_networkctl_delete() {
    let command = NetworkctlDelete::command("wg0");
    let command = command.as_std();
    assert_eq!(command.get_program(), "networkctl");
    assert_eq!(command.get_args().collect::<Vec<_>>(), ["delete", "wg0"]);
}

const GOLDEN_DHCP: &str = "[Match]\nName=eth0\n\n[Network]\nDHCP=yes\n";
const GOLDEN_STATIC: &str = "[Match]\nName=eth1\n\n[Network]\n\
    Address=192.168.1.10/24\nGateway=192.168.1.1\nDNS=1.1.1.1\nDNS=9.9.9.9\n";
const GOLDEN_EMPTY: &str = "[Match]\nName=eth2\n\n[Network]\n";
const GOLDEN_VLAN_NETDEV: &str = "[NetDev]\nName=eth0.100\nKind=vlan\n\n[VLAN]\nId=100\n";
const GOLDEN_VLAN_PARENT: &str = "[Match]\nName=eth0\n\n[Network]\nDHCP=yes\nVLAN=eth0.100\n";
const GOLDEN_VLAN_CHILD: &str = "[Match]\nName=eth0.100\n\n[Network]\nAddress=192.168.100.2/24\n";
const GOLDEN_BRIDGE_NETDEV: &str = "[NetDev]\nName=br0\nKind=bridge\n";
const GOLDEN_BRIDGE_PORT: &str = "[Match]\nName=eth1\n\n[Network]\nBridge=br0\n";

struct MockReload {
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl NetworkReload for MockReload {
    async fn reload(&self) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push("reload".to_string());
        Ok(())
    }
}

/// Recording [`LinkDelete`], so a test can say which devices the
/// reconciler asked the kernel to drop, and where those requests sit
/// against the reload.
struct MockLink {
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl LinkDelete for MockLink {
    async fn delete_link(&self, iface: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("del {iface}"));
        Ok(())
    }
}

/// A [`LinkDelete`] that records the request and then fails it, for the
/// path where a delete cannot be done.
struct FailingLink {
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl LinkDelete for FailingLink {
    async fn delete_link(&self, iface: &str) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!("del {iface}"));
        Err(anyhow::anyhow!(
            "networkctl delete {iface} failed: no device"
        ))
    }
}

/// The captured output of a tracing subscriber, so a test can assert what
/// this reconciler did and did not write to the journal.
#[derive(Clone, Default)]
struct LogCapture(Arc<Mutex<Vec<u8>>>);

impl LogCapture {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).expect("utf8 log")
    }
}

impl std::io::Write for LogCapture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for LogCapture {
    type Writer = Self;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

/// A key store under `dir`, which every test in this module is a temporary
/// directory: a key drawn anywhere else would be a real private key on the
/// machine running the tests.
fn keystore_in(dir: &std::path::Path) -> Keystore {
    Keystore::under(dir, None)
}

/// A reconciler in `dir` whose reload and device deletions share one call
/// log, so their order is part of what a test can assert.
fn reconciler_in(
    dir: &std::path::Path,
) -> (
    NetworkReconciler<MockReload, MockLink>,
    Arc<Mutex<Vec<String>>>,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let reconciler = NetworkReconciler::new(
        dir.to_path_buf(),
        MockReload {
            calls: Arc::clone(&calls),
        },
        MockLink {
            calls: Arc::clone(&calls),
        },
        keystore_in(dir),
    );
    (reconciler, calls)
}

/// A statically addressed link that hands out addresses and carries one
/// static route: the two fields together, because the rendering order of
/// the sections they produce is part of the file.
#[test]
fn a_server_and_a_route_render_as_their_own_sections() {
    let iface = IfaceSettings {
        dhcp: false,
        static_: Some(StaticConfig {
            address: "192.168.50.1/24".to_string(),
            gateway: None,
            dns: Vec::new(),
        }),
        routes: vec![micad_settings::RouteConfig {
            destination: "10.20.0.0/16".to_string(),
            gateway: Some("192.168.50.254".to_string()),
            metric: Some(200),
        }],
        dhcp_server: Some(micad_settings::DhcpServerConfig {
            pool_offset: 100,
            pool_size: 50,
            dns: vec!["192.168.50.1".to_string()],
            lease_seconds: Some(3600),
        }),
        ..IfaceSettings::default()
    };

    assert_eq!(
        render_unit("eth1", &iface, None, &[]),
        "[Match]\nName=eth1\n\n[Network]\nAddress=192.168.50.1/24\nDHCPServer=yes\n\n\
         [Route]\nDestination=10.20.0.0/16\nGateway=192.168.50.254\nMetric=200\n\n\
         [DHCPServer]\nPoolOffset=100\nPoolSize=50\nDNS=192.168.50.1\nDefaultLeaseTimeSec=3600\n"
    );
}

/// A port's addressing is the bridge's, and so is everything that depends
/// on having an address. A hand-edited document that puts a server on a
/// port renders a port, not a server.
#[test]
fn a_bridge_port_renders_neither_a_route_nor_a_server() {
    let iface = IfaceSettings {
        dhcp: false,
        routes: vec![micad_settings::RouteConfig {
            destination: "10.20.0.0/16".to_string(),
            gateway: None,
            metric: None,
        }],
        dhcp_server: Some(micad_settings::DhcpServerConfig {
            pool_offset: 2,
            pool_size: 10,
            dns: Vec::new(),
            lease_seconds: None,
        }),
        ..IfaceSettings::default()
    };

    assert_eq!(
        render_unit("eth2", &iface, Some("br0"), &[]),
        "[Match]\nName=eth2\n\n[Network]\nBridge=br0\n"
    );
}

/// A tunnel entry with `peers`, addressed statically the way a WireGuard
/// client is.
fn wireguard_iface(listen_port: Option<u16>, peers: Vec<WireguardPeer>) -> IfaceSettings {
    IfaceSettings {
        kind: IfaceKind::Wireguard,
        dhcp: false,
        static_: Some(StaticConfig {
            address: "10.8.0.2/24".to_string(),
            gateway: None,
            dns: Vec::new(),
        }),
        wireguard: Some(WireguardConfig { listen_port, peers }),
        ..IfaceSettings::default()
    }
}

/// A peer's public key: 32 bytes of a fixed pattern in base64, so the
/// golden units are stable and no key is drawn to write a test with.
fn peer_key(byte: u8) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode([byte; 32])
}

fn peer(byte: u8) -> WireguardPeer {
    WireguardPeer {
        public_key: peer_key(byte),
        allowed_ips: vec!["10.8.0.0/24".to_string()],
        endpoint: Some("vpn.example.net:51820".to_string()),
        persistent_keepalive: Some(25),
    }
}

fn vlan_iface(parent: &str, id: u16) -> IfaceSettings {
    IfaceSettings {
        kind: IfaceKind::Vlan,
        dhcp: false,
        static_: Some(StaticConfig {
            address: "192.168.100.2/24".to_string(),
            gateway: None,
            dns: Vec::new(),
        }),
        vlan: Some(VlanConfig {
            parent: parent.to_string(),
            id,
        }),
        ..IfaceSettings::default()
    }
}

fn bridge_iface(ports: &[&str]) -> IfaceSettings {
    IfaceSettings {
        kind: IfaceKind::Bridge,
        dhcp: true,
        bridge: Some(BridgeConfig {
            ports: ports.iter().map(|port| (*port).to_string()).collect(),
        }),
        ..IfaceSettings::default()
    }
}

/// An entry with no addressing of its own, which is what a bridge port has
/// to be.
fn port_iface() -> IfaceSettings {
    IfaceSettings {
        dhcp: false,
        ..IfaceSettings::default()
    }
}

fn dhcp_iface() -> IfaceSettings {
    IfaceSettings {
        dhcp: true,
        ..IfaceSettings::default()
    }
}

fn static_iface() -> IfaceSettings {
    IfaceSettings {
        dhcp: false,
        static_: Some(StaticConfig {
            address: "192.168.1.10/24".to_string(),
            gateway: Some("192.168.1.1".to_string()),
            dns: vec!["1.1.1.1".to_string(), "9.9.9.9".to_string()],
        }),
        ..IfaceSettings::default()
    }
}

fn settings_with(network: &[(&str, IfaceSettings)]) -> Settings {
    Settings {
        network: network
            .iter()
            .map(|(iface, cfg)| ((*iface).to_string(), cfg.clone()))
            .collect(),
        ..Settings::default()
    }
}

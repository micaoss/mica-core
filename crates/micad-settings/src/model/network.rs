//! Network interfaces: addressing, routes, VLAN, bridge, WireGuard and the DHCP server.

/// What kind of link a `network` entry describes.
///
/// Absent means [`IfaceKind::Physical`], and a physical entry never serializes
/// the field: a v6 tree of physical interfaces and its v7 form differ by the
/// schema version integer alone, which is what makes the v6 -> v7 bump
/// additive and the A/B rollback survivable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IfaceKind {
    /// A NIC the kernel already has.
    #[default]
    Physical,
    /// An 802.1Q VLAN on top of another declared entry.
    Vlan,
    /// A software bridge over other declared entries.
    Bridge,
    /// A WireGuard tunnel.
    Wireguard,
}

impl IfaceKind {
    /// Whether this is the default kind, the one that is never written out.
    pub(super) fn is_physical(&self) -> bool {
        matches!(self, Self::Physical)
    }
}

/// Network configuration for a single interface.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IfaceSettings {
    /// What kind of link this is; absent means physical.
    #[serde(default, skip_serializing_if = "IfaceKind::is_physical")]
    pub kind: IfaceKind,
    /// Whether the interface acquires its address via DHCP.
    pub dhcp: bool,
    /// Static addressing, used when `dhcp` is false.
    #[serde(rename = "static", default, skip_serializing_if = "Option::is_none")]
    pub static_: Option<StaticConfig>,
    /// VLAN parameters, for `kind = "vlan"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan: Option<VlanConfig>,
    /// Bridge parameters, for `kind = "bridge"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeConfig>,
    /// WireGuard parameters, for `kind = "wireguard"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireguardConfig>,
    /// Static routes this interface carries, beyond the default route a
    /// `static.gateway` declares.
    ///
    /// Empty and skipped when empty, so an entry that declares none
    /// serializes exactly as it did before routes existed: a document written
    /// by a build that has this field is byte-identical to one written by a
    /// build that does not, until someone uses it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteConfig>,
    /// The DHCP server this interface offers, if it offers one.
    #[serde(
        rename = "dhcpServer",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub dhcp_server: Option<DhcpServerConfig>,
}

/// One static route, rendered as networkd's `[Route]`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteConfig {
    /// Where the route leads, in CIDR notation. `0.0.0.0/0` is the default
    /// route, which is what `static.gateway` already writes -- declaring both
    /// is refused rather than rendered twice.
    pub destination: String,
    /// The next hop. Absent is a route out of this interface with no gateway,
    /// which is what an on-link route is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// Route metric; absent leaves networkd's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<u32>,
}

/// The DHCP server offered on an interface, rendered as networkd's
/// `[DHCPServer]`.
///
/// The pool is expressed the way networkd expresses it -- an offset into the
/// interface's own subnet and a count -- rather than as a first and last
/// address, because that is what the rendered file takes and a range converted
/// twice is a range that can disagree with itself.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DhcpServerConfig {
    /// First address handed out, as an offset from the subnet address.
    #[serde(rename = "poolOffset")]
    pub pool_offset: u32,
    /// How many addresses the pool holds.
    #[serde(rename = "poolSize")]
    pub pool_size: u32,
    /// DNS servers announced to clients; empty announces none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns: Vec<String>,
    /// Default lease time in seconds; absent leaves networkd's own default.
    #[serde(
        rename = "leaseSeconds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub lease_seconds: Option<u32>,
}

/// The 802.1Q parameters of a VLAN interface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VlanConfig {
    /// Name of the `network` entry this VLAN sits on.
    pub parent: String,
    /// 802.1Q VLAN id.
    pub id: u16,
}

/// The parameters of a software bridge.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeConfig {
    /// Names of the `network` entries enslaved to this bridge.
    #[serde(default)]
    pub ports: Vec<String>,
}

/// The parameters of a WireGuard tunnel.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireguardConfig {
    /// UDP port to listen on. Absent lets the kernel pick one, which is what a
    /// client that only ever initiates wants.
    #[serde(
        rename = "listenPort",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub listen_port: Option<u16>,
    /// The far ends of the tunnel.
    #[serde(default)]
    pub peers: Vec<WireguardPeer>,
}

/// One far end of a WireGuard tunnel.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WireguardPeer {
    /// The peer's base64 X25519 public key.
    #[serde(rename = "publicKey")]
    pub public_key: String,
    /// CIDRs routed to this peer.
    #[serde(rename = "allowedIps", default)]
    pub allowed_ips: Vec<String>,
    /// `host:port` to send to, for a peer this end initiates to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Keepalive interval in seconds, for a peer behind NAT.
    #[serde(
        rename = "persistentKeepalive",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub persistent_keepalive: Option<u16>,
}

/// Static addressing for a single interface.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaticConfig {
    /// Interface address in CIDR notation, e.g. `"192.168.1.10/24"`.
    pub address: String,
    /// Default gateway address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    /// DNS server addresses.
    #[serde(default)]
    pub dns: Vec<String>,
}

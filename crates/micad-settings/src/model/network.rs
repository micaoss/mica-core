//! Network interfaces: addressing, routes, VLAN, bridge, WireGuard, CAN and the DHCP server.

/// What kind of link a `network` entry describes.
///
/// Absent means [`IfaceKind::Physical`], and a physical entry never serializes
/// the field.
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
    /// A CAN controller the kernel already has.
    Can,
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
    /// Whether the interface acquires its address via DHCP; absent is false.
    #[serde(default)]
    pub dhcp: bool,
    /// Static addressing, used when `dhcp` is false.
    #[serde(rename = "static", default, skip_serializing_if = "Option::is_none")]
    pub static_: Option<StaticConfig>,
    /// DNS servers a DHCP entry uses instead of the ones its lease gives.
    /// Only with `dhcp`: a static entry names its servers in `static.dns`.
    ///
    /// Empty and skipped when empty, like `routes`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns: Vec<String>,
    /// VLAN parameters, for `kind = "vlan"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vlan: Option<VlanConfig>,
    /// Bridge parameters, for `kind = "bridge"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeConfig>,
    /// WireGuard parameters, for `kind = "wireguard"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard: Option<WireguardConfig>,
    /// CAN parameters, for `kind = "can"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can: Option<CanConfig>,
    /// Static routes this interface carries, beyond the default route a
    /// `static.gateway` declares.
    ///
    /// Empty and skipped when empty, so an entry that declares none
    /// serializes exactly as it did before routes existed: a document written
    /// by a build that has this field is byte-identical to one written by a
    /// build that does not, until someone uses it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteConfig>,
    /// Whether the interface may carry the default route: `false` takes a
    /// DHCP lease's address and subnet but not its gateway, and refuses a
    /// static one. Absent is `true`; physical, bridge and VLAN entries only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub internet: Option<bool>,
}

impl IfaceSettings {
    /// Whether the interface may carry the default route ([`Self::internet`]).
    #[must_use]
    pub fn internet(&self) -> bool {
        self.internet != Some(false)
    }
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

/// The 802.1Q parameters of a VLAN interface.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VlanConfig {
    /// Name of the `network` entry this VLAN sits on.
    pub parent: String,
    /// 802.1Q VLAN id.
    pub id: u16,
}

/// The parameters of a CAN controller.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CanConfig {
    /// The nominal (arbitration) bit rate, in bit/s.
    pub bitrate: u32,
    /// The delay before the controller restarts after bus-off; absent or 0
    /// never restarts it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_ms: Option<u32>,
    /// CAN FD on or off; absent is off. An FD-capable controller (the
    /// RK3576's) does not come up with a classic bit rate alone, and switching
    /// FD off is accepted by a controller without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fd: Option<bool>,
    /// The CAN FD data-phase bit rate, in bit/s, with `fd: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_bitrate: Option<u32>,
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
    /// A label for the peer, for people; never rendered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A pre-shared key mixed into the handshake: 32 bytes in padded base64,
    /// like a public key.
    #[serde(
        rename = "presharedKey",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub preshared_key: Option<String>,
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

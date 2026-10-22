//! The reads the network observation makes over netlink, because no tool
//! the image carries answers them: a CAN controller's state and counters
//! (rtnetlink; `ip -details -statistics` needs iproute2, which Base leaves
//! out of the floor), a WireGuard device's peers (generic netlink; `wg
//! show` needs wireguard-tools, which no image ships), and the kernel's
//! IPv4 addresses (rtnetlink), which BLE provisioning reads whatever
//! configures them.
//!
//! Both are one request and the replies to it, on a socket that lives for
//! that exchange. The parsers take the reply bytes, so they are tested
//! without a kernel.

use std::io;
use std::time::Duration;

use base64::Engine as _;
use rustix::net::{
    AddressFamily, RecvFlags, SendFlags, SocketFlags, SocketType, netlink, recv, sendto,
    socket_with,
};
use serde_json::{Map, Value, json};

/// How long one reply may take.
const TIMEOUT: Duration = Duration::from_secs(1);

const NLMSG_HEADER: usize = 16;
const NLMSG_ERROR: u16 = 2;
const NLMSG_DONE: u16 = 3;
const NLM_F_REQUEST: u16 = 0x1;
const NLM_F_MULTI: u16 = 0x2;
const NLM_F_DUMP: u16 = 0x300;
/// The attribute type bits: the top two are the nested and byte-order flags.
const NLA_TYPE_MASK: u16 = 0x3fff;

const RTM_GETLINK: u16 = 18;
const RTM_NEWADDR: u16 = 20;
const RTM_GETADDR: u16 = 22;
const IFADDRMSG: usize = 8;
const AF_INET: u8 = 2;
const RT_SCOPE_HOST: u8 = 254;
const IFA_LOCAL: u16 = 2;
const IFA_LABEL: u16 = 3;
const IFINFOMSG: usize = 16;
const IFLA_IFNAME: u16 = 3;
const IFLA_LINKINFO: u16 = 18;
const IFLA_STATS64: u16 = 23;
const IFLA_INFO_KIND: u16 = 1;
const IFLA_INFO_DATA: u16 = 2;
const IFLA_INFO_XSTATS: u16 = 4;
const IFLA_CAN_BITTIMING: u16 = 1;
const IFLA_CAN_STATE: u16 = 4;
const IFLA_CAN_BERR_COUNTER: u16 = 8;

const GENL_ID_CTRL: u16 = 0x10;
const GENLMSG: usize = 4;
const CTRL_CMD_GETFAMILY: u8 = 3;
const CTRL_ATTR_FAMILY_ID: u16 = 1;
const CTRL_ATTR_FAMILY_NAME: u16 = 2;
const WG_GENL_NAME: &str = "wireguard";
const WG_GENL_VERSION: u8 = 1;
const WG_CMD_GET_DEVICE: u8 = 0;
const WGDEVICE_A_IFNAME: u16 = 2;
const WGDEVICE_A_PEERS: u16 = 8;
const WGPEER_A_PUBLIC_KEY: u16 = 1;
const WGPEER_A_PERSISTENT_KEEPALIVE_INTERVAL: u16 = 5;
const WGPEER_A_LAST_HANDSHAKE_TIME: u16 = 6;
const WGPEER_A_RX_BYTES: u16 = 7;
const WGPEER_A_TX_BYTES: u16 = 8;

/// The attributes in `bytes`: each one's type, flags masked off, and payload.
/// A truncated tail ends the walk.
pub(crate) fn attributes(mut bytes: &[u8]) -> Vec<(u16, &[u8])> {
    let mut found = Vec::new();
    while bytes.len() >= 4 {
        let length = usize::from(u16::from_ne_bytes([bytes[0], bytes[1]]));
        if length < 4 || length > bytes.len() {
            break;
        }
        let kind = u16::from_ne_bytes([bytes[2], bytes[3]]) & NLA_TYPE_MASK;
        found.push((kind, &bytes[4..length]));
        bytes = &bytes[align(length).min(bytes.len())..];
    }
    found
}

fn align(length: usize) -> usize {
    (length + 3) & !3
}

fn attribute(kind: u16, payload: &[u8]) -> Vec<u8> {
    let length = 4 + payload.len();
    let mut bytes = Vec::with_capacity(align(length));
    bytes.extend_from_slice(&u16::try_from(length).unwrap_or(u16::MAX).to_ne_bytes());
    bytes.extend_from_slice(&kind.to_ne_bytes());
    bytes.extend_from_slice(payload);
    bytes.resize(align(length), 0);
    bytes
}

fn name_payload(name: &str) -> Vec<u8> {
    let mut bytes = name.as_bytes().to_vec();
    bytes.push(0);
    bytes
}

fn message(kind: u16, flags: u16, body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(NLMSG_HEADER + body.len()).unwrap_or(u32::MAX);
    let mut bytes = Vec::with_capacity(NLMSG_HEADER + body.len());
    bytes.extend_from_slice(&length.to_ne_bytes());
    bytes.extend_from_slice(&kind.to_ne_bytes());
    bytes.extend_from_slice(&(flags | NLM_F_REQUEST).to_ne_bytes());
    bytes.extend_from_slice(&1u32.to_ne_bytes());
    bytes.extend_from_slice(&0u32.to_ne_bytes());
    bytes.extend_from_slice(body);
    bytes
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_ne_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_ne_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_ne_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

/// The messages in one received datagram: each one's type, flags and body.
fn messages(mut bytes: &[u8]) -> Vec<(u16, u16, &[u8])> {
    let mut found = Vec::new();
    while bytes.len() >= NLMSG_HEADER {
        let Some(length) = u32_at(bytes, 0).and_then(|length| usize::try_from(length).ok()) else {
            break;
        };
        if length < NLMSG_HEADER || length > bytes.len() {
            break;
        }
        let kind = u16_at(bytes, 4).unwrap_or(0);
        let flags = u16_at(bytes, 6).unwrap_or(0);
        found.push((kind, flags, &bytes[NLMSG_HEADER..length]));
        bytes = &bytes[align(length).min(bytes.len())..];
    }
    found
}

/// Send `request` on a fresh `protocol` socket and collect the bodies of
/// every reply, until the dump ends or the one reply arrives. A kernel error
/// is the error it carries. `None` is `NETLINK_ROUTE`, whose number is zero.
fn exchange(
    protocol: Option<rustix::net::Protocol>,
    request: &[u8],
) -> io::Result<Vec<(u16, Vec<u8>)>> {
    let socket = socket_with(
        AddressFamily::NETLINK,
        SocketType::RAW,
        SocketFlags::CLOEXEC,
        protocol,
    )?;
    rustix::net::sockopt::set_socket_timeout(
        &socket,
        rustix::net::sockopt::Timeout::Recv,
        Some(TIMEOUT),
    )?;
    sendto(
        &socket,
        request,
        SendFlags::empty(),
        &netlink::SocketAddrNetlink::new(0, 0),
    )?;
    let mut replies = Vec::new();
    let mut buffer = vec![0u8; 32 * 1024];
    loop {
        let (received, _) = recv(&socket, &mut buffer[..], RecvFlags::empty())?;
        let mut more = false;
        for (kind, flags, body) in messages(&buffer[..received.min(buffer.len())]) {
            match kind {
                NLMSG_DONE => return Ok(replies),
                NLMSG_ERROR => {
                    let code = u32_at(body, 0).map_or(0, |code| code as i32);
                    if code == 0 {
                        return Ok(replies);
                    }
                    return Err(io::Error::from_raw_os_error(-code));
                }
                _ => {
                    more |= flags & NLM_F_MULTI != 0;
                    replies.push((kind, body.to_vec()));
                }
            }
        }
        if !more {
            return Ok(replies);
        }
    }
}

// ---- CAN -------------------------------------------------------------------

fn can_state_name(state: u32) -> &'static str {
    match state {
        0 => "error-active",
        1 => "error-warning",
        2 => "error-passive",
        3 => "bus-off",
        4 => "stopped",
        5 => "sleeping",
        _ => "unknown",
    }
}

/// A CAN link as an `RTM_NEWLINK` body reports it: the controller's state,
/// bit timing and error counters, and the link's packet counters. `None`
/// when the link is not a CAN one.
#[must_use]
pub fn can_json(body: &[u8]) -> Option<Value> {
    let attrs = attributes(body.get(IFINFOMSG..)?);
    let info = attrs
        .iter()
        .find(|(kind, _)| *kind == IFLA_LINKINFO)
        .map(|(_, payload)| attributes(payload))?;
    let kind = info
        .iter()
        .find(|(kind, _)| *kind == IFLA_INFO_KIND)
        .map(|(_, payload)| payload.split(|byte| *byte == 0).next().unwrap_or_default())?;
    if kind != b"can" {
        return None;
    }
    let mut root = Map::new();
    root.insert("available".to_string(), json!(true));
    let data = info
        .iter()
        .find(|(kind, _)| *kind == IFLA_INFO_DATA)
        .map(|(_, payload)| attributes(payload))
        .unwrap_or_default();
    for (kind, payload) in &data {
        match *kind {
            IFLA_CAN_STATE => {
                if let Some(state) = u32_at(payload, 0) {
                    root.insert("busState".to_string(), json!(can_state_name(state)));
                }
            }
            IFLA_CAN_BITTIMING => {
                if let (Some(bitrate), Some(sample)) = (u32_at(payload, 0), u32_at(payload, 4)) {
                    root.insert("bitrate".to_string(), json!(bitrate));
                    root.insert("samplePoint".to_string(), json!(f64::from(sample) / 1000.0));
                }
            }
            IFLA_CAN_BERR_COUNTER => {
                if let (Some(tx), Some(rx)) = (u16_at(payload, 0), u16_at(payload, 2)) {
                    root.insert("txErrors".to_string(), json!(tx));
                    root.insert("rxErrors".to_string(), json!(rx));
                }
            }
            _ => {}
        }
    }
    // struct can_device_stats: bus_error, error_warning, error_passive,
    // bus_off, arbitration_lost, restarts.
    if let Some((_, stats)) = info.iter().find(|(kind, _)| *kind == IFLA_INFO_XSTATS) {
        for (key, at) in [
            ("warningCount", 4),
            ("errorPassiveCount", 8),
            ("busOffCount", 12),
        ] {
            if let Some(count) = u32_at(stats, at) {
                root.insert(key.to_string(), json!(count));
            }
        }
    }
    // struct rtnl_link_stats64: rx_packets, tx_packets, rx_bytes, tx_bytes,
    // rx_errors, tx_errors, rx_dropped, tx_dropped, multicast, collisions,
    // rx_length_errors, rx_over_errors, ...
    if let Some((_, stats)) = attrs.iter().find(|(kind, _)| *kind == IFLA_STATS64) {
        let at = |index: usize| u64_at(stats, index * 8);
        root.insert(
            "rx".to_string(),
            json!({ "packets": at(0), "dropped": at(6), "overruns": at(11), "errors": at(4) }),
        );
        root.insert(
            "tx".to_string(),
            json!({ "packets": at(1), "dropped": at(7), "errors": at(5) }),
        );
    }
    Some(Value::Object(root))
}

/// One CAN link's state, asked of the kernel by name.
pub fn can_link(interface: &str) -> io::Result<Option<Value>> {
    let mut body = vec![0u8; IFINFOMSG];
    body.extend(attribute(IFLA_IFNAME, &name_payload(interface)));
    let replies = exchange(None, &message(RTM_GETLINK, 0, &body))?;
    Ok(replies.iter().find_map(|(_, body)| can_json(body)))
}

// ---- IPv4 addresses --------------------------------------------------------

/// An `RTM_NEWADDR` body's IPv4 address: its interface, from the label
/// without an alias suffix, and the address with its prefix. `None` for
/// another family, a host-scope address, or a body without both.
pub fn ipv4_address(body: &[u8]) -> Option<(String, String)> {
    let header = body.get(..IFADDRMSG)?;
    if header[0] != AF_INET || header[3] == RT_SCOPE_HOST {
        return None;
    }
    let attributes = attributes(body.get(IFADDRMSG..)?);
    let local = attributes
        .iter()
        .find(|(kind, _)| *kind == IFA_LOCAL)
        .and_then(|(_, payload)| <[u8; 4]>::try_from(*payload).ok())?;
    let label = attributes
        .iter()
        .find(|(kind, _)| *kind == IFA_LABEL)
        .and_then(|(_, payload)| std::str::from_utf8(payload).ok())?
        .trim_end_matches('\0');
    let name = label.split(':').next().unwrap_or(label);
    Some((
        name.to_string(),
        format!("{}/{}", std::net::Ipv4Addr::from(local), header[1]),
    ))
}

/// The kernel's IPv4 addresses, the first of each interface, by name.
pub fn ipv4_addresses() -> io::Result<std::collections::BTreeMap<String, String>> {
    let mut body = vec![0u8; IFADDRMSG];
    body[0] = AF_INET;
    let replies = exchange(None, &message(RTM_GETADDR, NLM_F_DUMP, &body))?;
    let mut found = std::collections::BTreeMap::new();
    for (kind, body) in &replies {
        if *kind == RTM_NEWADDR
            && let Some((name, address)) = ipv4_address(body)
        {
            found.entry(name).or_insert(address);
        }
    }
    Ok(found)
}

// ---- WireGuard -------------------------------------------------------------

/// One peer as the kernel reports it. The reply also carries the device's
/// private key and each peer's pre-shared key; neither is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireguardPeerObservation {
    pub public_key: String,
    /// Unix seconds; `None` when the peer never completed a handshake.
    pub latest_handshake: Option<u64>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub persistent_keepalive: u16,
}

impl WireguardPeerObservation {
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "publicKey": self.public_key,
            "latestHandshake": self.latest_handshake,
            "rxBytes": self.rx_bytes,
            "txBytes": self.tx_bytes,
            "persistentKeepalive": self.persistent_keepalive,
        })
    }
}

/// The generic netlink family id in a `CTRL_CMD_GETFAMILY` reply body.
fn family_id(body: &[u8]) -> Option<u16> {
    attributes(body.get(GENLMSG..)?)
        .into_iter()
        .find(|(kind, _)| *kind == CTRL_ATTR_FAMILY_ID)
        .and_then(|(_, payload)| u16_at(payload, 0))
}

/// The peers in one `WG_CMD_GET_DEVICE` reply body. A device with many
/// peers is split over several replies, each carrying some of them.
#[must_use]
pub fn wireguard_peers(body: &[u8]) -> Vec<WireguardPeerObservation> {
    let Some(attrs) = body.get(GENLMSG..).map(attributes) else {
        return Vec::new();
    };
    attrs
        .into_iter()
        .filter(|(kind, _)| *kind == WGDEVICE_A_PEERS)
        .flat_map(|(_, peers)| attributes(peers))
        .filter_map(|(_, peer)| {
            let mut public_key = None;
            let mut observation = WireguardPeerObservation {
                public_key: String::new(),
                latest_handshake: None,
                rx_bytes: 0,
                tx_bytes: 0,
                persistent_keepalive: 0,
            };
            for (kind, payload) in attributes(peer) {
                match kind {
                    WGPEER_A_PUBLIC_KEY if payload.len() == 32 => {
                        public_key =
                            Some(base64::engine::general_purpose::STANDARD.encode(payload));
                    }
                    WGPEER_A_PERSISTENT_KEEPALIVE_INTERVAL => {
                        observation.persistent_keepalive = u16_at(payload, 0).unwrap_or(0);
                    }
                    // struct __kernel_timespec { s64 tv_sec; s64 tv_nsec; }
                    WGPEER_A_LAST_HANDSHAKE_TIME => {
                        observation.latest_handshake =
                            u64_at(payload, 0).filter(|seconds| *seconds > 0);
                    }
                    WGPEER_A_RX_BYTES => observation.rx_bytes = u64_at(payload, 0).unwrap_or(0),
                    WGPEER_A_TX_BYTES => observation.tx_bytes = u64_at(payload, 0).unwrap_or(0),
                    _ => {}
                }
            }
            observation.public_key = public_key?;
            Some(observation)
        })
        .collect()
}

/// A WireGuard device's peers, asked of the kernel by interface name.
pub fn wireguard_device(interface: &str) -> io::Result<Vec<WireguardPeerObservation>> {
    let mut body = vec![CTRL_CMD_GETFAMILY, 1, 0, 0];
    body.extend(attribute(
        CTRL_ATTR_FAMILY_NAME,
        &name_payload(WG_GENL_NAME),
    ));
    let family = exchange(Some(netlink::GENERIC), &message(GENL_ID_CTRL, 0, &body))?
        .iter()
        .find_map(|(_, body)| family_id(body))
        .ok_or_else(|| {
            io::Error::other("the wireguard generic netlink family is not registered")
        })?;
    let mut body = vec![WG_CMD_GET_DEVICE, WG_GENL_VERSION, 0, 0];
    body.extend(attribute(WGDEVICE_A_IFNAME, &name_payload(interface)));
    Ok(
        exchange(Some(netlink::GENERIC), &message(family, NLM_F_DUMP, &body))?
            .iter()
            .flat_map(|(_, body)| wireguard_peers(body))
            .collect(),
    )
}

#[cfg(test)]
mod tests;

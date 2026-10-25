//! SSDP reconciler: the device answers UPnP discovery on the LAN
//! (docs/mica-core.md section 3.14).
//!
//! A responder of micad's own, on either init, on 239.255.255.250:1900 over
//! IPv4: NOTIFY `ssdp:alive` at start, on a new hostname and every 900 s,
//! `ssdp:byebye` at stop, and an answer to an M-SEARCH for `ssdp:all`,
//! `upnp:rootdevice`, its device type or its UUID, sent only to a searcher
//! in a subnet of the interface the search arrived on, for at most 2
//! searches a second per searcher and 20 in all, at most 3 packets each. apid serves the description,
//! `/ssdp/device.xml`. It runs while apid serves the network and the switch
//! (`access.ssdp.enabled`, else the product's `SSDP=` line) is on; otherwise
//! there is no socket at all.

use std::collections::{BTreeSet, HashMap};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use micad_settings::{Settings, WebMode};
use serde_json::json;

use super::Reconciler;

/// The device type the description and the announcements name.
pub const DEVICE_TYPE: &str = "urn:micaos:device:mica:1";
/// The product line naming the default while `access.ssdp.enabled` is unset.
const PRODUCT_KEY: &str = "SSDP=";
/// The SSDP group and port.
const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const PORT: u16 = 1900;
/// How long an announcement holds.
const MAX_AGE: u64 = 1800;
/// How often the device announces itself again: half of [`MAX_AGE`].
const ANNOUNCE_EVERY: Duration = Duration::from_secs(900);
/// How often the interfaces are looked at for a new address to listen on.
const INTERFACES_EVERY: Duration = Duration::from_secs(10);
/// Where apid serves the description.
pub const DESCRIPTION_PATH: &str = "/ssdp/device.xml";
const SERVER: &str = "Linux UPnP/1.1 Mica/1";
/// Searches answered a second for one searcher, and in all.
const PER_SEARCHER: u32 = 2;
const IN_ALL: u32 = 20;
/// The largest search read; a larger one is not SSDP.
const MAX_PACKET: usize = 1500;

/// The device's names on the LAN: stable, derived from the machine id, and
/// neither the id itself nor the device code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Identity {
    /// `uuid:<uuid>`, the device's UDN.
    pub udn: String,
    /// The description's `serialNumber`: 16 hex digits.
    pub serial: String,
}

/// The identity of the device whose machine id is `machine_id`: each name is
/// an HMAC-SHA256 of its own label under the id, so neither gives the id
/// away nor the other.
pub(crate) fn identity(machine_id: &str) -> Identity {
    use aws_lc_rs::hmac;

    let key = hmac::Key::new(hmac::HMAC_SHA256, machine_id.trim().as_bytes());
    let udn = hmac::sign(&key, b"mica-ssdp-udn-v1");
    let serial = hmac::sign(&key, b"mica-ssdp-serial-v1");
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&udn.as_ref()[..16]);
    // An RFC 9562 version 8 UUID: custom bits, the version and variant set.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let uuid = hex(&bytes);
    Identity {
        udn: format!(
            "uuid:{}-{}-{}-{}-{}",
            &uuid[..8],
            &uuid[8..12],
            &uuid[12..16],
            &uuid[16..20],
            &uuid[20..]
        ),
        serial: hex(&serial.as_ref()[..8]),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the responder announces; a change re-announces the device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Announcement {
    pub udn: String,
    /// The hostname, the description's `friendlyName`.
    pub hostname: String,
    /// apid's HTTP port, where `LOCATION` points.
    pub http_port: u16,
}

impl Announcement {
    /// The `(NT or ST, USN)` pairs of the device: the root device, the
    /// device by UUID, the device by type.
    fn targets(&self) -> [(String, String); 3] {
        [
            (
                "upnp:rootdevice".to_string(),
                format!("{}::upnp:rootdevice", self.udn),
            ),
            (self.udn.clone(), self.udn.clone()),
            (
                DEVICE_TYPE.to_string(),
                format!("{}::{DEVICE_TYPE}", self.udn),
            ),
        ]
    }

    /// The pairs a search for `st` is answered with.
    fn answers(&self, st: &str) -> Vec<(String, String)> {
        self.targets()
            .into_iter()
            .filter(|(target, _)| st == "ssdp:all" || st == target)
            .collect()
    }

    fn location(&self, address: Ipv4Addr) -> String {
        format!("http://{address}:{}{DESCRIPTION_PATH}", self.http_port)
    }
}

/// The search target of `packet` when it is an SSDP M-SEARCH.
pub(crate) fn search_target(packet: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(packet).ok()?;
    let mut lines = text.split("\r\n");
    if lines.next()? != "M-SEARCH * HTTP/1.1" {
        return None;
    }
    let (mut man, mut st) = (None, None);
    for line in lines.take_while(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':')?;
        match name.trim().to_ascii_uppercase().as_str() {
            "MAN" => man = Some(value.trim()),
            "ST" => st = Some(value.trim()),
            _ => {}
        }
    }
    (man? == "\"ssdp:discover\"").then(|| st.map(str::to_string))?
}

/// The answer to a search, unicast to the searcher.
pub(crate) fn response(
    announcement: &Announcement,
    st: &str,
    usn: &str,
    address: Ipv4Addr,
    boot_id: u64,
    date: &str,
) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age={MAX_AGE}\r\nDATE: {date}\r\nEXT:\r\nLOCATION: {}\r\nSERVER: {SERVER}\r\nST: {st}\r\nUSN: {usn}\r\nBOOTID.UPNP.ORG: {boot_id}\r\nCONFIGID.UPNP.ORG: 1\r\n\r\n",
        announcement.location(address)
    )
}

/// An announcement, multicast on one interface: `alive` with where the
/// description is, `byebye` without.
pub(crate) fn notify(
    announcement: &Announcement,
    nt: &str,
    usn: &str,
    alive: Option<Ipv4Addr>,
    boot_id: u64,
) -> String {
    match alive {
        Some(address) => format!(
            "NOTIFY * HTTP/1.1\r\nHOST: {GROUP}:{PORT}\r\nCACHE-CONTROL: max-age={MAX_AGE}\r\nLOCATION: {}\r\nNT: {nt}\r\nNTS: ssdp:alive\r\nSERVER: {SERVER}\r\nUSN: {usn}\r\nBOOTID.UPNP.ORG: {boot_id}\r\nCONFIGID.UPNP.ORG: 1\r\n\r\n",
            announcement.location(address)
        ),
        None => format!(
            "NOTIFY * HTTP/1.1\r\nHOST: {GROUP}:{PORT}\r\nNT: {nt}\r\nNTS: ssdp:byebye\r\nUSN: {usn}\r\nBOOTID.UPNP.ORG: {boot_id}\r\nCONFIGID.UPNP.ORG: 1\r\n\r\n"
        ),
    }
}

/// One IPv4 address of an interface the responder serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Served {
    pub index: u32,
    pub address: Ipv4Addr,
    pub netmask: Ipv4Addr,
}

impl Served {
    fn holds(&self, source: Ipv4Addr) -> bool {
        let mask = u32::from(self.netmask);
        u32::from(self.address) & mask == u32::from(source) & mask
    }
}

/// The address of interface `index` whose subnet holds `source`, if any:
/// the only kind of searcher answered.
pub(crate) fn on_link(served: &[Served], index: u32, source: Ipv4Addr) -> Option<Ipv4Addr> {
    served
        .iter()
        .find(|s| s.index == index && s.holds(source))
        .map(|s| s.address)
}

/// The searches answered a second, per searcher and in all.
#[derive(Debug, Default)]
pub(crate) struct RateLimit {
    second: Option<Instant>,
    total: u32,
    by_searcher: HashMap<Ipv4Addr, u32>,
}

impl RateLimit {
    /// Whether answering one more search of `searcher` at `now` is within
    /// the limits.
    pub(crate) fn allow(&mut self, searcher: Ipv4Addr, now: Instant) -> bool {
        if self
            .second
            .is_none_or(|start| now.duration_since(start) >= Duration::from_secs(1))
        {
            self.second = Some(now);
            self.total = 0;
            self.by_searcher.clear();
        }
        let sent = self.by_searcher.entry(searcher).or_default();
        if self.total >= IN_ALL || *sent >= PER_SEARCHER {
            return false;
        }
        *sent += 1;
        self.total += 1;
        true
    }
}

/// A running responder. Dropped, it says byebye and closes its socket.
pub(crate) trait Responder: Send {
    /// Announce `announcement` from now on.
    fn update(&mut self, announcement: &Announcement) -> Result<()>;
}

/// How a responder is started: a socket on the device, a recorder in tests.
pub(crate) trait StartResponder: Send + Sync {
    fn start(&self, announcement: &Announcement) -> Result<Box<dyn Responder>>;
}

/// What the product says: its name, and whether SSDP runs by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Product {
    pub name: Option<String>,
    pub default_on: bool,
}

/// The responder while it runs, and what it announces.
struct Running {
    responder: Box<dyn Responder>,
    announcement: Announcement,
}

/// Reconciler for the `access.ssdp` settings subtree.
pub struct SsdpReconciler {
    product: Product,
    identity: Option<Identity>,
    start: Box<dyn StartResponder>,
    running: Mutex<Option<Running>>,
}

impl SsdpReconciler {
    pub(crate) fn new(
        product: Product,
        identity: Option<Identity>,
        start: Box<dyn StartResponder>,
    ) -> Self {
        Self {
            product,
            identity,
            start,
            running: Mutex::new(None),
        }
    }

    /// The responder on this device's interfaces, for this root's product
    /// and machine id.
    pub fn production() -> Self {
        let conf = std::fs::read_to_string(micad_settings::PRODUCT_FILE).unwrap_or_default();
        let identity = std::fs::read_to_string("/etc/machine-id")
            .ok()
            .filter(|id| crate::system_info::is_machine_id(id))
            .map(|id| identity(&id));
        Self::new(
            Product {
                name: micad_settings::product_name(&conf).map(str::to_string),
                default_on: micad_settings::product_switch(&conf, PRODUCT_KEY),
            },
            identity,
            Box::new(Udp),
        )
    }
}

#[async_trait::async_trait]
impl Reconciler for SsdpReconciler {
    fn name(&self) -> &'static str {
        "ssdp"
    }

    fn subtree(&self) -> &'static str {
        "access.ssdp"
    }

    fn reads(&self) -> &'static [&'static str] {
        // The hostname is the friendly name; the console's mode and ports
        // decide whether there is a description to point at, and where.
        &["hostname", "access.web"]
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let enabled = settings.access.ssdp.effective(self.product.default_on);
        let source = if settings.access.ssdp.enabled.is_some() {
            "setting"
        } else {
            "product"
        };
        let web = &settings.access.web;
        let serving = web.mode == WebMode::Network;
        let mut running = self
            .running
            .lock()
            .map_err(|_| anyhow!("ssdp state poisoned"))?;
        let mut state = json!({
            "enabled": enabled,
            "source": source,
            "active": false,
            "friendlyName": settings.hostname,
            "manufacturer": "Mica",
            "modelName": self.product.name,
            "deviceType": DEVICE_TYPE,
            "console": {
                "scheme": if web.https_enabled { "https" } else { "http" },
                "port": if web.https_enabled { web.https_port } else { web.http_port },
            },
        });
        let identity = match (&self.identity, enabled, serving) {
            (_, false, _) => None,
            (_, true, false) => {
                state["reason"] = json!("the console is not on the network");
                None
            }
            (None, true, true) => {
                state["reason"] = json!("this device has no machine id");
                None
            }
            (Some(identity), true, true) => Some(identity),
        };
        let Some(identity) = identity else {
            // Off is no responder at all; dropping it says byebye.
            running.take();
            return Ok(state);
        };

        let announcement = Announcement {
            udn: identity.udn.clone(),
            hostname: settings.hostname.clone(),
            http_port: web.http_port,
        };
        match running.as_mut() {
            Some(current) if current.announcement == announcement => {}
            Some(current) => {
                current.responder.update(&announcement)?;
                current.announcement = announcement;
            }
            None => {
                *running = Some(Running {
                    responder: self
                        .start
                        .start(&announcement)
                        .context("start the SSDP responder")?,
                    announcement,
                });
            }
        }
        state["active"] = json!(true);
        state["udn"] = json!(identity.udn);
        state["serialNumber"] = json!(identity.serial);
        Ok(state)
    }
}

/// The production responder: a UDP socket on 1900, served by a thread.
struct Udp;

impl StartResponder for Udp {
    fn start(&self, announcement: &Announcement) -> Result<Box<dyn Responder>> {
        use socket2::{Domain, SockAddr};

        let socket = socket_pktinfo::PktInfoUdpSocket::new(Domain::IPV4)?;
        socket.set_reuse_address(true)?;
        socket.bind(&SockAddr::from(SocketAddr::from((
            Ipv4Addr::UNSPECIFIED,
            PORT,
        ))))?;
        socket.set_multicast_loop_v4(false)?;
        socket.set_multicast_ttl_v4(2)?;
        socket
            .try_clone_std()?
            .set_read_timeout(Some(Duration::from_secs(1)))?;
        let shared = Arc::new(Mutex::new(announcement.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
            std::thread::Builder::new()
                .name("ssdp".to_string())
                .spawn(move || serve(&socket, &shared, &stop))?
        };
        Ok(Box::new(Thread {
            shared,
            stop,
            thread: Some(thread),
        }))
    }
}

struct Thread {
    shared: Arc<Mutex<Announcement>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Responder for Thread {
    fn update(&mut self, announcement: &Announcement) -> Result<()> {
        *self
            .shared
            .lock()
            .map_err(|_| anyhow!("ssdp announcement poisoned"))? = announcement.clone();
        Ok(())
    }
}

impl Drop for Thread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // The thread wakes within its read timeout, says byebye and ends.
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("ssdp: the responder thread panicked");
        }
    }
}

/// The IPv4 addresses of the interfaces the responder serves: every one
/// mDNS serves ([`super::mdns`]'s silent prefixes left out).
fn served() -> Vec<Served> {
    let Ok(interfaces) = if_addrs::get_if_addrs() else {
        return Vec::new();
    };
    interfaces
        .into_iter()
        .filter(|intf| {
            !intf.is_loopback()
                && !super::mdns::SILENT_PREFIXES
                    .iter()
                    .any(|prefix| intf.name.starts_with(prefix))
        })
        .filter_map(|intf| match (intf.addr, intf.index) {
            (if_addrs::IfAddr::V4(v4), Some(index)) => Some(Served {
                index,
                address: v4.ip,
                netmask: v4.netmask,
            }),
            _ => None,
        })
        .collect()
}

/// Announce, answer and say goodbye until `stop`.
fn serve(
    socket: &socket_pktinfo::PktInfoUdpSocket,
    shared: &Mutex<Announcement>,
    stop: &AtomicBool,
) {
    let send = |packet: &str, to: SocketAddr| {
        if let Err(err) = socket.send_to(packet.as_bytes(), &socket2::SockAddr::from(to)) {
            tracing::debug!(%to, "ssdp: send: {err}");
        }
    };
    let announce = |announcement: &Announcement, served: &[Served], alive: bool, boot_id: u64| {
        for interface in served {
            if socket.set_multicast_if_v4(&interface.address).is_err() {
                continue;
            }
            // Twice, as UDP may lose one.
            for _ in 0..2 {
                for (nt, usn) in announcement.targets() {
                    let at = alive.then_some(interface.address);
                    send(
                        &notify(announcement, &nt, &usn, at, boot_id),
                        SocketAddr::from((GROUP, PORT)),
                    );
                }
            }
        }
    };
    let mut boot_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |d| d.as_secs());
    let mut joined = BTreeSet::new();
    // The addresses of `interfaces` newly listened on.
    let mut join = |interfaces: &[Served]| -> Vec<Served> {
        interfaces
            .iter()
            .filter(|interface| {
                !joined.contains(&interface.address)
                    && socket.join_multicast_v4(&GROUP, &interface.address).is_ok()
                    && joined.insert(interface.address)
            })
            .copied()
            .collect()
    };
    let mut interfaces = served();
    join(&interfaces);
    let mut current = shared
        .lock()
        .map_or_else(|e| e.into_inner().clone(), |a| a.clone());
    announce(&current, &interfaces, true, boot_id);
    let (mut announced, mut looked) = (Instant::now(), Instant::now());
    let mut limit = RateLimit::default();
    let mut buf = [0u8; MAX_PACKET];
    while !stop.load(Ordering::Acquire) {
        let next = shared
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |a| a.clone());
        if next != current {
            announce(&current, &interfaces, false, boot_id);
            boot_id += 1;
            current = next;
            announce(&current, &interfaces, true, boot_id);
            announced = Instant::now();
        }
        if looked.elapsed() >= INTERFACES_EVERY {
            interfaces = served();
            // A new address is announced on at once.
            announce(&current, &join(&interfaces), true, boot_id);
            looked = Instant::now();
        }
        if announced.elapsed() >= ANNOUNCE_EVERY {
            announce(&current, &interfaces, true, boot_id);
            announced = Instant::now();
        }
        let (len, info) = match socket.recv(&mut buf) {
            Ok(received) => received,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(err) => {
                tracing::warn!("ssdp: receive: {err}");
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let (SocketAddr::V4(searcher), Ok(index)) = (info.addr_src, u32::try_from(info.if_index))
        else {
            continue;
        };
        let Some(st) = search_target(&buf[..len]) else {
            continue;
        };
        // A searcher off the link is never answered: no reflection, and no
        // answer to a spoofed source elsewhere.
        let Some(address) = on_link(&interfaces, index, *searcher.ip()) else {
            continue;
        };
        if !limit.allow(*searcher.ip(), Instant::now()) {
            continue;
        }
        let date = chrono::Utc::now()
            .format("%a, %d %b %Y %H:%M:%S GMT")
            .to_string();
        for (st, usn) in current.answers(&st) {
            send(
                &response(&current, &st, &usn, address, boot_id, &date),
                SocketAddr::V4(searcher),
            );
        }
    }
    announce(&current, &interfaces, false, boot_id);
}

#[cfg(test)]
mod tests;

//! The probes of a reachability round, each out of one interface: bound to it
//! with `SO_BINDTODEVICE` and to its address as the source, so the kernel
//! takes the route out of that interface whichever default route is active.
//!
//! - `icmp`: one echo request over a raw ICMP socket, answered by the echo
//!   reply that carries its identifier and sequence from the target.
//! - `tcp`: a connect to the port, closed at once. A refusal answers too: a
//!   reset from the target proves the path as a handshake does.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU16, Ordering};
use std::time::{Duration, Instant};

use micad_settings::ProbeTarget;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

/// A target with its address known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Icmp(Ipv4Addr),
    Tcp(SocketAddrV4),
}

impl Target {
    /// `target` with `gateway` for the uplink's own router.
    ///
    /// # Errors
    /// `gateway` is named and the uplink has none, or the address does not
    /// parse.
    pub fn of(target: &ProbeTarget, gateway: Option<Ipv4Addr>) -> Result<Self, String> {
        let address = if target.address == "gateway" {
            gateway.ok_or_else(|| "the uplink has no gateway".to_string())?
        } else {
            target
                .address
                .parse()
                .map_err(|_| format!("{:?} is not an IPv4 address", target.address))?
        };
        match target.method.as_str() {
            "icmp" => Ok(Self::Icmp(address)),
            "tcp" => Ok(Self::Tcp(SocketAddrV4::new(
                address,
                target.port.unwrap_or(ProbeTarget::TCP_PORT),
            ))),
            method => Err(format!("{method:?} is not a probe method")),
        }
    }
}

/// Probe `target` out of `iface` from `source` within `timeout`: how long it
/// took to answer, or why it did not.
///
/// # Errors
/// No answer in time, or the socket could not be made.
pub fn run(
    iface: &str,
    source: Ipv4Addr,
    target: Target,
    timeout: Duration,
) -> Result<Duration, String> {
    match target {
        Target::Icmp(address) => echo(iface, source, address, timeout),
        Target::Tcp(address) => connect(iface, source, address, timeout),
    }
}

fn bound(iface: &str, source: Ipv4Addr, kind: Type, protocol: Protocol) -> Result<Socket, String> {
    let socket =
        Socket::new(Domain::IPV4, kind, Some(protocol)).map_err(|err| format!("socket: {err}"))?;
    socket
        .bind_device(Some(iface.as_bytes()))
        .map_err(|err| format!("bind to {iface}: {err}"))?;
    socket
        .bind(&SockAddr::from(SocketAddrV4::new(source, 0)))
        .map_err(|err| format!("bind to {source}: {err}"))?;
    Ok(socket)
}

fn connect(
    iface: &str,
    source: Ipv4Addr,
    address: SocketAddrV4,
    timeout: Duration,
) -> Result<Duration, String> {
    let socket = bound(iface, source, Type::STREAM, Protocol::TCP)?;
    let started = Instant::now();
    match socket.connect_timeout(&SockAddr::from(SocketAddr::V4(address)), timeout) {
        Ok(()) => Ok(started.elapsed()),
        Err(err) if err.kind() == std::io::ErrorKind::ConnectionRefused => Ok(started.elapsed()),
        Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {
            Err(format!("no answer within {} ms", timeout.as_millis()))
        }
        Err(err) => Err(err.to_string()),
    }
}

/// The identifier of this process's echoes; each echo takes the next
/// sequence number.
static SEQUENCE: AtomicU16 = AtomicU16::new(1);

/// An ICMP echo request: type 8, code 0, the checksum, `id`, `sequence` and
/// eight bytes of payload.
#[must_use]
pub fn echo_request(id: u16, sequence: u16) -> [u8; 16] {
    let mut packet = [0u8; 16];
    packet[0] = 8;
    packet[4..6].copy_from_slice(&id.to_be_bytes());
    packet[6..8].copy_from_slice(&sequence.to_be_bytes());
    packet[8..].copy_from_slice(b"mica-up\0");
    let sum = checksum(&packet);
    packet[2..4].copy_from_slice(&sum.to_be_bytes());
    packet
}

/// The Internet checksum of `bytes`.
#[must_use]
pub fn checksum(bytes: &[u8]) -> u16 {
    let mut sum: u32 = bytes
        .chunks(2)
        .map(|pair| u32::from(u16::from_be_bytes([pair[0], *pair.get(1).unwrap_or(&0)])))
        .sum();
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !u16::try_from(sum).unwrap_or(u16::MAX)
}

/// Whether `datagram`, as a raw socket gives it (the IPv4 header first), is
/// the echo reply to `id` and `sequence` from `from`.
#[must_use]
pub fn is_reply(datagram: &[u8], from: Ipv4Addr, id: u16, sequence: u16) -> bool {
    let Some(&first) = datagram.first() else {
        return false;
    };
    let header = usize::from(first & 0x0f) * 4;
    let (Some(source), Some(icmp)) = (datagram.get(12..16), datagram.get(header..header + 8))
    else {
        return false;
    };
    source == from.octets()
        && icmp[0] == 0
        && icmp[1] == 0
        && icmp[4..6] == id.to_be_bytes()
        && icmp[6..8] == sequence.to_be_bytes()
}

fn echo(
    iface: &str,
    source: Ipv4Addr,
    address: Ipv4Addr,
    timeout: Duration,
) -> Result<Duration, String> {
    let socket = bound(iface, source, Type::RAW, Protocol::ICMPV4)?;
    let id = u16::try_from(std::process::id() & 0xffff).unwrap_or(0);
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    socket
        .send_to(
            &echo_request(id, sequence),
            &SockAddr::from(SocketAddrV4::new(address, 0)),
        )
        .map_err(|err| format!("send: {err}"))?;
    let mut buffer = [0u8; 1500];
    loop {
        let left = timeout.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(format!("no answer within {} ms", timeout.as_millis()));
        }
        socket
            .set_read_timeout(Some(left))
            .map_err(|err| err.to_string())?;
        match std::io::Read::read(&mut &socket, &mut buffer) {
            Ok(length) => {
                if is_reply(&buffer[..length], address, id, sequence) {
                    return Ok(started.elapsed());
                }
            }
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(format!("no answer within {} ms", timeout.as_millis()));
            }
            Err(err) => return Err(err.to_string()),
        }
    }
}

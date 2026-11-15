//! The routes the uplink engine reads and writes: the kernel's IPv4 default
//! routes in the main table, micad's failover route (protocol
//! [`RTPROT_MICA`], metric [`OVERRIDE_METRIC`]), and a socket that wakes the
//! engine on each route, address or link change.
//!
//! The builders and the parser take and give bytes, so they are tested
//! without a kernel.

use std::io;
use std::net::Ipv4Addr;
use std::os::fd::OwnedFd;
use std::time::Duration;

use rustix::net::{
    AddressFamily, RecvFlags, SocketFlags, SocketType, bind, netlink, recv, socket_with,
};

use super::tunnel::{
    NLM_F_ACK, NLM_F_CREATE, NLM_F_REPLACE, RT_SCOPE_LINK, RT_SCOPE_UNIVERSE, RT_TABLE_MAIN,
    RTA_PRIORITY, RTA_TABLE, RTM_DELROUTE, RTN_UNICAST, RTPROT_MICA,
};
use super::{
    AF_INET, NLM_F_DUMP, RTA_GATEWAY, RTA_OIF, RTM_GETROUTE, RTM_NEWROUTE, RTMSG, attribute,
    attributes, exchange, message, u32_at,
};

/// The metric of micad's failover route: above every uplink's own.
pub const OVERRIDE_METRIC: u32 = micad_settings::OVERRIDE_METRIC;

/// `RTMGRP_LINK`, `RTMGRP_IPV4_IFADDR` and `RTMGRP_IPV4_ROUTE`.
const EVENT_GROUPS: u32 = 0x1 | 0x10 | 0x40;

/// One IPv4 default route of the main table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultRoute {
    /// The link it leaves by.
    pub link: u32,
    /// Its next hop; none on a point-to-point link.
    pub gateway: Option<Ipv4Addr>,
    pub metric: u32,
    /// Who added it: [`RTPROT_MICA`] is micad's failover route.
    pub protocol: u8,
}

impl DefaultRoute {
    /// Whether this is micad's failover route.
    #[must_use]
    pub fn is_override(&self) -> bool {
        self.protocol == RTPROT_MICA
    }
}

/// The route in an `RTM_NEWROUTE` body, when it is an IPv4 unicast default
/// route of the main table out of one link.
pub(super) fn default_route(body: &[u8]) -> Option<DefaultRoute> {
    let header = body.get(..RTMSG)?;
    let (family, length, protocol, kind) = (header[0], header[1], header[5], header[7]);
    if family != AF_INET || length != 0 || kind != RTN_UNICAST {
        return None;
    }
    let mut table = u32::from(header[4]);
    let (mut link, mut gateway, mut metric) = (None, None, 0);
    for (attr, payload) in attributes(body.get(RTMSG..)?) {
        match attr {
            RTA_TABLE => table = u32_at(payload, 0)?,
            RTA_OIF => link = u32_at(payload, 0),
            RTA_GATEWAY => {
                gateway = Some(Ipv4Addr::from(<[u8; 4]>::try_from(payload).ok()?));
            }
            RTA_PRIORITY => metric = u32_at(payload, 0)?,
            _ => {}
        }
    }
    (table == u32::from(RT_TABLE_MAIN)).then_some(())?;
    Some(DefaultRoute {
        link: link?,
        gateway,
        metric,
        protocol,
    })
}

/// The dump of every IPv4 route.
pub(super) fn dump_request() -> Vec<u8> {
    let mut body = vec![0u8; RTMSG];
    body[0] = AF_INET;
    message(RTM_GETROUTE, NLM_F_DUMP, &body)
}

/// `kind` (`RTM_NEWROUTE` or `RTM_DELROUTE`) of the failover route out of
/// `link` via `gateway`. A new one replaces the one there, so the route moves
/// from one uplink to another in one step.
pub(super) fn override_request(kind: u16, link: u32, gateway: Option<Ipv4Addr>) -> Vec<u8> {
    let scope = if gateway.is_some() {
        RT_SCOPE_UNIVERSE
    } else {
        RT_SCOPE_LINK
    };
    let mut body = vec![
        AF_INET,
        0,
        0,
        0,
        RT_TABLE_MAIN,
        RTPROT_MICA,
        scope,
        RTN_UNICAST,
    ];
    body.extend(0u32.to_ne_bytes());
    body.extend(attribute(RTA_OIF, &link.to_ne_bytes()));
    if let Some(gateway) = gateway {
        body.extend(attribute(RTA_GATEWAY, &gateway.octets()));
    }
    body.extend(attribute(RTA_PRIORITY, &OVERRIDE_METRIC.to_ne_bytes()));
    let flags = if kind == RTM_NEWROUTE {
        NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE
    } else {
        NLM_F_ACK
    };
    message(kind, flags, &body)
}

/// Every IPv4 default route of the main table.
pub fn default_routes() -> io::Result<Vec<DefaultRoute>> {
    Ok(exchange(None, &dump_request())?
        .iter()
        .filter(|(kind, _)| *kind == RTM_NEWROUTE)
        .filter_map(|(_, body)| default_route(body))
        .collect())
}

/// Put the failover route out of `link` via `gateway`, in place of the one
/// there.
pub fn set_override(link: u32, gateway: Option<Ipv4Addr>) -> io::Result<()> {
    exchange(None, &override_request(RTM_NEWROUTE, link, gateway)).map(drop)
}

/// Remove the failover route `route`.
pub fn delete_override(route: &DefaultRoute) -> io::Result<()> {
    exchange(
        None,
        &override_request(RTM_DELROUTE, route.link, route.gateway),
    )
    .map(drop)
}

/// A socket on the route, address and link events.
pub struct Events(OwnedFd);

impl Events {
    pub fn open() -> io::Result<Self> {
        let socket = socket_with(
            AddressFamily::NETLINK,
            SocketType::RAW,
            SocketFlags::CLOEXEC,
            None,
        )?;
        bind(&socket, &netlink::SocketAddrNetlink::new(0, EVENT_GROUPS))?;
        Ok(Self(socket))
    }

    /// Wait up to `timeout` for events; true when any came. Every event that
    /// is queued is read, so one wake covers a burst.
    pub fn wait(&self, timeout: Duration) -> io::Result<bool> {
        rustix::net::sockopt::set_socket_timeout(
            &self.0,
            rustix::net::sockopt::Timeout::Recv,
            Some(timeout),
        )?;
        let mut buffer = vec![0u8; 32 * 1024];
        match recv(&self.0, &mut buffer[..], RecvFlags::empty()) {
            Ok(_) => {}
            Err(rustix::io::Errno::AGAIN) => return Ok(false),
            Err(err) => return Err(err.into()),
        }
        while recv(&self.0, &mut buffer[..], RecvFlags::DONTWAIT).is_ok() {}
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The failover route as the kernel takes it: main table, protocol 109,
    // universe scope via a gateway (link scope without one), unicast, out of
    // the link, metric 10; a new one replaces, a removal only asks for the
    // acknowledgement.
    #[test]
    fn the_override_route_is_main_table_protocol_109_metric_10() {
        let request = override_request(RTM_NEWROUTE, 3, Some(Ipv4Addr::new(192, 0, 2, 1)));
        let flags = u16::from_ne_bytes([request[6], request[7]]);
        assert_eq!(flags, 0x1 | NLM_F_ACK | NLM_F_CREATE | NLM_F_REPLACE);
        assert_eq!(u16::from_ne_bytes([request[4], request[5]]), RTM_NEWROUTE);
        let body = &request[16..];
        assert_eq!(&body[..8], &[AF_INET, 0, 0, 0, 254, 109, 0, 1]);
        assert_eq!(
            attributes(&body[RTMSG..]),
            [
                (RTA_OIF, &3u32.to_ne_bytes()[..]),
                (RTA_GATEWAY, &[192, 0, 2, 1][..]),
                (RTA_PRIORITY, &10u32.to_ne_bytes()[..]),
            ]
        );
        let delete = override_request(RTM_DELROUTE, 3, None);
        assert_eq!(u16::from_ne_bytes([delete[6], delete[7]]), 0x1 | NLM_F_ACK);
        assert_eq!(&delete[16..24], &[AF_INET, 0, 0, 0, 254, 109, 253, 1]);
        assert_eq!(attributes(&delete[16 + RTMSG..]).len(), 2);
    }

    fn route_body(
        family: u8,
        length: u8,
        table: u8,
        protocol: u8,
        attrs: &[(u16, Vec<u8>)],
    ) -> Vec<u8> {
        let mut body = vec![family, length, 0, 0, table, protocol, 0, RTN_UNICAST];
        body.extend(0u32.to_ne_bytes());
        for (kind, payload) in attrs {
            body.extend(attribute(*kind, payload));
        }
        body
    }

    // Only an IPv4 main-table default route out of a link is read, with its
    // gateway, metric and protocol.
    #[test]
    fn a_default_route_is_read_from_its_dump_body() {
        let attrs = vec![
            (RTA_OIF, 2u32.to_ne_bytes().to_vec()),
            (RTA_GATEWAY, vec![10, 0, 0, 1]),
            (RTA_PRIORITY, 100u32.to_ne_bytes().to_vec()),
        ];
        assert_eq!(
            default_route(&route_body(AF_INET, 0, 254, 16, &attrs)),
            Some(DefaultRoute {
                link: 2,
                gateway: Some(Ipv4Addr::new(10, 0, 0, 1)),
                metric: 100,
                protocol: 16,
            })
        );
        assert!(
            default_route(&route_body(AF_INET, 0, 254, 109, &attrs))
                .unwrap()
                .is_override()
        );
        assert_eq!(
            default_route(&route_body(AF_INET, 24, 254, 16, &attrs)),
            None
        );
        assert_eq!(
            default_route(&route_body(AF_INET, 0, 255, 16, &attrs)),
            None
        );
        assert_eq!(default_route(&route_body(10, 0, 254, 16, &attrs)), None);
        assert_eq!(
            default_route(&route_body(AF_INET, 0, 254, 16, &attrs[1..])),
            None
        );
        let dump = dump_request();
        assert_eq!(u16::from_ne_bytes([dump[4], dump[5]]), RTM_GETROUTE);
        assert_eq!(dump[16], AF_INET);
    }
}

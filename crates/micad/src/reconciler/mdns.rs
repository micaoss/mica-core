//! mDNS reconciler: the device announces itself on the LAN (docs/mica-core.md
//! section 3.13).
//!
//! A responder on mdns-sd, on either init, answers `<hostname>.local` with the
//! addresses of the interface a query arrives on, on every interface but the
//! loopback, the container engines' links, WireGuard and CAN, and publishes
//! DNS-SD services: `_mica._tcp` always, the console while apid serves the
//! network, SSH while it is on. A query from off the link is not answered.
//! Switched off (`access.mdns.enabled`, else the product's `MDNS=` line),
//! there is no socket at all.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow};
use micad_settings::Settings;
use serde_json::json;

use super::Reconciler;

/// Interfaces the responder leaves out by name prefix: the loopback, the
/// virtual ends the container engines make (the ones the login prompt also
/// leaves out), WireGuard, which carries no multicast, and CAN.
const SILENT_PREFIXES: &[&str] = &[
    "lo", "veth", "podman", "cni-", "docker", "dummy", "wg", "can", "vcan",
];

/// The product line naming the default while `access.mdns.enabled` is unset.
const PRODUCT_KEY: &str = "MDNS=";

/// The service type every Mica device publishes.
pub(crate) const MICA_TYPE: &str = "_mica._tcp";

/// One DNS-SD service as it is published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Service {
    /// The instance label, such as the hostname.
    pub instance: String,
    /// The service type, `_<name>._tcp` or `_<name>._udp`.
    pub kind: String,
    pub port: u16,
    pub txt: BTreeMap<String, String>,
    /// The addresses whose interfaces it is published on; every interface
    /// when empty.
    pub addresses: Vec<IpAddr>,
    /// Who asked for it: `built-in` here.
    pub source: String,
}

impl Service {
    /// The service's DNS name, `<instance>.<type>.local.`.
    pub(crate) fn fullname(&self) -> String {
        format!("{}.{}.local.", self.instance, self.kind)
    }

    fn json(&self) -> serde_json::Value {
        json!({
            "instance": self.instance,
            "type": self.kind,
            "port": self.port,
            "txt": self.txt,
            "addresses": self.addresses.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "source": self.source,
        })
    }
}

/// A running responder: publishes and withdraws services for one hostname.
/// Dropped, it says goodbye for what it still publishes and closes its socket.
pub(crate) trait Responder: Send {
    /// Publish `service` under `<hostname>.local`.
    fn publish(&mut self, hostname: &str, service: &Service) -> Result<()>;
    /// Withdraw the service named `fullname`, with a goodbye.
    fn withdraw(&mut self, fullname: &str) -> Result<()>;
}

/// How a responder is started: mdns-sd on the device, a recorder in tests.
pub(crate) trait StartResponder: Send + Sync {
    fn start(&self) -> Result<Box<dyn Responder>>;
}

/// What the product says: its name, and whether discovery runs by default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Product {
    pub name: Option<String>,
    pub default_on: bool,
}

impl Product {
    /// The product of `/usr/lib/mica/product.conf`; a root without it is a
    /// product with no name, discovery on.
    fn production() -> Self {
        let conf = std::fs::read_to_string(micad_settings::PRODUCT_FILE).unwrap_or_default();
        Self {
            name: micad_settings::product_name(&conf).map(str::to_string),
            default_on: micad_settings::product_switch(&conf, PRODUCT_KEY),
        }
    }
}

/// The built-in services `settings` asks for on a device of `product`.
pub(crate) fn built_in_services(settings: &Settings, product: &Product) -> Vec<Service> {
    let hostname = &settings.hostname;
    let built_in = |instance: &str, kind: &str, port: u16, txt: &[(&str, &str)]| Service {
        instance: instance.to_string(),
        kind: kind.to_string(),
        port,
        txt: txt
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        addresses: Vec::new(),
        source: "built-in".to_string(),
    };
    let web = &settings.access.web;
    let console = (web.mode == micad_settings::WebMode::Network).then_some(if web.https_enabled {
        ("_https._tcp", web.https_port)
    } else {
        ("_http._tcp", web.http_port)
    });

    let mut mica_txt = vec![("txtvers", "1")];
    if let Some(name) = &product.name {
        mica_txt.push(("product", name));
    }
    mica_txt.push(("hostname", hostname));
    let mut services = vec![built_in(
        hostname,
        MICA_TYPE,
        console.map_or(0, |(_, port)| port),
        &mica_txt,
    )];
    if let Some((kind, port)) = console {
        services.push(built_in(hostname, kind, port, &[("path", "/")]));
    }
    let ssh = &settings.access.ssh;
    if ssh.enabled {
        let mut service = built_in(hostname, "_ssh._tcp", ssh.port, &[]);
        // Listening on chosen addresses, SSH is announced on their
        // interfaces. An entry with a port of its own is not on `port`, so
        // it is left out, and with only such entries SSH is not announced.
        if ssh.listen_addresses.is_empty() {
            services.push(service);
        } else {
            service.addresses = ssh
                .listen_addresses
                .iter()
                .filter_map(|entry| entry.parse::<IpAddr>().ok())
                .collect();
            if !service.addresses.is_empty() {
                services.push(service);
            }
        }
    }
    services
}

/// The responder while it runs, and what it publishes.
struct Running {
    responder: Box<dyn Responder>,
    hostname: String,
    published: BTreeMap<String, Service>,
}

impl Running {
    /// Withdraw what is published and stop.
    fn stop(mut self) {
        for fullname in self.published.keys() {
            if let Err(err) = self.responder.withdraw(fullname) {
                tracing::warn!(service = %fullname, "mdns: withdraw: {err:#}");
            }
        }
    }
}

/// Reconciler for the `access.mdns` settings subtree.
pub struct MdnsReconciler {
    product: Product,
    start: Box<dyn StartResponder>,
    running: Mutex<Option<Running>>,
}

impl MdnsReconciler {
    pub(crate) fn new(product: Product, start: Box<dyn StartResponder>) -> Self {
        Self {
            product,
            start,
            running: Mutex::new(None),
        }
    }

    /// The responder on this device's interfaces, for this root's product.
    pub fn production() -> Self {
        Self::new(Product::production(), Box::new(MdnsSd))
    }
}

#[async_trait::async_trait]
impl Reconciler for MdnsReconciler {
    fn name(&self) -> &'static str {
        "mdns"
    }

    fn subtree(&self) -> &'static str {
        "access.mdns"
    }

    fn reads(&self) -> &'static [&'static str] {
        // The hostname names every record, the console's mode and ports and
        // SSH's switch and port decide what is published.
        &["hostname", "access.web", "access.ssh"]
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let enabled = settings.access.mdns.effective(self.product.default_on);
        let source = if settings.access.mdns.enabled.is_some() {
            "setting"
        } else {
            "product"
        };
        let mut running = self
            .running
            .lock()
            .map_err(|_| anyhow!("mdns state poisoned"))?;
        if !enabled {
            if let Some(stopped) = running.take() {
                stopped.stop();
            }
            return Ok(json!({
                "enabled": false,
                "source": source,
                "hostname": format!("{}.local", settings.hostname),
                "services": [],
            }));
        }

        let wanted = built_in_services(settings, &self.product);
        if running.is_none() {
            *running = Some(Running {
                responder: self.start.start().context("start the mDNS responder")?,
                hostname: settings.hostname.clone(),
                published: BTreeMap::new(),
            });
        }
        let current = running
            .as_mut()
            .ok_or_else(|| anyhow!("the mDNS responder is not running"))?;
        // A new hostname renames every record: withdraw all under the old
        // name, then publish under the new one.
        let renamed = current.hostname != settings.hostname;
        let stale: Vec<String> = current
            .published
            .iter()
            .filter(|(fullname, service)| {
                renamed
                    || !wanted
                        .iter()
                        .any(|want| &want.fullname() == *fullname && want == *service)
            })
            .map(|(fullname, _)| fullname.clone())
            .collect();
        for fullname in stale {
            current.responder.withdraw(&fullname)?;
            current.published.remove(&fullname);
        }
        current.hostname.clone_from(&settings.hostname);
        for service in &wanted {
            let fullname = service.fullname();
            if !current.published.contains_key(&fullname) {
                current.responder.publish(&current.hostname, service)?;
                current.published.insert(fullname, service.clone());
            }
        }

        Ok(json!({
            "enabled": true,
            "source": source,
            "hostname": format!("{}.local", current.hostname),
            "services": current.published.values().map(Service::json).collect::<Vec<_>>(),
        }))
    }
}

/// The production responder: mdns-sd's daemon thread.
struct MdnsSd;

impl StartResponder for MdnsSd {
    fn start(&self) -> Result<Box<dyn Responder>> {
        let daemon = mdns_sd::ServiceDaemon::new().map_err(|err| anyhow!("{err}"))?;
        let silent = mdns_sd::IfPredicate::new(|intf: &if_addrs::Interface| {
            SILENT_PREFIXES
                .iter()
                .any(|prefix| intf.name.starts_with(prefix))
        });
        daemon
            .disable_interface(vec![
                mdns_sd::IfKind::LoopbackV4,
                mdns_sd::IfKind::LoopbackV6,
                mdns_sd::IfKind::Predicate(silent),
            ])
            .map_err(|err| anyhow!("{err}"))?;
        Ok(Box::new(Daemon(daemon)))
    }
}

struct Daemon(mdns_sd::ServiceDaemon);

impl Responder for Daemon {
    fn publish(&mut self, hostname: &str, service: &Service) -> Result<()> {
        let txt: std::collections::HashMap<String, String> =
            service.txt.clone().into_iter().collect();
        let mut info = mdns_sd::ServiceInfo::new(
            &format!("{}.local.", service.kind),
            &service.instance,
            &format!("{hostname}.local."),
            (),
            service.port,
            txt,
        )
        .map_err(|err| anyhow!("{err}"))?
        .enable_addr_auto();
        if !service.addresses.is_empty() {
            info.set_interfaces(
                service
                    .addresses
                    .iter()
                    .map(|address| mdns_sd::IfKind::Addr(*address))
                    .collect(),
            );
        }
        self.0
            .register(info)
            .map_err(|err| anyhow!("publish {}: {err}", service.fullname()))
    }

    fn withdraw(&mut self, fullname: &str) -> Result<()> {
        self.0
            .unregister(fullname)
            .map(drop)
            .map_err(|err| anyhow!("withdraw {fullname}: {err}"))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if let Err(err) = self.0.shutdown() {
            tracing::warn!("mdns: shut down the responder: {err}");
        }
    }
}

#[cfg(test)]
mod tests;

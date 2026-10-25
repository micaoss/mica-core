//! mDNS reconciler: the device announces itself on the LAN (docs/mica-core.md
//! section 3.13).
//!
//! A responder on mdns-sd, on either init, answers `<hostname>.local` with the
//! addresses of the interface a query arrives on, on every interface but the
//! loopback, the container engines' links, WireGuard and CAN, and publishes
//! DNS-SD services: `_mica._tcp` always and the console while apid serves the
//! network, then what the drop-in files and the running apps ask for
//! ([`entries`]); SSH is never announced. A query from off the link is not answered.
//! Switched off (`access.mdns.enabled`, else the product's `MDNS=` line),
//! there is no socket at all.

pub mod entries;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow};
use micad_settings::Settings;
use serde_json::json;

use super::Reconciler;

/// Interfaces the responder leaves out by name prefix: the loopback, the
/// virtual ends the container engines make (the ones the login prompt also
/// leaves out), WireGuard, which carries no multicast, and CAN.
pub(crate) const SILENT_PREFIXES: &[&str] = &[
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
    /// Who asked for it: `built-in`, `image:<name>`, `runtime:<name>` or
    /// `app:<name>`.
    pub source: String,
}

impl Service {
    /// The service's DNS name, `<instance>.<type>.local.`.
    pub(crate) fn fullname(&self) -> String {
        format!("{}.{}.local.", self.instance, self.kind)
    }

    /// `entry` of `source`, under `hostname` unless it names its instance.
    fn asked(hostname: &str, source: &str, entry: entries::Entry) -> Self {
        Self {
            instance: entry.instance.unwrap_or_else(|| hostname.to_string()),
            kind: entry.kind,
            port: entry.port,
            txt: entry.txt,
            source: source.to_string(),
        }
    }

    fn json(&self, published: bool) -> serde_json::Value {
        json!({
            "instance": self.instance,
            "type": self.kind,
            "port": self.port,
            "txt": self.txt,
            "source": self.source,
            "published": published,
        })
    }
}

/// The entries of the installed apps whose units run.
#[async_trait::async_trait]
pub trait AppEntries: Send + Sync {
    /// By app name, each app's entries that are published now.
    async fn running(&self) -> Vec<(String, Vec<entries::Entry>)>;
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

/// Where the entries beside the built-in ones come from.
pub(crate) struct Sources {
    pub image_dir: PathBuf,
    pub runtime_dir: PathBuf,
    /// Who must own a runtime drop-in: root on a device.
    pub owner: u32,
    pub apps: Option<Arc<dyn AppEntries>>,
}

/// Reconciler for the `access.mdns` settings subtree.
pub struct MdnsReconciler {
    product: Product,
    sources: Sources,
    start: Box<dyn StartResponder>,
    running: Mutex<Option<Running>>,
    /// The refusals last logged, so each is logged once.
    refused: Mutex<BTreeMap<String, String>>,
}

impl MdnsReconciler {
    pub(crate) fn new(product: Product, sources: Sources, start: Box<dyn StartResponder>) -> Self {
        Self {
            product,
            sources,
            start,
            running: Mutex::new(None),
            refused: Mutex::new(BTreeMap::new()),
        }
    }

    /// The responder on this device's interfaces, for this root's product,
    /// with the drop-ins and the entries of `apps`.
    pub fn production(apps: Option<Arc<dyn AppEntries>>) -> Self {
        Self::new(
            Product::production(),
            Sources {
                image_dir: PathBuf::from(entries::IMAGE_DIR),
                runtime_dir: PathBuf::from(entries::RUNTIME_DIR),
                owner: 0,
                apps,
            },
            Box::new(MdnsSd),
        )
    }

    /// Every service `settings` and the sources ask for, in order: the
    /// built-in ones, the drop-ins, the apps'. One whose name is taken
    /// already, or past [`entries::MAX_PUBLISHED`], is refused, as is a
    /// broken file; the refusals are returned by source.
    async fn wanted(&self, settings: &Settings) -> (Vec<Service>, BTreeMap<String, String>) {
        let hostname = &settings.hostname;
        let mut wanted = built_in_services(settings, &self.product);
        let mut refused = BTreeMap::new();
        let (image, runtime, owner) = (
            self.sources.image_dir.clone(),
            self.sources.runtime_dir.clone(),
            self.sources.owner,
        );
        let drop_ins =
            tokio::task::spawn_blocking(move || entries::drop_ins(&image, &runtime, owner))
                .await
                .unwrap_or_default();
        let mut asked = Vec::new();
        for (source, read) in drop_ins {
            match read {
                Ok(found) => asked.push((source, found)),
                Err(err) => {
                    refused.insert(source, err);
                }
            }
        }
        if let Some(apps) = &self.sources.apps {
            for (app, found) in apps.running().await {
                asked.push((format!("app:{app}"), found));
            }
        }
        for (source, found) in asked {
            for entry in found {
                let service = Service::asked(hostname, &source, entry);
                let fullname = service.fullname();
                if wanted.iter().any(|have| have.fullname() == fullname) {
                    refused.insert(source.clone(), format!("{fullname} is published already"));
                } else if wanted.len() >= entries::MAX_PUBLISHED {
                    refused.insert(
                        source.clone(),
                        format!("more than {} services", entries::MAX_PUBLISHED),
                    );
                } else {
                    wanted.push(service);
                }
            }
        }
        (wanted, refused)
    }

    /// Log each refusal once, while it lasts.
    fn log_refusals(&self, refused: &BTreeMap<String, String>) {
        let Ok(mut logged) = self.refused.lock() else {
            return;
        };
        for (source, err) in refused {
            if logged.get(source) != Some(err) {
                tracing::warn!(%source, "mdns: refused: {err}");
            }
        }
        logged.clone_from(refused);
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
        // The hostname names every record; the console's mode and ports
        // decide what is published.
        &["hostname", "access.web"]
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let enabled = settings.access.mdns.effective(self.product.default_on);
        let source = if settings.access.mdns.enabled.is_some() {
            "setting"
        } else {
            "product"
        };
        let (wanted, refused) = self.wanted(settings).await;
        self.log_refusals(&refused);
        let refused: Vec<_> = refused
            .iter()
            .map(|(source, error)| json!({ "source": source, "error": error }))
            .collect();
        let mut running = self
            .running
            .lock()
            .map_err(|_| anyhow!("mdns state poisoned"))?;
        if !enabled {
            if let Some(stopped) = running.take() {
                stopped.stop();
            }
            // Kept and listed, not published.
            return Ok(json!({
                "enabled": false,
                "source": source,
                "hostname": format!("{}.local", settings.hostname),
                "services": wanted.iter().map(|s| s.json(false)).collect::<Vec<_>>(),
                "refused": refused,
            }));
        }

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
            "services": wanted.iter().map(|s| s.json(true)).collect::<Vec<_>>(),
            "refused": refused,
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
        let info = mdns_sd::ServiceInfo::new(
            &format!("{}.local.", service.kind),
            &service.instance,
            &format!("{hostname}.local."),
            (),
            service.port,
            txt,
        )
        .map_err(|err| anyhow!("{err}"))?
        .enable_addr_auto();
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

//! DNS reconciler: the hosts file with the static records, and the global
//! servers in place of every interface's.
//!
//! `/etc/hosts` links to [`HOSTS_PATH`], which Base seeds at boot with the
//! lines this reconciler starts with. Global servers are resolved's drop-in on
//! systemd, with a networkd drop-in beside every `.network` that takes the
//! link's own servers away; on OpenRC they are `resolv.d/global`, which Base's
//! `mica-resolv` makes the whole of `resolv.conf`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use micad_settings::{DnsSettings, Settings};
use serde_json::json;

use super::Reconciler;
use super::network::{NetworkReload, Networkd};
use super::systemd::{Systemd, UnitControl};
use crate::fswrite::write_config_if_changed;
use crate::openrc::{Commands, Host};

/// The hosts file `/etc/hosts` links to.
const HOSTS_PATH: &str = "/run/mica/hosts";
/// Mode of every file this reconciler writes: none carries a secret.
const FILE_MODE: u32 = 0o644;
/// resolved's drop-in carrying the global servers.
const RESOLVED_DROP_IN: &str = "/run/systemd/resolved.conf.d/60-mica-dns.conf";
/// The resolver unit restarted when its drop-in changes.
const RESOLVED_UNIT: &str = "systemd-resolved.service";
/// Where networkd finds `.network` files, in its own order.
const NETWORK_DIRS: [&str; 4] = [
    "/etc/systemd/network",
    "/run/systemd/network",
    "/usr/local/lib/systemd/network",
    "/usr/lib/systemd/network",
];
/// Where the networkd drop-ins go: `<file>.network.d/` under it.
const RUNTIME_NETWORK_DIR: &str = "/run/systemd/network";
/// The name of the drop-in in each `.network.d`.
const LINK_DROP_IN: &str = "90-mica-global-dns.conf";
/// What the link drop-in says: no server of the link's own, and none taken
/// from a lease or a router advertisement.
const LINK_DROP_IN_TEXT: &str = "# Managed by micad from dns.global. Do not edit.\n\
     [Network]\nDNS=\n\n[DHCPv4]\nUseDNS=no\n\n[DHCPv6]\nUseDNS=no\n\n[IPv6AcceptRA]\nUseDNS=no\n";
/// The OpenRC resolver directory and the global file in it.
const RESOLVERS_DIR: &str = "/run/mica/resolv.d";
/// Base's program building `resolv.conf` from [`RESOLVERS_DIR`].
const MICA_RESOLV: &str = "/usr/lib/mica/mica-resolv";

/// Where the global servers go.
enum Global {
    /// resolved's drop-in, and the networkd drop-ins.
    Resolved {
        drop_in: PathBuf,
        network_dirs: Vec<PathBuf>,
        runtime_dir: PathBuf,
        control: Arc<dyn UnitControl>,
        reloader: Arc<dyn NetworkReload>,
    },
    /// `resolv.d/global` and `mica-resolv`.
    Resolvd {
        dir: PathBuf,
        rebuild: PathBuf,
        commands: Arc<dyn Commands>,
    },
}

/// Reconciler for the `dns` settings subtree.
pub struct DnsReconciler {
    hosts: PathBuf,
    global: Global,
}

impl DnsReconciler {
    /// A reconciler for a systemd root, writing the hosts file to `hosts`,
    /// resolved's drop-in to `drop_in` and the link drop-ins under
    /// `runtime_dir` for each `.network` in `network_dirs`.
    pub fn resolved(
        hosts: PathBuf,
        drop_in: PathBuf,
        network_dirs: Vec<PathBuf>,
        runtime_dir: PathBuf,
        control: Arc<dyn UnitControl>,
        reloader: Arc<dyn NetworkReload>,
    ) -> Self {
        Self {
            hosts,
            global: Global::Resolved {
                drop_in,
                network_dirs,
                runtime_dir,
                control,
                reloader,
            },
        }
    }

    /// A reconciler for an OpenRC root, writing `global` in `dir` and running
    /// `rebuild` after it changes.
    pub fn resolvd(
        hosts: PathBuf,
        dir: PathBuf,
        rebuild: PathBuf,
        commands: Arc<dyn Commands>,
    ) -> Self {
        Self {
            hosts,
            global: Global::Resolvd {
                dir,
                rebuild,
                commands,
            },
        }
    }

    /// The production reconciler of a systemd root.
    pub fn production() -> Self {
        Self::resolved(
            PathBuf::from(HOSTS_PATH),
            PathBuf::from(RESOLVED_DROP_IN),
            NETWORK_DIRS.iter().map(PathBuf::from).collect(),
            PathBuf::from(RUNTIME_NETWORK_DIR),
            Arc::new(Systemd::new()),
            Arc::new(Networkd),
        )
    }

    /// The production reconciler of an OpenRC root.
    pub fn openrc() -> Self {
        Self::resolvd(
            PathBuf::from(HOSTS_PATH),
            PathBuf::from(RESOLVERS_DIR),
            PathBuf::from(MICA_RESOLV),
            Arc::new(Host),
        )
    }
}

/// The hosts file for `hostname` and the static records: Base's three lines,
/// then one line per record with its comment.
fn render_hosts(hostname: &str, dns: &DnsSettings) -> String {
    let mut out = format!("127.0.0.1\tlocalhost\n::1\tlocalhost\n127.0.1.1\t{hostname}\n");
    for record in &dns.static_records {
        out.push_str(&format!("{}\t{}", record.address, record.hostname));
        if let Some(comment) = &record.comment {
            out.push_str(&format!("\t# {comment}"));
        }
        out.push('\n');
    }
    out
}

/// resolved's drop-in for the global servers.
fn render_resolved(servers: &[String]) -> String {
    format!(
        "# Managed by micad from dns.global. Do not edit.\n[Resolve]\nDNS={}\n",
        servers.join(" ")
    )
}

/// `resolv.d/global` for the global servers.
fn render_resolvd(servers: &[String]) -> String {
    servers
        .iter()
        .map(|server| format!("nameserver {server}\n"))
        .collect()
}

/// Remove `path`; reports whether it was there.
fn remove_if_present(path: &Path) -> Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

/// The names of the `.network` files in `dirs`.
fn network_files(dirs: &[PathBuf]) -> BTreeSet<String> {
    dirs.iter()
        .filter_map(|dir| std::fs::read_dir(dir).ok())
        .flatten()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.ends_with(".network"))
        .collect()
}

/// Write a link drop-in beside each `.network` in `wanted`, and remove every
/// other link drop-in under `runtime_dir`; reports whether anything changed.
fn sync_link_drop_ins(runtime_dir: &Path, wanted: &BTreeSet<String>) -> Result<bool> {
    let mut changed = false;
    for name in wanted {
        let path = runtime_dir.join(format!("{name}.d")).join(LINK_DROP_IN);
        changed |= write_config_if_changed(&path, LINK_DROP_IN_TEXT, FILE_MODE)?;
    }
    let Ok(entries) = std::fs::read_dir(runtime_dir) else {
        return Ok(changed);
    };
    for entry in entries {
        let entry = entry?;
        let Ok(dir_name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(name) = dir_name.strip_suffix(".d") else {
            continue;
        };
        if !name.ends_with(".network") || wanted.contains(name) {
            continue;
        }
        if remove_if_present(&entry.path().join(LINK_DROP_IN))? {
            changed = true;
            // Only when nothing else is in it: another drop-in is not ours.
            let _ = std::fs::remove_dir(entry.path());
        }
    }
    Ok(changed)
}

#[async_trait::async_trait]
impl Reconciler for DnsReconciler {
    fn name(&self) -> &'static str {
        "dns"
    }

    fn subtree(&self) -> &'static str {
        "dns"
    }

    fn reads(&self) -> &'static [&'static str] {
        // The hosts file names the hostname, and a link drop-in goes beside
        // each `.network` the network and station reconcilers write.
        &["hostname", "network", "wifi.client"]
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let dns = &settings.dns;
        micad_settings::validate_dns(dns, &settings.hostname, &settings.network)
            .map_err(anyhow::Error::msg)?;
        write_config_if_changed(
            &self.hosts,
            &render_hosts(&settings.hostname, dns),
            FILE_MODE,
        )?;
        let global = &dns.global;
        match &self.global {
            Global::Resolved {
                drop_in,
                network_dirs,
                runtime_dir,
                control,
                reloader,
            } => {
                let resolver_changed = if global.enabled {
                    write_config_if_changed(drop_in, &render_resolved(&global.servers), FILE_MODE)?
                } else {
                    remove_if_present(drop_in)?
                };
                let wanted = if global.enabled {
                    network_files(network_dirs)
                } else {
                    BTreeSet::new()
                };
                if sync_link_drop_ins(runtime_dir, &wanted)? {
                    reloader.reload().await?;
                }
                if resolver_changed {
                    control.restart(RESOLVED_UNIT).await?;
                }
            }
            Global::Resolvd {
                dir,
                rebuild,
                commands,
            } => {
                let path = dir.join("global");
                let changed = if global.enabled {
                    write_config_if_changed(&path, &render_resolvd(&global.servers), FILE_MODE)?
                } else {
                    remove_if_present(&path)?
                };
                if changed {
                    let program = rebuild.display().to_string();
                    commands.run(&program, &[]).await?.success(&program)?;
                }
            }
        }
        Ok(json!({
            "hosts": self.hosts.display().to_string(),
            "staticRecords": dns.static_records.len(),
            "global": { "enabled": global.enabled, "servers": global.servers },
        }))
    }
}

#[cfg(test)]
mod tests;

use std::sync::Mutex;

use micad_settings::{GlobalDns, StaticRecord};

use super::*;
use crate::openrc::FakeCommands;
use crate::reconciler::systemd::mock::MockUnitControl;

/// Recording [`NetworkReload`].
#[derive(Default)]
struct Reloads(Mutex<usize>);

#[async_trait::async_trait]
impl NetworkReload for Reloads {
    async fn reload(&self) -> anyhow::Result<()> {
        *self.0.lock().unwrap() += 1;
        Ok(())
    }
}

fn settings(enabled: bool, servers: &[&str], records: Vec<StaticRecord>) -> Settings {
    let mut settings = Settings {
        hostname: "edge".to_string(),
        ..Settings::default()
    };
    settings.dns.global = GlobalDns {
        enabled,
        servers: servers.iter().map(|server| (*server).to_string()).collect(),
    };
    settings.dns.static_records = records;
    settings
}

fn nas() -> StaticRecord {
    StaticRecord {
        hostname: "nas.lan".to_string(),
        address: "192.168.1.5".to_string(),
        comment: Some("the NAS".to_string()),
    }
}

struct Systemd {
    dir: tempfile::TempDir,
    control: Arc<MockUnitControl>,
    reloads: Arc<Reloads>,
    reconciler: DnsReconciler,
}

fn systemd() -> Systemd {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("lib");
    let runtime = dir.path().join("run");
    std::fs::create_dir_all(&image).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(image.join("80-dhcp.network"), "[Match]\nName=eth*\n").unwrap();
    std::fs::write(runtime.join("50-mica-eth1.network"), "[Match]\nName=eth1\n").unwrap();
    std::fs::write(runtime.join("50-mica-eth1.netdev"), "").unwrap();
    let control = Arc::new(MockUnitControl::new("active", "enabled"));
    let reloads = Arc::new(Reloads::default());
    let reconciler = DnsReconciler::resolved(
        dir.path().join("hosts"),
        dir.path().join("resolved.conf.d/60-mica-dns.conf"),
        vec![image, runtime.clone()],
        runtime,
        control.clone(),
        reloads.clone(),
    );
    Systemd {
        dir,
        control,
        reloads,
        reconciler,
    }
}

#[tokio::test]
async fn the_hosts_file_carries_the_hostname_and_the_records() {
    let host = systemd();
    host.reconciler
        .apply(&settings(false, &[], vec![nas()]))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(host.dir.path().join("hosts")).unwrap(),
        "127.0.0.1\tlocalhost\n::1\tlocalhost\n127.0.1.1\tedge\n\
         192.168.1.5\tnas.lan\t# the NAS\n"
    );
    // Off, nothing else is written and nothing restarted.
    assert!(!host.dir.path().join("resolved.conf.d").exists());
    assert!(host.control.calls().is_empty());
    assert_eq!(*host.reloads.0.lock().unwrap(), 0);
}

#[tokio::test]
async fn global_servers_replace_every_links_own_and_go_away_again() {
    let host = systemd();
    host.reconciler
        .apply(&settings(true, &["9.9.9.9", "1.1.1.1"], Vec::new()))
        .await
        .unwrap();

    assert_eq!(
        std::fs::read_to_string(host.dir.path().join("resolved.conf.d/60-mica-dns.conf")).unwrap(),
        "# Managed by micad from dns.global. Do not edit.\n[Resolve]\nDNS=9.9.9.9 1.1.1.1\n"
    );
    let run = host.dir.path().join("run");
    for unit in ["80-dhcp.network", "50-mica-eth1.network"] {
        let drop_in = run.join(format!("{unit}.d")).join(LINK_DROP_IN);
        let text = std::fs::read_to_string(&drop_in).unwrap();
        assert!(text.contains("[Network]\nDNS=\n"), "{text}");
        assert!(text.contains("[DHCPv4]\nUseDNS=no\n"), "{text}");
    }
    assert!(!run.join("50-mica-eth1.netdev.d").exists());
    assert_eq!(host.control.calls(), ["restart systemd-resolved.service"]);
    assert_eq!(*host.reloads.0.lock().unwrap(), 1);

    // The same settings again change nothing.
    host.reconciler
        .apply(&settings(true, &["9.9.9.9", "1.1.1.1"], Vec::new()))
        .await
        .unwrap();
    assert_eq!(host.control.calls().len(), 1);
    assert_eq!(*host.reloads.0.lock().unwrap(), 1);

    // Off keeps the servers in the settings and takes every file away.
    host.reconciler
        .apply(&settings(false, &["9.9.9.9", "1.1.1.1"], Vec::new()))
        .await
        .unwrap();
    assert!(
        !host
            .dir
            .path()
            .join("resolved.conf.d/60-mica-dns.conf")
            .exists()
    );
    assert!(!run.join("80-dhcp.network.d").exists());
    assert!(!run.join("50-mica-eth1.network.d").exists());
    assert_eq!(host.control.calls().len(), 2);
    assert_eq!(*host.reloads.0.lock().unwrap(), 2);
}

#[tokio::test]
async fn a_drop_in_beside_a_unit_that_is_gone_is_removed() {
    let host = systemd();
    let on = settings(true, &["9.9.9.9"], Vec::new());
    host.reconciler.apply(&on).await.unwrap();
    let run = host.dir.path().join("run");
    std::fs::remove_file(run.join("50-mica-eth1.network")).unwrap();

    host.reconciler.apply(&on).await.unwrap();

    assert!(!run.join("50-mica-eth1.network.d").exists());
    assert!(run.join("80-dhcp.network.d").join(LINK_DROP_IN).exists());
}

#[tokio::test]
async fn on_openrc_the_global_file_stands_alone_and_mica_resolv_rebuilds() {
    let dir = tempfile::tempdir().unwrap();
    let commands = Arc::new(FakeCommands::default());
    let reconciler = DnsReconciler::resolvd(
        dir.path().join("hosts"),
        dir.path().join("resolv.d"),
        PathBuf::from("/usr/lib/mica/mica-resolv"),
        commands.clone(),
    );

    reconciler
        .apply(&settings(true, &["9.9.9.9", "1.1.1.1"], Vec::new()))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("resolv.d/global")).unwrap(),
        "nameserver 9.9.9.9\nnameserver 1.1.1.1\n"
    );
    assert_eq!(commands.calls(), ["/usr/lib/mica/mica-resolv"]);

    reconciler
        .apply(&settings(false, &["9.9.9.9"], Vec::new()))
        .await
        .unwrap();
    assert!(!dir.path().join("resolv.d/global").exists());
    assert_eq!(commands.calls().len(), 2);
}

#[tokio::test]
async fn a_hand_edited_record_the_rule_refuses_writes_nothing() {
    let host = systemd();
    let mut bad = nas();
    bad.hostname = "edge".to_string();

    assert!(
        host.reconciler
            .apply(&settings(false, &[], vec![bad]))
            .await
            .is_err()
    );
    assert!(!host.dir.path().join("hosts").exists());
}

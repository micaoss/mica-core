//! The systemd case of DNS following: booted with systemd, networkd and
//! resolved, two uplinks networkd manages and one it does not, micad's
//! [`dns::Resolved`] takes a down uplink off the default DNS route and puts
//! it back, reads it from resolved, and sets it again after networkd
//! reconfigures the link.
//!
//! Ignored: it needs a running systemd with networkd and resolved; it runs
//! booted in a VM.

use std::process::Command;
use std::time::Duration;

use super::super::dns::{self, DnsOrder, Follow};

fn run(program: &str, args: &[&str]) -> String {
    let output = Command::new(program).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{program} {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn index(name: &str) -> i32 {
    std::fs::read_to_string(format!("/sys/class/net/{name}/ifindex"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// resolved's `DefaultRoute` of the link `name`.
async fn default_route(connection: &zbus::Connection, name: &str) -> bool {
    let reply = connection
        .call_method(
            Some("org.freedesktop.resolve1"),
            "/org/freedesktop/resolve1",
            Some("org.freedesktop.resolve1.Manager"),
            "GetLink",
            &(index(name)),
        )
        .await
        .unwrap();
    let path: zbus::OwnedObjectPath = reply.body().deserialize().unwrap();
    let reply = connection
        .call_method(
            Some("org.freedesktop.resolve1"),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.resolve1.Link", "DefaultRoute"),
        )
        .await
        .unwrap();
    let value: zbus::OwnedVariant = reply.body().deserialize().unwrap();
    bool::try_from(value).unwrap()
}

/// Wait until networkd has configured `name`.
async fn configured(name: &str) {
    let state = format!("/run/systemd/netif/links/{}", index(name));
    for _ in 0..300 {
        if std::fs::read_to_string(&state)
            .is_ok_and(|text| text.lines().any(|line| line == "ADMIN_STATE=configured"))
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("networkd did not configure {name}");
}

fn order(order: &[&str], down: &[&str]) -> DnsOrder {
    DnsOrder {
        order: order.iter().map(|name| (*name).to_string()).collect(),
        down: down.iter().map(|name| (*name).to_string()).collect(),
    }
}

#[tokio::test]
#[ignore = "needs systemd, networkd and resolved: run booted in a VM"]
async fn vm_dns_follows_on_systemd() {
    for (n, name) in ["eth0", "eth1", "eth2"].iter().enumerate() {
        run(
            "ip",
            &[
                "link",
                "add",
                name,
                "type",
                "veth",
                "peer",
                "name",
                &format!("p{n}"),
            ],
        );
        run("ip", &["link", "set", &format!("p{n}"), "up"]);
    }
    std::fs::create_dir_all("/etc/systemd/network").unwrap();
    for n in 1..=2 {
        std::fs::write(
            format!("/etc/systemd/network/10-eth{}.network", n - 1),
            format!(
                "[Match]\nName=eth{}\n\n[Network]\nAddress=10.{n}.0.2/24\nGateway=10.{n}.0.1\nDNS=10.{n}.0.53\nConfigureWithoutCarrier=yes\n",
                n - 1
            ),
        )
        .unwrap();
    }
    run("networkctl", &["reload"]);
    configured("eth0").await;
    configured("eth1").await;
    run("ip", &["link", "set", "eth2", "up"]);
    let connection = zbus::Connection::system().await.unwrap();
    assert!(default_route(&connection, "eth0").await);
    println!("VM_DNS networkd configured eth0 and eth1; eth2 unmanaged");

    // eth0 down: managed (through networkd) and unmanaged (resolved) alike.
    let resolved = dns::Resolved::default();
    resolved
        .follow(&order(&["eth1", "eth0", "eth2"], &["eth0", "eth2"]))
        .await;
    assert!(!default_route(&connection, "eth0").await);
    assert!(!default_route(&connection, "eth2").await);
    assert!(default_route(&connection, "eth1").await);
    println!(
        "VM_DNS eth0 down: {}",
        run("resolvectl", &["status", "eth0"]).replace('\n', " | ")
    );

    // networkd reconfigures the link; the next pass past REASSERT sets it again.
    run("networkctl", &["reconfigure", "eth0"]);
    configured("eth0").await;
    tokio::time::sleep(dns::REASSERT + Duration::from_secs(1)).await;
    resolved
        .follow(&order(&["eth1", "eth0", "eth2"], &["eth0", "eth2"]))
        .await;
    assert!(!default_route(&connection, "eth0").await);
    println!("VM_DNS eth0 still off the default route after a reconfigure");

    // Back: nothing to steer.
    resolved.follow(&DnsOrder::default()).await;
    assert!(default_route(&connection, "eth0").await);
    assert!(default_route(&connection, "eth2").await);
    println!("VM_DNS eth0 back on the default route");
    println!("VM_DNS PASS");
}

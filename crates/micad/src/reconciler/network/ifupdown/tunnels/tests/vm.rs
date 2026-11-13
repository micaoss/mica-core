//! The QEMU case of the OpenRC tunnels: the reconciler against a real kernel,
//! with two far ends in network namespaces of their own, each behind a veth
//! pair and holding a subnet of its own.
//!
//! Ignored: it needs root, a kernel with WireGuard, and iproute2's `ip`,
//! wireguard-tools' `wg` and `ping` on `PATH`; it runs booted in a VM.

use std::io::Write as _;
use std::process::{Command, Stdio};

use super::*;
use crate::netlink::tunnel::Kernel;
use resolve::SystemResolver;

fn run(args: &[&str]) -> String {
    let output = Command::new(args[0]).args(&args[1..]).output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn far(ns: &str, args: &[&str]) -> String {
    let mut all = vec!["ip", "netns", "exec", ns];
    all.extend(args);
    run(&all)
}

/// A WireGuard key pair, private and public.
fn key_pair() -> (String, String) {
    let private = run(&["wg", "genkey"]).trim().to_string();
    let mut child = Command::new("wg")
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(private.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    (
        private,
        String::from_utf8(output.stdout).unwrap().trim().to_string(),
    )
}

/// Whether `address` answers a ping within a few tries; the first may be
/// spent on a handshake.
fn reaches(address: &str) -> bool {
    (0..5).any(|_| {
        Command::new("ping")
            .args(["-c", "1", "-W", "2", address])
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    })
}

/// A far end: namespace `ns` behind veth `outer`/`inner` on `underlay`
/// (`.1` here, `.2` there), its tunnel `wg1` at `tunnel` on `port`, and
/// `subnet`'s `.1` on its loopback. Returns its public key.
fn far_end(
    ns: &str,
    outer: &str,
    underlay: &str,
    tunnel: &str,
    port: &str,
    subnet: &str,
) -> String {
    run(&["ip", "netns", "add", ns]);
    run(&[
        "ip", "link", "add", outer, "type", "veth", "peer", "name", "inner",
    ]);
    run(&["ip", "link", "set", "inner", "netns", ns]);
    run(&[
        "ip",
        "addr",
        "add",
        &format!("{underlay}.1/24"),
        "dev",
        outer,
    ]);
    run(&["ip", "link", "set", outer, "up"]);
    far(
        ns,
        &[
            "ip",
            "addr",
            "add",
            &format!("{underlay}.2/24"),
            "dev",
            "inner",
        ],
    );
    far(ns, &["ip", "link", "set", "inner", "up"]);
    far(ns, &["ip", "link", "set", "lo", "up"]);
    far(
        ns,
        &["ip", "addr", "add", &format!("{subnet}.1/24"), "dev", "lo"],
    );
    let (private, public) = key_pair();
    let key_file = format!("/tmp/{ns}.key");
    std::fs::write(&key_file, private).unwrap();
    far(ns, &["ip", "link", "add", "wg1", "type", "wireguard"]);
    far(
        ns,
        &[
            "wg",
            "set",
            "wg1",
            "private-key",
            &key_file,
            "listen-port",
            port,
        ],
    );
    far(ns, &["ip", "addr", "add", tunnel, "dev", "wg1"]);
    far(ns, &["ip", "link", "set", "wg1", "up"]);
    public
}

fn far_peer(public_key: &str, allowed: &[&str], endpoint: &str) -> WireguardPeer {
    WireguardPeer {
        public_key: public_key.to_string(),
        allowed_ips: allowed.iter().map(|ip| (*ip).to_string()).collect(),
        endpoint: Some(endpoint.to_string()),
        persistent_keepalive: None,
        name: None,
        preshared_key: None,
    }
}

/// The kernel's latest handshake with the peer `public_key` of `wg0`.
fn handshake(public_key: &str) -> Option<u64> {
    crate::netlink::wireguard_device("wg0")
        .unwrap()
        .into_iter()
        .find(|peer| peer.public_key == public_key)
        .and_then(|peer| peer.latest_handshake)
}

fn kernel_endpoint(public_key: &str) -> Option<SocketAddr> {
    crate::netlink::wireguard_device("wg0")
        .unwrap()
        .into_iter()
        .find(|peer| peer.public_key == public_key)
        .and_then(|peer| peer.endpoint)
}

fn routes_to(destination: &str) -> String {
    run(&["ip", "route", "show", destination])
}

#[tokio::test]
#[ignore = "needs root and a WireGuard kernel: run booted in a VM"]
async fn vm_openrc_tunnels() {
    let dir = tempfile::tempdir().unwrap();
    let keys = Keystore::under(dir.path(), None);
    let fast = vec![Duration::from_millis(200)];
    let tunnels = || {
        Tunnels::new(Arc::new(Kernel), keys.clone())
            .with_resolver(Arc::new(SystemResolver), fast.clone())
    };
    let first = tunnels();

    let one = far_end(
        "far1",
        "veth1",
        "192.0.2",
        "10.8.0.2/24",
        "51821",
        "10.20.0",
    );
    let two = far_end(
        "far2",
        "veth2",
        "198.51.100",
        "10.8.0.3/24",
        "51822",
        "10.30.0",
    );
    let settings = |two_allowed: &[&str], one_endpoint: &str| {
        network(&[(
            "wg0",
            tunnel(
                "10.8.0.1/24",
                vec![
                    far_peer(&one, &["10.8.0.2/32", "10.20.0.0/24"], one_endpoint),
                    far_peer(&two, two_allowed, "198.51.100.2:51822"),
                ],
            ),
        )])
    };
    let state = first
        .apply(&settings(
            &["10.8.0.3/32", "10.30.0.0/24"],
            "192.0.2.2:51821",
        ))
        .unwrap();
    let device = state["wg0"]["publicKey"].as_str().unwrap().to_string();
    for ns in ["far1", "far2"] {
        far(
            ns,
            &[
                "wg",
                "set",
                "wg1",
                "peer",
                &device,
                "allowed-ips",
                "10.8.0.0/24",
            ],
        );
    }

    // A handshake with each far end, and each one's subnet routed.
    assert!(reaches("10.20.0.1"), "far1's subnet over the tunnel");
    assert!(reaches("10.30.0.1"), "far2's subnet over the tunnel");
    assert!(routes_to("10.20.0.0/24").contains("proto 109"));
    let index = first.kernel.owned_links().unwrap()["wg0"];
    let one_handshake = handshake(&one);
    assert!(one_handshake.is_some());
    println!("WG-OPENRC first apply: link {index}, both subnets reached");

    // A peer edit: far2 gains a subnet, far1 keeps its session.
    far("far2", &["ip", "addr", "add", "10.31.0.1/24", "dev", "lo"]);
    let edited = settings(
        &["10.8.0.3/32", "10.30.0.0/24", "10.31.0.0/24"],
        "192.0.2.2:51821",
    );
    first.apply(&edited).unwrap();
    assert!(reaches("10.31.0.1"), "far2's new subnet");
    assert_eq!(first.kernel.owned_links().unwrap()["wg0"], index);
    assert_eq!(handshake(&one), one_handshake, "far1 kept its session");
    println!("WG-OPENRC peer edit: in place, far1's handshake unchanged");

    // A micad restart: a new reconciler changes nothing that is healthy.
    run(&[
        "ip",
        "route",
        "add",
        "10.99.0.0/24",
        "dev",
        "wg0",
        "proto",
        "static",
    ]);
    let restarted = tunnels();
    restarted.apply(&edited).unwrap();
    assert_eq!(restarted.kernel.owned_links().unwrap()["wg0"], index);
    assert_eq!(handshake(&one), one_handshake);
    assert!(reaches("10.20.0.1"));
    assert!(
        !routes_to("10.99.0.0/24").is_empty(),
        "a foreign route stays"
    );
    println!("WG-OPENRC restart: same link, same session, foreign route kept");

    // A stale route of micad's goes with the prefix that named it.
    restarted
        .apply(&settings(
            &["10.8.0.3/32", "10.30.0.0/24"],
            "192.0.2.2:51821",
        ))
        .unwrap();
    assert!(routes_to("10.31.0.0/24").is_empty());
    assert!(!routes_to("10.99.0.0/24").is_empty());

    // A reboot: the link is gone, and the next apply builds it again with the
    // same key, so the far ends answer.
    run(&["ip", "link", "del", "wg0"]);
    let rebooted = tunnels();
    rebooted
        .apply(&settings(
            &["10.8.0.3/32", "10.30.0.0/24"],
            "192.0.2.2:51821",
        ))
        .unwrap();
    assert!(reaches("10.20.0.1"), "after a reboot");
    assert!(reaches("10.30.0.1"), "after a reboot");
    println!("WG-OPENRC reboot: rebuilt, both subnets reached");

    // A host-name endpoint is pending until the name resolves, then set.
    run(&["ip", "link", "del", "wg0"]);
    std::fs::create_dir_all("/etc").unwrap();
    std::fs::write("/etc/hosts", "127.0.0.1 localhost\n").unwrap();
    rebooted
        .apply(&settings(
            &["10.8.0.3/32", "10.30.0.0/24"],
            "far1.test:51821",
        ))
        .unwrap();
    assert_eq!(kernel_endpoint(&one), None, "pending resolution");
    std::fs::write("/etc/hosts", "127.0.0.1 localhost\n192.0.2.2 far1.test\n").unwrap();
    let mut resolved = None;
    for _ in 0..50 {
        resolved = kernel_endpoint(&one);
        if resolved.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(resolved, Some("192.0.2.2:51821".parse().unwrap()));
    assert!(reaches("10.20.0.1"), "through the resolved endpoint");
    println!("WG-OPENRC host name: pending, then resolved to {resolved:?}");

    // A removed entry deletes micad's link and leaves a foreign one.
    run(&["ip", "link", "add", "wg9", "type", "wireguard"]);
    rebooted.apply(&BTreeMap::new()).unwrap();
    assert!(
        Command::new("ip")
            .args(["link", "show", "wg0"])
            .output()
            .unwrap()
            .stdout
            .is_empty()
    );
    assert!(!run(&["ip", "link", "show", "wg9"]).is_empty());
    println!("WG-OPENRC removal: wg0 deleted, the foreign wg9 kept");
}

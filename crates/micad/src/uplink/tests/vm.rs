//! The QEMU case of the uplink engine: the engine on the running kernel, two
//! uplinks to two "ISP" namespaces, each with an upstream link to one
//! "internet" namespace that holds the probe target.
//!
//! Ignored: it needs root and iproute2's `ip` on `PATH`; it runs booted in a
//! VM.

use std::process::Command;

use super::*;

fn ip(args: &[&str]) -> String {
    let output = Command::new("ip").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "ip {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

/// ISP `n`: the device's `eth{n-1}` at 10.n.0.2 to the ISP at 10.n.0.1, and
/// the ISP's upstream `up{n}` to the internet namespace over 172.16.n.0/30.
fn isp(n: u8) {
    let (ns, device, isp_side, up, net) = (
        format!("isp{n}"),
        format!("eth{}", n - 1),
        format!("i{n}"),
        format!("up{n}"),
        format!("n{n}"),
    );
    ip(&["netns", "add", &ns]);
    ip(&[
        "link", "add", &device, "type", "veth", "peer", "name", &isp_side, "netns", &ns,
    ]);
    ip(&["addr", "add", &format!("10.{n}.0.2/24"), "dev", &device]);
    ip(&["link", "set", &device, "up"]);
    ip(&[
        "-n",
        &ns,
        "addr",
        "add",
        &format!("10.{n}.0.1/24"),
        "dev",
        &isp_side,
    ]);
    ip(&["-n", &ns, "link", "set", &isp_side, "up"]);
    ip(&["-n", &ns, "link", "set", "lo", "up"]);
    ip(&[
        "-n", &ns, "link", "add", &up, "type", "veth", "peer", "name", &net, "netns", "net",
    ]);
    ip(&[
        "-n",
        &ns,
        "addr",
        "add",
        &format!("172.16.{n}.1/30"),
        "dev",
        &up,
    ]);
    ip(&["-n", &ns, "link", "set", &up, "up"]);
    ip(&[
        "-n",
        &ns,
        "route",
        "add",
        "default",
        "via",
        &format!("172.16.{n}.2"),
    ]);
    ip(&[
        "netns",
        "exec",
        &ns,
        "sysctl",
        "-qw",
        "net.ipv4.ip_forward=1",
    ]);
    ip(&[
        "-n",
        "net",
        "addr",
        "add",
        &format!("172.16.{n}.2/30"),
        "dev",
        &net,
    ]);
    ip(&["-n", "net", "link", "set", &net, "up"]);
    ip(&[
        "-n",
        "net",
        "route",
        "add",
        &format!("10.{n}.0.0/24"),
        "via",
        &format!("172.16.{n}.1"),
    ]);
    // As networkd and mica-udhcpc put it: the uplink's own default route.
    ip(&[
        "route",
        "add",
        "default",
        "via",
        &format!("10.{n}.0.1"),
        "dev",
        &device,
        "proto",
        "dhcp",
        "metric",
        &(90 + u32::from(n) * 10).to_string(),
    ]);
}

fn overrides() -> String {
    ip(&["route", "show", "default", "proto", "109"])
        .trim()
        .to_string()
}

fn settings() -> BTreeMap<String, IfaceSettings> {
    let check = ProbeConfig {
        enabled: true,
        targets: vec![
            ProbeTarget {
                address: "192.0.2.10".to_string(),
                method: "icmp".to_string(),
                port: None,
            },
            // A closed port answers with a reset, which proves the path.
            ProbeTarget {
                address: "192.0.2.10".to_string(),
                method: "tcp".to_string(),
                port: Some(53),
            },
        ],
        interval_s: Some(2),
        timeout_ms: Some(500),
        failures_to_down: Some(2),
        successes_to_up: Some(2),
    };
    ["eth0", "eth1"]
        .iter()
        .map(|name| {
            (
                (*name).to_string(),
                IfaceSettings {
                    dhcp: true,
                    probe: Some(check.clone()),
                    ..IfaceSettings::default()
                },
            )
        })
        .collect()
}

/// Rounds until `done` holds of the engine, at most `limit` of them.
async fn until(
    engine: &mut Engine,
    settings: &BTreeMap<String, IfaceSettings>,
    limit: usize,
    done: impl Fn(&Engine) -> bool,
) {
    for _ in 0..limit {
        rounds(engine, settings, 1).await;
        if done(engine) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("not reached: {:#}", engine.observation(settings));
}

fn states(engine: &Engine) -> (State, State) {
    (engine.trackers["eth0"].state, engine.trackers["eth1"].state)
}

#[tokio::test]
#[ignore = "needs root and iproute2: run booted in a VM"]
async fn vm_uplink_failover() {
    ip(&["link", "set", "lo", "up"]);
    ip(&["netns", "add", "net"]);
    ip(&["-n", "net", "link", "set", "lo", "up"]);
    ip(&["-n", "net", "addr", "add", "192.0.2.10/32", "dev", "lo"]);
    isp(1);
    isp(2);
    let settings = settings();
    let mut engine = Engine::new(Arc::new(Kernel));
    engine.clear();

    until(&mut engine, &settings, 10, |e| {
        states(e) == (State::Up, State::Up)
    })
    .await;
    assert_eq!(overrides(), "");
    println!("VM_UPLINK both up, no failover route");

    // ISP 1's upstream dies, its link to the device stays up.
    ip(&["-n", "isp1", "link", "set", "up1", "down"]);
    until(&mut engine, &settings, 10, |e| states(e).0 == State::Down).await;
    assert_eq!(overrides(), "default via 10.2.0.1 dev eth1 metric 10");
    assert!(ip(&["route", "get", "192.0.2.10"]).contains("dev eth1"));
    println!("VM_UPLINK eth0 down: failover via eth1, traffic leaves by eth1");

    // A renewal's address flush takes the route; the next pass puts it back.
    ip(&["route", "del", "default", "proto", "109"]);
    engine.tick(&settings);
    assert_eq!(overrides(), "default via 10.2.0.1 dev eth1 metric 10");
    println!("VM_UPLINK failover route re-added after removal");

    ip(&["-n", "isp1", "link", "set", "up1", "up"]);
    ip(&[
        "-n",
        "isp1",
        "route",
        "replace",
        "default",
        "via",
        "172.16.1.2",
    ]);
    until(&mut engine, &settings, 10, |e| states(e).0 == State::Up).await;
    assert_eq!(overrides(), "");
    assert_eq!(engine.last_change.clone().unwrap().reason, "recovered");
    println!("VM_UPLINK eth0 back: failover route removed");

    ip(&["-n", "isp1", "link", "set", "up1", "down"]);
    ip(&["-n", "isp2", "link", "set", "up2", "down"]);
    until(&mut engine, &settings, 10, |e| {
        states(e) == (State::Down, State::Down)
    })
    .await;
    assert_eq!(overrides(), "");
    println!("VM_UPLINK both down: no failover route");

    std::fs::write("/proc/sys/net/ipv4/conf/eth1/rp_filter", "1").unwrap();
    engine.tick(&settings);
    assert_eq!(engine.trackers["eth1"].state, State::Blocked);
    println!("VM_UPLINK strict rp_filter: eth1 blocked");
    println!("VM_UPLINK PASS");
}

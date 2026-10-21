use super::*;
use crate::openrc::FakeCommands;

const ADDRS: &str = "\
1: lo    inet 127.0.0.1/8 scope host lo\\       valid_lft forever preferred_lft forever
2: eth0    inet 10.0.2.15/24 brd 10.0.2.255 scope global eth0\\       valid_lft forever preferred_lft forever
2: eth0    inet6 fe80::5054:ff:fe12:3456/64 scope link \\       valid_lft forever preferred_lft forever
";

fn sysfs(root: &Path, name: &str, attributes: &[(&str, &str)]) {
    let dir = root.join(NET_CLASS_DIR).join(name);
    std::fs::create_dir_all(&dir).unwrap();
    for (attribute, value) in attributes {
        std::fs::write(dir.join(attribute), format!("{value}\n")).unwrap();
    }
}

fn fixture() -> (tempfile::TempDir, Arc<FakeCommands>) {
    let root = tempfile::tempdir().unwrap();
    sysfs(
        root.path(),
        "lo",
        &[
            ("ifindex", "1"),
            ("type", "772"),
            ("carrier", "1"),
            ("operstate", "unknown"),
            ("mtu", "65536"),
            ("address", "00:00:00:00:00:00"),
        ],
    );
    sysfs(
        root.path(),
        "eth0",
        &[
            ("ifindex", "2"),
            ("type", "1"),
            ("carrier", "1"),
            ("operstate", "up"),
            ("mtu", "1500"),
            ("address", "52:54:00:12:34:56"),
        ],
    );
    sysfs(
        root.path(),
        "eth1",
        &[
            ("ifindex", "3"),
            ("type", "1"),
            ("carrier", "0"),
            ("operstate", "down"),
            ("mtu", "1500"),
            ("address", "52:54:00:12:34:57"),
        ],
    );
    let resolvers = root.path().join(RESOLVERS_DIR);
    std::fs::create_dir_all(&resolvers).unwrap();
    std::fs::write(resolvers.join("eth0"), "search lan\nnameserver 10.0.2.3\n").unwrap();
    let fake = Arc::new(FakeCommands::default());
    fake.answer("ip -o addr show", 0, ADDRS);
    fake.answer(
        "ip -4 route show",
        0,
        "default via 10.0.2.2 dev eth0 metric 10\n\
         10.0.2.0/24 dev eth0 scope link  src 10.0.2.15\n\
         unreachable 198.51.100.0/24\n",
    );
    fake.answer("ip -6 route show", 0, "fe80::/64 dev eth0  metric 256\n");
    (root, fake)
}

#[tokio::test]
async fn links_addresses_routes_and_dns_are_described_as_networkd_would() {
    let (root, fake) = fixture();
    let state = IpNetworkState::new(root.path(), Arc::clone(&fake) as Arc<dyn Commands>);
    let raw = state.describe_raw().await.unwrap();
    let links = raw["Interfaces"].as_array().unwrap();
    let names: Vec<_> = links
        .iter()
        .map(|link| link["Name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["eth0", "eth1", "lo"]);
    let eth0 = &links[0];
    assert_eq!(eth0["OperationalState"], "routable");
    assert_eq!(
        eth0["HardwareAddress"],
        json!([0x52, 0x54, 0, 0x12, 0x34, 0x56])
    );
    assert_eq!(eth0["Addresses"][0]["Address"], json!([10, 0, 2, 15]));
    assert_eq!(eth0["Addresses"][0]["PrefixLength"], 24);
    assert_eq!(eth0["Addresses"].as_array().unwrap().len(), 2);
    assert_eq!(eth0["Routes"][0]["Gateway"], json!([10, 0, 2, 2]));
    assert_eq!(eth0["Routes"][0]["Priority"], 10);
    assert_eq!(eth0["DNS"][0]["Address"], json!([10, 0, 2, 3]));
    assert_eq!(links[1]["OperationalState"], "no-carrier");
    assert_eq!(links[2]["Type"], "loopback");
    assert!(links[2].get("HardwareAddress").is_none());
}

#[tokio::test]
async fn the_observation_reports_the_interfaces_and_the_default_route() {
    let (root, fake) = fixture();
    let state = IpNetworkState::new(root.path(), Arc::clone(&fake) as Arc<dyn Commands>);
    let observed = state.observe().await.unwrap();
    assert_eq!(observed["interfaces"]["available"], true);
    assert_eq!(observed["interfaces"]["count"], 3);
    assert_eq!(observed["defaultRoutes"]["count"], 1);
    let described = state.describe().await.unwrap();
    assert_eq!(described["interfaceCount"], 3);
}

#[tokio::test]
async fn an_ip_that_fails_leaves_the_interfaces_absent() {
    let (root, fake) = fixture();
    fake.answer("ip -o addr show", 1, "");
    let state = IpNetworkState::new(root.path(), Arc::clone(&fake) as Arc<dyn Commands>);
    let observed = state.observe().await.unwrap();
    assert_eq!(observed["interfaces"]["available"], false);
}

#[tokio::test]
async fn every_main_table_route_is_described_with_its_destination() {
    let (root, fake) = fixture();
    let state = IpNetworkState::new(root.path(), Arc::clone(&fake) as Arc<dyn Commands>);
    let raw = state.describe_raw().await.unwrap();
    let routes = &raw["Interfaces"][0]["Routes"];
    assert_eq!(routes.as_array().unwrap().len(), 3);
    assert_eq!(routes[1]["Destination"], json!([10, 0, 2, 0]));
    assert_eq!(routes[1]["DestinationPrefixLength"], 24);
    assert!(routes[1].get("Gateway").is_none());
    assert_eq!(routes[2]["Family"], 10);
    assert_eq!(routes[2]["DestinationPrefixLength"], 64);
    assert_eq!(routes[2]["Priority"], 256);
}

#[tokio::test]
async fn a_can_link_is_typed_can() {
    let (root, fake) = fixture();
    sysfs(
        root.path(),
        "can0",
        &[
            ("ifindex", "4"),
            ("type", "280"),
            ("carrier", "1"),
            ("operstate", "up"),
        ],
    );
    let state = IpNetworkState::new(root.path(), Arc::clone(&fake) as Arc<dyn Commands>);
    let raw = state.describe_raw().await.unwrap();
    assert_eq!(raw["Interfaces"][0]["Name"], "can0");
    assert_eq!(raw["Interfaces"][0]["Type"], "can");
}

/// The access point's udhcpd leases land on its link the way networkd's DHCP
/// server reports its own.
#[tokio::test]
async fn the_access_point_link_carries_udhcpd_s_leases() {
    let (root, fake) = fixture();
    let run = root.path().join("run/mica");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("wifi-ap.env"), "interface=eth1\nconfig=/x\n").unwrap();
    let mut file = 1_792_000_000u64.to_be_bytes().to_vec();
    for (last, hostname) in [(1u8, &b"phone"[..]), (2, &b""[..])] {
        file.extend(3600u32.to_be_bytes());
        file.extend([192, 168, 4, 100 + last]);
        file.extend([0x02, 0x5c, 0, 0, 0, last]);
        let mut name = hostname.to_vec();
        name.resize(20, 0);
        file.extend(name);
        file.extend([0, 0]);
    }
    std::fs::write(run.join("wifi-ap-udhcpd.leases"), file).unwrap();
    let state = IpNetworkState::new(root.path(), Arc::clone(&fake) as Arc<dyn Commands>);
    let raw = state.describe_raw().await.unwrap();
    let leases = &raw["Interfaces"][1]["DHCPServer"]["Leases"];
    assert_eq!(
        leases,
        &json!([
            {"Address": [192, 168, 4, 101], "HardwareAddress": [2, 0x5c, 0, 0, 0, 1], "Hostname": "phone"},
            {"Address": [192, 168, 4, 102], "HardwareAddress": [2, 0x5c, 0, 0, 0, 2]}
        ])
    );
    assert!(raw["Interfaces"][0].get("DHCPServer").is_none());
}

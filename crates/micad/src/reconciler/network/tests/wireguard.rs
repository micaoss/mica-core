//! WireGuard tunnels, their keys and rotation.

use micad_settings::IfaceSettings;
use micad_settings::WireguardPeer;
use std::sync::{Arc, Mutex};

use super::*;

#[tokio::test]
pub(super) async fn renders_a_wireguard_netdev_naming_its_key_file_and_its_peers() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("wg0", wireguard_iface(Some(51820), vec![peer(1)]))]);

    reconciler.apply(&settings).await.unwrap();

    let netdev = std::fs::read_to_string(dir.path().join("50-mica-wg0.netdev")).unwrap();
    let unit = std::fs::read_to_string(dir.path().join("50-mica-wg0.network")).unwrap();
    assert_eq!(
        netdev,
        format!(
            "[NetDev]\nName=wg0\nKind=wireguard\n\n\
             [WireGuard]\nPrivateKeyFile={}\nListenPort=51820\n\n\
             [WireGuardPeer]\nPublicKey={}\nAllowedIPs=10.8.0.0/24\n\
             Endpoint=vpn.example.net:51820\nPersistentKeepalive=25\n",
            dir.path().join("networkd-secrets/wg-wg0.key").display(),
            peer_key(1),
        )
    );
    // The addressing side of a tunnel is the same `.network` every other
    // kind gets.
    assert_eq!(
        unit,
        "[Match]\nName=wg0\n\n[Network]\nAddress=10.8.0.2/24\n"
    );
    // A device that did not exist before is not deleted on the way in.
    assert_eq!(*calls.lock().unwrap(), vec!["reload".to_string()]);
}

#[tokio::test]
pub(super) async fn renders_a_tunnel_that_only_initiates_without_a_listen_port() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[(
        "wg0",
        wireguard_iface(
            None,
            vec![WireguardPeer {
                public_key: peer_key(2),
                allowed_ips: vec!["10.8.0.0/24".to_string(), "fd00::/64".to_string()],
                endpoint: None,
                persistent_keepalive: None,
            }],
        ),
    )]);

    reconciler.apply(&settings).await.unwrap();

    let netdev = std::fs::read_to_string(dir.path().join("50-mica-wg0.netdev")).unwrap();
    // Absent optionals render no line at all: an empty `ListenPort=` would
    // be a port, and networkd picking one is what a client wants.
    assert!(!netdev.contains("ListenPort"), "{netdev}");
    assert!(!netdev.contains("Endpoint"), "{netdev}");
    assert!(!netdev.contains("PersistentKeepalive"), "{netdev}");
    // Several allowed IPs are one comma-separated directive.
    assert!(
        netdev.contains("AllowedIPs=10.8.0.0/24,fd00::/64\n"),
        "{netdev}"
    );
}

#[tokio::test]
pub(super) async fn generates_the_key_once_and_publishes_only_its_public_half() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("wg0", wireguard_iface(Some(51820), vec![peer(1)]))]);

    let first = reconciler.apply(&settings).await.unwrap();
    let second = reconciler.apply(&settings).await.unwrap();

    let key_file = dir.path().join("networkd-secrets/wg-wg0.key");
    let private_key = std::fs::read_to_string(&key_file).unwrap();
    let public_key = first["wg0"]["publicKey"].as_str().unwrap().to_string();
    // Lazy, and exactly once: the second pass finds the key it drew.
    assert_eq!(first, second);
    assert_eq!(std::fs::read_to_string(&key_file).unwrap(), private_key);
    assert_eq!(
        first,
        json!({
            "wg0": {
                "file": "50-mica-wg0.network",
                "dhcp": false,
                "kind": "wireguard",
                "publicKey": public_key,
            }
        })
    );
    // The leak canary: the private key is in exactly one place, and the
    // tree served over the bus is not it. Nor is any rendered unit -- they
    // name the key file, they do not carry it.
    let state = serde_json::to_string(&first).unwrap();
    assert!(!state.contains(private_key.trim()), "{state}");
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() {
            let rendered = std::fs::read_to_string(&path).unwrap();
            assert!(
                !rendered.contains(private_key.trim()),
                "{} carries the private key",
                path.display()
            );
        }
    }
}

#[tokio::test]
pub(super) async fn live_state_names_the_kind_of_every_entry() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[
        ("br0", bridge_iface(&["eth1"])),
        ("eth0", dhcp_iface()),
        ("eth0.100", vlan_iface("eth0", 100)),
        ("eth1", port_iface()),
    ]);

    let state = reconciler.apply(&settings).await.unwrap();

    // The reader of this field is the pane; the writer is here, beside the public key it sits next to.
    assert_eq!(state["br0"]["kind"], "bridge");
    assert_eq!(state["eth0"]["kind"], "physical");
    assert_eq!(state["eth0.100"]["kind"], "vlan");
    assert_eq!(state["eth1"]["kind"], "physical");
    // A physical entry has no public key to publish.
    assert!(state["eth0"].get("publicKey").is_none());
}

#[tokio::test]
pub(super) async fn recreates_a_tunnel_whose_peers_changed_and_tears_down_a_removed_one() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    reconciler
        .apply(&settings_with(&[(
            "wg0",
            wireguard_iface(Some(51820), vec![peer(1)]),
        )]))
        .await
        .unwrap();

    reconciler
        .apply(&settings_with(&[(
            "wg0",
            wireguard_iface(Some(51820), vec![peer(1), peer(2)]),
        )]))
        .await
        .unwrap();
    reconciler.apply(&Settings::default()).await.unwrap();

    assert!(!dir.path().join("50-mica-wg0.netdev").exists());
    // A changed netdev is a recreated device, and a swept one is a deleted
    // device: a tunnel joins the same mechanism the VLAN uses.
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            "reload".to_string(),
            "del wg0".to_string(),
            "reload".to_string(),
            "del wg0".to_string(),
            "reload".to_string(),
        ]
    );
    // The key outlives the entry: re-declaring wg0 keeps the identity the
    // far end already trusts, and the file is unreachable to everything
    // but networkd meanwhile.
    assert!(dir.path().join("networkd-secrets/wg-wg0.key").exists());
}

#[tokio::test]
pub(super) async fn rejects_a_wireguard_entry_that_carries_no_wireguard_block() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[(
        "wg0",
        IfaceSettings {
            kind: IfaceKind::Wireguard,
            dhcp: false,
            ..IfaceSettings::default()
        },
    )]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    // The tunnel rule: the block
    // carries the peers, so an entry without one is a link that could
    // never carry a packet.
    assert!(
        err.to_string().contains("carries no wireguard block"),
        "{err}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
pub(super) async fn rejects_a_peer_whose_public_key_is_not_a_key() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let mut broken = peer(1);
    // A newline here would land verbatim on the PublicKey= line, where it
    // starts a new networkd directive.
    broken.public_key = "AAAA\nEndpoint=10.0.0.1:1".to_string();
    let settings = settings_with(&[("wg0", wireguard_iface(None, vec![broken]))]);

    let err = reconciler.apply(&settings).await.unwrap_err();

    assert!(
        err.to_string()
            .contains("peer 0 has a public key that is not a WireGuard key"),
        "{err}"
    );
    // The peer is named by its index, never by the value: a private key
    // pasted into this field must not come back out in an error.
    assert!(!err.to_string().contains("AAAA"), "{err}");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
pub(super) async fn rejects_a_peer_allowed_ip_and_endpoint_that_are_not_what_they_claim() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let mut bad_ip = peer(1);
    bad_ip.allowed_ips = vec!["10.8.0.0/33".to_string()];
    let mut bad_endpoint = peer(1);
    bad_endpoint.endpoint = Some("vpn.example.net:51820\nPublicKey=x".to_string());

    let ip_err = reconciler
        .apply(&settings_with(&[(
            "wg0",
            wireguard_iface(None, vec![bad_ip]),
        )]))
        .await
        .unwrap_err();
    let endpoint_err = reconciler
        .apply(&settings_with(&[(
            "wg0",
            wireguard_iface(None, vec![bad_endpoint]),
        )]))
        .await
        .unwrap_err();

    assert!(
        ip_err.to_string().contains("is not an IP address or CIDR"),
        "{ip_err}"
    );
    assert!(
        endpoint_err.to_string().contains("is not host:port"),
        "{endpoint_err}"
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[tokio::test]
pub(super) async fn rotation_draws_a_new_key_deletes_the_device_and_never_logs_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("wg0", wireguard_iface(Some(51820), vec![peer(1)]))]);
    let before = reconciler.apply(&settings).await.unwrap();
    let old_private_key =
        std::fs::read_to_string(dir.path().join("networkd-secrets/wg-wg0.key")).unwrap();
    let rotation = KeyRotation::new(
        keystore_in(dir.path()),
        MockLink {
            calls: Arc::clone(&calls),
        },
    );

    let public_key = rotation.rotate_key("wg0").await.unwrap();
    let after = reconciler.apply(&settings).await.unwrap();

    // networkd reads the key when it creates the device, so the device
    // holding the old key is deleted and the reconcile that follows builds
    // it back around the new one.
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            "reload".to_string(),
            "del wg0".to_string(),
            "reload".to_string(),
        ]
    );
    assert_ne!(public_key, before["wg0"]["publicKey"].as_str().unwrap());
    assert_eq!(after["wg0"]["publicKey"], public_key);
    // The old private key is gone from disk; there is no history beside it.
    let new_private_key =
        std::fs::read_to_string(dir.path().join("networkd-secrets/wg-wg0.key")).unwrap();
    assert_ne!(new_private_key, old_private_key);
    assert_eq!(
        std::fs::read_dir(dir.path().join("networkd-secrets"))
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
pub(super) async fn a_rotation_that_cannot_delete_the_device_is_an_error_that_names_no_key() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let rotation = KeyRotation::new(
        keystore_in(dir.path()),
        FailingLink {
            calls: Arc::clone(&calls),
        },
    );

    let err = rotation.rotate_key("wg0").await.unwrap_err();

    // Unlike the sweep's best-effort delete, this one is fatal: the key on
    // disk and the key in the kernel have diverged, and the caller would
    // otherwise be handed a public key the tunnel is not using.
    assert!(
        err.to_string().contains("networkctl delete wg0 failed"),
        "{err}"
    );
    let private_key =
        std::fs::read_to_string(dir.path().join("networkd-secrets/wg-wg0.key")).unwrap();
    assert!(!format!("{err:#}").contains(private_key.trim()), "{err:#}");
}

#[tokio::test]
pub(super) async fn a_rotation_refuses_a_name_that_would_escape_the_key_directory() {
    let dir = tempfile::tempdir().unwrap();
    let rotation = KeyRotation::new(keystore_in(dir.path()), NoDelete);

    let err = rotation.rotate_key("../evil").await.unwrap_err();

    // The name reaches a file path and a command argument here exactly as
    // it does on the render path, so it is checked here too.
    assert!(err.to_string().contains("interface"), "{err}");
    assert!(!dir.path().join("secrets").exists());
}

#[tokio::test]
pub(super) async fn the_only_log_line_a_tunnel_can_produce_carries_no_key() {
    let dir = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    // A deleter that fails is the one path in this reconciler that logs at
    // all, and a tunnel being torn down is when it fires.
    let reconciler = NetworkReconciler::new(
        dir.path().to_path_buf(),
        MockReload {
            calls: Arc::clone(&calls),
        },
        FailingLink {
            calls: Arc::clone(&calls),
        },
        keystore_in(dir.path()),
    );
    let settings = settings_with(&[("wg0", wireguard_iface(Some(51820), vec![peer(1)]))]);
    reconciler.apply(&settings).await.unwrap();
    let private_key =
        std::fs::read_to_string(dir.path().join("networkd-secrets/wg-wg0.key")).unwrap();

    let logs = LogCapture::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .without_time()
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    reconciler.apply(&Settings::default()).await.unwrap();
    drop(guard);

    let captured = logs.text();
    assert!(
        captured.contains("could not delete network device"),
        "{captured}"
    );
    assert!(!captured.contains(private_key.trim()), "{captured}");
    // The tear-down is reported and the apply still converges, which is
    // the property that keeps one dead device from failing every other
    // interface.
    assert!(captured.contains("wg0"), "{captured}");
}

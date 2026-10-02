//! Network interfaces, their kinds and their quoted keys.

use micad_settings::{
    BridgeConfig, IfaceKind, IfaceSettings, NETWORK_SCHEMA_VERSION, Settings, SettingsError,
    StaticConfig, VlanConfig, WireguardConfig, WireguardPeer, json_path_get,
};
use serde_json::json;
use std::fs;

use super::*;

/// The tree the constant above describes, typed.
pub(super) fn every_kind() -> Settings {
    Settings {
        hostname: "edge-1".to_string(),
        network: [
            (
                "eth0".to_string(),
                IfaceSettings {
                    dhcp: true,
                    ..IfaceSettings::default()
                },
            ),
            (
                "eth0.100".to_string(),
                IfaceSettings {
                    kind: IfaceKind::Vlan,
                    dhcp: false,
                    static_: Some(StaticConfig {
                        address: "192.168.100.2/24".to_string(),
                        gateway: None,
                        dns: Vec::new(),
                    }),
                    vlan: Some(VlanConfig {
                        parent: "eth0".to_string(),
                        id: 100,
                    }),
                    ..IfaceSettings::default()
                },
            ),
            (
                "br0".to_string(),
                IfaceSettings {
                    kind: IfaceKind::Bridge,
                    dhcp: true,
                    bridge: Some(BridgeConfig {
                        ports: vec!["eth1".to_string(), "eth2".to_string()],
                    }),
                    ..IfaceSettings::default()
                },
            ),
            (
                "wg0".to_string(),
                IfaceSettings {
                    kind: IfaceKind::Wireguard,
                    dhcp: false,
                    wireguard: Some(WireguardConfig {
                        listen_port: Some(51820),
                        peers: vec![WireguardPeer {
                            public_key: WG_PEER_PUBLIC_KEY.to_string(),
                            allowed_ips: vec!["10.8.0.0/24".to_string()],
                            endpoint: Some("vpn.example.net:51820".to_string()),
                            persistent_keepalive: Some(25),
                        }],
                    }),
                    ..IfaceSettings::default()
                },
            ),
        ]
        .into_iter()
        .collect(),
        ..Settings::default()
    }
}

pub(super) const WG_PEER_PUBLIC_KEY: &str = "AI9C8xytM2fi+RUcnV5RvMnSq4ZQffgDZ37h0vc0AU8=";

/// A tree of physical interfaces serializes exactly as v6 wrote it: no `kind`,
/// no empty blocks. This is what makes the v6 -> v7 bump additive, so it is
/// asserted on the bytes rather than inferred from the attributes.
#[test]
pub(super) fn a_physical_tree_carries_no_trace_of_the_new_fields() {
    let text = toml::to_string(&populated()).unwrap();

    for key in ["kind", "vlan", "bridge", "wireguard"] {
        assert!(!text.contains(key), "{key} was written out: {text}");
    }
    // And an absent `kind` reads back as physical.
    let parsed: Settings = toml::from_str(&text).unwrap();
    assert_eq!(parsed.network["eth0"].kind, IfaceKind::Physical);
    assert_eq!(parsed, populated());
}

/// Every kind round-trips through the store: written, read back typed, and
/// spelled on disk the way the design spells it.
#[test]
pub(super) fn all_three_kinds_round_trip_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    store.save(&every_kind()).unwrap();

    let settings = store.load().unwrap();
    assert_eq!(settings, every_kind());

    // Spelled on disk the way the design spells it, in the ONE document that
    // carries the subtree: the kind discriminant beside its block, and no
    // other document touched.
    let written = config_document(&dir, "network.json");
    assert_eq!(written["network"]["eth0.100"]["kind"], json!("vlan"));
    assert_eq!(
        written["network"]["eth0.100"]["vlan"],
        json!({"parent": "eth0", "id": 100})
    );
    assert_eq!(
        written["network"]["br0"]["bridge"]["ports"],
        json!(["eth1", "eth2"])
    );
    assert_eq!(
        written["network"]["wg0"]["wireguard"]["listenPort"],
        json!(51820)
    );
    assert_eq!(store.load().unwrap(), settings);
}

/// The dot-path accessor reaches into each new block, quoted segment and all.
#[test]
pub(super) fn dot_paths_reach_the_new_interface_blocks() {
    let settings = every_kind();

    // A physical entry does not serialize `kind` at all, so there is no node
    // at that path: absent IS physical, and the accessor says so rather than
    // inventing a default the file does not carry.
    assert!(matches!(
        settings.get("network.eth0.kind"),
        Err(SettingsError::NotFound(_))
    ));
    assert_eq!(
        settings.get(r#"network."eth0.100".kind"#).unwrap(),
        json!("vlan")
    );
    assert_eq!(
        settings.get(r#"network."eth0.100".vlan.parent"#).unwrap(),
        json!("eth0")
    );
    assert_eq!(
        settings.get(r#"network."eth0.100".vlan.id"#).unwrap(),
        json!(100)
    );
    assert_eq!(
        settings.get("network.br0.bridge.ports").unwrap(),
        json!(["eth1", "eth2"])
    );
    assert_eq!(
        settings.get("network.wg0.wireguard.listenPort").unwrap(),
        json!(51820)
    );
    assert_eq!(
        settings.get("network.wg0.wireguard.peers").unwrap(),
        json!([{
            "publicKey": WG_PEER_PUBLIC_KEY,
            "allowedIps": ["10.8.0.0/24"],
            "endpoint": "vpn.example.net:51820",
            "persistentKeepalive": 25,
        }])
    );
}

/// A whole VLAN entry can be written through `set`, and an unknown key inside
/// a block is still refused: `deny_unknown_fields` survives the extension,
/// which is the reason the schema carries a `kind` field instead of a
/// serde-tagged enum.
#[test]
pub(super) fn set_writes_a_vlan_entry_and_still_refuses_an_unknown_key() {
    let mut settings = Settings::default();
    settings
        .set(
            r#"network."eth0.100""#,
            json!({
                "kind": "vlan",
                "dhcp": false,
                "vlan": {"parent": "eth0", "id": 100},
            }),
        )
        .unwrap();
    assert_eq!(
        settings.network["eth0.100"],
        IfaceSettings {
            kind: IfaceKind::Vlan,
            dhcp: false,
            vlan: Some(VlanConfig {
                parent: "eth0".to_string(),
                id: 100,
            }),
            ..IfaceSettings::default()
        }
    );

    let before = settings.clone();
    let err = settings
        .set(r#"network."eth0.100".vlan.protocol"#, json!("802.1ad"))
        .unwrap_err();
    assert!(matches!(err, SettingsError::Validation { .. }), "{err:?}");
    assert_eq!(settings, before);

    // And a kind the schema does not name is a validation error, not a silent
    // fallback to physical.
    let err = settings
        .set(r#"network."eth0.100".kind"#, json!("macvlan"))
        .unwrap_err();
    assert!(matches!(err, SettingsError::Validation { .. }), "{err:?}");
    assert_eq!(settings, before);
}

/// **There is no private-key field in this schema.** The absence is the
/// mechanism — a field here is a value published to every bus client — so it
/// is asserted, not assumed.
#[test]
pub(super) fn the_wireguard_subtree_holds_no_secret() {
    let text = toml::to_string(&every_kind()).unwrap();
    for secret in ["privateKey", "private_key", "presharedKey", "presharedkey"] {
        assert!(!text.contains(secret), "{secret} is in the tree: {text}");
    }
}

// --- Quoted path segments --------------------------------------------------

/// A VLAN-named entry: unquoted, the dot-path would split `eth0.100` into two
/// segments and fail with `unknown field \`100\``. Quoted, it lands on the key
/// `eth0.100`, and every layer — get, set of a leaf inside it, TOML
/// persistence, reload — spells it the same way.
#[test]
pub(super) fn a_quoted_segment_round_trips_a_dotted_interface_key() {
    let mut settings = Settings::default();
    settings
        .set(
            r#"network."eth0.100""#,
            json!({"dhcp": false, "static": {"address": "192.168.100.2/24", "dns": []}}),
        )
        .unwrap();
    assert_eq!(
        settings.network.keys().collect::<Vec<_>>(),
        vec!["eth0.100"]
    );
    assert_eq!(
        settings
            .get(r#"network."eth0.100".static.address"#)
            .unwrap(),
        json!("192.168.100.2/24")
    );

    settings
        .set(r#"network."eth0.100".dhcp"#, json!(true))
        .unwrap();
    assert!(settings.network["eth0.100"].dhcp);

    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    store.save(&settings).unwrap();
    let doc = config_document(&dir, "network.json");
    assert_eq!(
        doc["network"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec!["eth0.100"],
        "persistence spells the key the way the path does: {doc}"
    );
    assert_eq!(store.load().unwrap(), settings);
}

/// The unquoted spelling still means what it always meant: two segments, so
/// `100` is looked up as a field of `IfaceSettings`.
#[test]
pub(super) fn an_unquoted_dotted_key_is_still_a_field_of_the_interface() {
    let mut settings = Settings::default();
    let err = settings.set("network.eth0.100", json!({"dhcp": true}));
    assert!(
        matches!(&err, Err(SettingsError::Validation { message, .. }) if message.contains("unknown field `100`")),
        "{err:?}"
    );
    assert!(settings.network.is_empty());
    assert!(matches!(
        settings.get("network.eth0.100"),
        Err(SettingsError::NotFound(_))
    ));
}

/// Quoting is a grammar, not a string search: a segment is quoted whole or
/// not at all, and anything else is a malformed path rather than a key.
#[test]
pub(super) fn malformed_quoting_is_a_path_error_on_both_sides() {
    let tree = json!({"network": {"eth0.100": {"dhcp": true}, "\"odd\"": 1}});
    assert_eq!(
        json_path_get(&tree, r#"network."eth0.100".dhcp"#),
        Some(&json!(true))
    );
    for path in [
        r#"network."eth0.100"#,   // unterminated
        r#"network."eth0.100"x"#, // trailing text after the closing quote
        r#"network.eth"0.100""#,  // a quote inside a bare segment
        r#"network."""#,          // an empty quoted segment
        r#"network.""#,           // a lone quote
    ] {
        assert_eq!(json_path_get(&tree, path), None, "{path}");
        let mut settings = Settings::default();
        assert!(
            matches!(
                settings.set(path, json!(true)),
                Err(SettingsError::NotFound(_))
            ),
            "{path}"
        );
    }
}

/// A `network` key the reconciler would refuse never reaches the tree: the
/// write is rejected, including the one key the path syntax cannot spell.
#[test]
pub(super) fn set_rejects_a_network_key_that_is_not_an_interface_name() {
    let mut settings = Settings::default();
    for key in [
        "eth0 100",
        "eth0/100",
        "waytoolongiface016",
        ".",
        "..",
        "eth\"0",
    ] {
        let path = format!("network.\"{key}\"");
        let err = settings.set(&path, json!({"dhcp": true}));
        assert!(
            matches!(
                err,
                Err(SettingsError::Validation { .. }) | Err(SettingsError::NotFound(_))
            ),
            "{key}: {err:?}"
        );
    }
    assert!(settings.network.is_empty());
    settings
        .set(r#"network."eth0.100""#, json!({"dhcp": true}))
        .unwrap();
    settings
        .set("network.br-lan:0", json!({"dhcp": true}))
        .unwrap();
}

/// The rule is a property of the write, not of the tree: a key that a hand
/// edit put there before the rule existed still loads, and still does not
/// stand between an operator and an unrelated write.
#[test]
pub(super) fn a_key_that_predates_the_rule_does_not_block_other_writes() {
    let dir = tempfile::tempdir().unwrap();
    let store = store_at(&dir);
    fs::write(
        dir.path().join("config/network.json"),
        format!(
            r#"{{"schema_version": {NETWORK_SCHEMA_VERSION}, "network": {{"eth0 100": {{"dhcp": true}}}}}}"#
        ),
    )
    .unwrap();
    let mut settings = store.load().unwrap();
    assert!(settings.network.contains_key("eth0 100"));

    settings.set("hostname", json!("edge-1")).unwrap();
    assert_eq!(settings.hostname, "edge-1");
    assert!(settings.network.contains_key("eth0 100"));

    // Touching that entry is a write, and the write is refused.
    assert!(matches!(
        settings.set(r#"network."eth0 100".dhcp"#, json!(false)),
        Err(SettingsError::Validation { .. })
    ));
}

// --- Bearer API tokens (schema v8) -----------------------------------------

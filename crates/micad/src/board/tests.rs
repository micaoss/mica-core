use super::*;

const CX3576: &str = r#"{
  "version": 1,
  "interfaces": [
    { "name": "eth0", "kind": "ethernet", "label": "LAN1", "speedMbps": 1000 },
    { "name": "eth1", "kind": "ethernet", "label": "LAN2", "role": "internal" },
    { "name": "wlan0", "kind": "wifi", "label": "Wi-Fi", "ap": true, "bands": ["2.4", "5"] },
    { "name": "can0", "kind": "can", "label": "CAN", "fd": true },
    { "name": "eth9", "kind": "ethernet", "label": "Debug", "hidden": true }
  ]
}"#;

fn board() -> Board {
    Board::parse(CX3576).unwrap()
}

#[test]
fn a_declaration_reads_its_ports_in_order() {
    let Board::Declared(ports) = board() else {
        panic!("declared");
    };
    let names: Vec<&str> = ports.iter().map(|port| port.name.as_str()).collect();
    assert_eq!(names, ["eth0", "eth1", "wlan0", "can0", "eth9"]);
    assert_eq!(
        Board::parse(r#"{"version":1,"discover":true}"#),
        Ok(Board::Discover)
    );
}

#[test]
fn a_declaration_this_build_does_not_know_is_refused() {
    for (text, why) in [
        (r#"{"version":2,"discover":true}"#, "version"),
        (
            r#"{"version":1,"discover":true,"extra":1}"#,
            "unknown field",
        ),
        (r#"{"version":1}"#, "declares its interfaces"),
        (
            r#"{"version":1,"interfaces":[]}"#,
            "declares its interfaces",
        ),
        (
            r#"{"version":1,"discover":true,"interfaces":[]}"#,
            "discovers",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"br0","kind":"bridge","label":"B"}]}"#,
            "unknown variant",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"eth0","kind":"ethernet","label":"A"},{"name":"eth0","kind":"ethernet","label":"B"}]}"#,
            "twice",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"eth0","kind":"ethernet","label":"A very long label"}]}"#,
            "label",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"eth0","kind":"ethernet","label":""}]}"#,
            "label",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"can0","kind":"can","label":"CAN","role":"uplink"}]}"#,
            "role is not for a can port",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"eth0","kind":"ethernet","label":"A","fd":true}]}"#,
            "fd is not for a ethernet port",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"wlan0","kind":"wifi","label":"W","bands":["2.4","2.4"]}]}"#,
            "bands",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"wlan0","kind":"wifi","label":"W","bands":["60"]}]}"#,
            "bands",
        ),
        (
            r#"{"version":1,"interfaces":[{"name":"eth0","kind":"ethernet","label":"A","match":{}}]}"#,
            "unknown field",
        ),
    ] {
        let err = Board::parse(text).unwrap_err();
        assert!(err.contains(why), "{text}: {err}");
    }
}

#[test]
fn an_internal_port_stays_off_the_internet_until_its_entry_says_otherwise() {
    let board = board();
    let network = BTreeMap::from([
        ("eth0".to_string(), IfaceSettings::default()),
        ("eth1".to_string(), IfaceSettings::default()),
    ]);
    let effective = board.effective(&network);
    assert_eq!(effective["eth0"].internet, None);
    assert_eq!(effective["eth1"].internet, Some(false));
    let chosen = BTreeMap::from([(
        "eth1".to_string(),
        IfaceSettings {
            internet: Some(true),
            ..IfaceSettings::default()
        },
    )]);
    assert_eq!(board.effective(&chosen)["eth1"].internet, Some(true));
    assert!(board.internal("eth1") && !board.internal("eth0"));
    assert!(!Board::Discover.internal("eth1"));
}

#[test]
fn the_observation_gains_the_declaration_and_the_ports_the_device_lacks() {
    let mut observed = json!({
        "interfaces": { "available": true, "entries": [
            { "name": "eth0", "present": true },
            { "name": "eth9", "present": true },
            { "name": "eth2", "present": true },
        ] },
    });
    board().annotate(&mut observed, &BTreeSet::new());
    assert_eq!(observed["board"], "declared");
    let entries = observed["interfaces"]["entries"].as_array().unwrap();
    let names: Vec<&str> = entries
        .iter()
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["eth0", "eth2", "eth1", "wlan0", "can0"]);
    assert_eq!(
        entries[0]["declared"],
        json!({ "kind": "ethernet", "label": "LAN1", "order": 0, "role": "uplink", "capabilities": { "speedMbps": 1000 } })
    );
    assert_eq!(entries[1]["declared"], Value::Null);
    assert_eq!(entries[2]["present"], false);
    assert_eq!(entries[2]["declared"]["role"], "internal");
    assert_eq!(
        entries[3]["declared"]["capabilities"],
        json!({ "ap": true, "bands": ["2.4", "5"] })
    );
    assert_eq!(
        entries[4]["declared"],
        json!({ "kind": "can", "label": "CAN", "order": 3, "capabilities": { "fd": true } })
    );
}

#[test]
fn a_hidden_port_is_listed_only_when_the_tree_has_an_entry_for_it() {
    let mut observed =
        json!({ "interfaces": { "available": true, "entries": [{ "name": "eth9" }] } });
    board().annotate(&mut observed, &BTreeSet::from(["eth9".to_string()]));
    assert_eq!(
        observed["interfaces"]["entries"][0]["declared"]["label"],
        "Debug"
    );
    assert!(board().hidden("eth9"));
}

#[test]
fn a_discovering_board_says_so_and_declares_nothing() {
    let mut observed = json!({ "interfaces": { "available": false, "detail": "no" } });
    Board::Discover.annotate(&mut observed, &BTreeSet::new());
    assert_eq!(observed["board"], "discover");
    let mut listed =
        json!({ "interfaces": { "available": true, "entries": [{ "name": "eth0" }] } });
    Board::Discover.annotate(&mut listed, &BTreeSet::new());
    assert_eq!(listed["interfaces"]["entries"][0]["declared"], Value::Null);
}

#[test]
fn a_root_without_a_declaration_or_with_a_refused_one_discovers() {
    let root = tempfile::tempdir().unwrap();
    assert_eq!(Board::load(root.path()), Board::Discover);
    let path = root.path().join(BOARD_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"version":9}"#).unwrap();
    assert_eq!(Board::load(root.path()), Board::Discover);
    std::fs::write(&path, CX3576).unwrap();
    assert_eq!(Board::load(root.path()), board());
}

#[test]
fn the_check_command_reads_a_file_with_the_same_parser() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("interfaces.json");
    std::fs::write(&path, CX3576).unwrap();
    let file = path.display().to_string();
    assert!(main(&["check".to_string(), file.clone()]).is_ok());
    std::fs::write(&path, r#"{"version":1}"#).unwrap();
    assert!(main(&["check".to_string(), file]).is_err());
    assert!(main(&["lint".to_string()]).is_err());
}

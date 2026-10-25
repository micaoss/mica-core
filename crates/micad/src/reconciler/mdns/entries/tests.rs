use std::os::unix::fs::PermissionsExt;

use super::*;

fn entry(kind: &str, port: u16) -> Entry {
    Entry {
        instance: None,
        kind: kind.to_string(),
        port,
        txt: BTreeMap::new(),
        service: None,
    }
}

fn owner() -> u32 {
    rustix::process::geteuid().as_raw()
}

fn write(dir: &Path, name: &str, text: &str) {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
}

const GRAFANA: &str = r#"{"version":1,"services":[
    {"instance":"Grafana","type":"_http._tcp","port":3000,"txt":{"path":"/dashboards"}}]}"#;

#[test]
fn service_types_are_rfc_6335_names_over_tcp_or_udp() {
    for kind in ["_http._tcp", "_ipp._tcp", "_node-red._tcp", "_x1._udp"] {
        assert!(is_type(kind), "{kind}");
    }
    for kind in [
        "http._tcp",
        "_http._sctp",
        "_._tcp",
        "_HTTP._tcp",
        "_-http._tcp",
        "_http-._tcp",
        "_a--b._tcp",
        "_123._tcp",
        "_sixteen-letters1._tcp",
    ] {
        assert!(!is_type(kind), "{kind}");
    }
}

#[test]
fn every_entry_rule_is_held() {
    assert_eq!(check(&entry("_http._tcp", 80)), Ok(()));
    let broken = [
        entry("_mica._tcp", 80),
        entry("_http._tcp", 0),
        entry("http", 80),
        Entry {
            instance: Some("a.b".into()),
            ..entry("_http._tcp", 80)
        },
        Entry {
            instance: Some(String::new()),
            ..entry("_http._tcp", 80)
        },
        Entry {
            txt: (0..17).map(|i| (format!("k{i}"), String::new())).collect(),
            ..entry("_http._tcp", 80)
        },
        Entry {
            txt: BTreeMap::from([("k".into(), "v".repeat(254))]),
            ..entry("_http._tcp", 80)
        },
        Entry {
            txt: BTreeMap::from([("a=b".into(), String::new())]),
            ..entry("_http._tcp", 80)
        },
        Entry {
            txt: (0..6).map(|i| (format!("k{i}"), "v".repeat(250))).collect(),
            ..entry("_http._tcp", 80)
        },
    ];
    for entry in broken {
        assert!(check(&entry).is_err(), "{entry:?}");
    }
    assert!(check_all(&vec![entry("_http._tcp", 80); 9]).is_err());
}

#[test]
fn drop_in_names_are_short_lower_case_words() {
    for name in ["grafana", "node-red", "0x"] {
        assert!(is_name(name), "{name}");
    }
    for name in ["", "-x", "Grafana", "a.b", &"a".repeat(33)] {
        assert!(!is_name(name), "{name}");
    }
}

#[test]
fn a_runtime_file_replaces_the_images_of_the_same_name() {
    let (image, runtime) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    write(image.path(), "grafana.json", GRAFANA);
    write(
        image.path(),
        "printer.json",
        r#"{"version":1,"services":[{"type":"_ipp._tcp","port":631}]}"#,
    );
    write(
        runtime.path(),
        "grafana.json",
        r#"{"version":1,"services":[{"type":"_http._tcp","port":3001}]}"#,
    );
    // Not drop-ins at all: no .json, or not a name.
    write(runtime.path(), "notes.txt", GRAFANA);
    write(runtime.path(), ".grafana.json.tmp", GRAFANA);

    let found = drop_ins(image.path(), runtime.path(), owner());

    assert_eq!(
        found.keys().collect::<Vec<_>>(),
        ["image:printer", "runtime:grafana"]
    );
    assert_eq!(found["runtime:grafana"].as_ref().unwrap()[0].port, 3001);
    assert_eq!(
        found["image:printer"].as_ref().unwrap()[0].kind,
        "_ipp._tcp"
    );
}

#[test]
fn a_broken_or_unsafe_file_is_refused_whole_and_named() {
    let (image, runtime) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    for (name, text) in [
        ("version.json", r#"{"version":2,"services":[]}"#),
        ("unknown.json", r#"{"version":1,"services":[],"extra":1}"#),
        (
            "app-key.json",
            r#"{"version":1,"services":[{"type":"_x._tcp","port":1,"service":"x"}]}"#,
        ),
        (
            "one-bad.json",
            r#"{"version":1,"services":[{"type":"_x._tcp","port":1},{"type":"_mica._tcp","port":1}]}"#,
        ),
        ("garbage.json", "{"),
    ] {
        write(runtime.path(), name, text);
    }
    write(runtime.path(), "writable.json", GRAFANA);
    std::fs::set_permissions(
        runtime.path().join("writable.json"),
        std::fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        runtime.path().join("writable.json"),
        runtime.path().join("link.json"),
    )
    .unwrap();
    write(runtime.path(), "stranger.json", GRAFANA);

    let found = drop_ins(image.path(), runtime.path(), owner());
    for (source, read) in &found {
        if source == "runtime:stranger" {
            assert!(read.is_ok());
        } else {
            assert!(read.is_err(), "{source}: {read:?}");
        }
    }
    assert_eq!(found.len(), 8);

    // Owned by someone else, the same file is refused.
    let found = drop_ins(image.path(), runtime.path(), owner() + 1);
    assert!(found["runtime:stranger"].is_err());
    // The image's files are the root's own and are not held to it.
    write(image.path(), "grafana.json", GRAFANA);
    let found = drop_ins(image.path(), runtime.path(), owner() + 1);
    assert!(found["image:grafana"].is_ok());
}

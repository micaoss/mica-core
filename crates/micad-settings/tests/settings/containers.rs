//! Declared containers in the tree.

/// A minimal unit: everything else defaults, and the defaults are the safe
/// ones -- no command, no ports, no volumes, no restart, no autostart.
pub(super) fn container_unit(image: &str) -> micad_settings::ContainerUnit {
    micad_settings::ContainerUnit {
        image: image.to_string(),
        command: Vec::new(),
        environment: std::collections::BTreeMap::new(),
        publish: Vec::new(),
        volumes: Vec::new(),
        restart: micad_settings::RestartPolicy::default(),
        auto_start: false,
        pids: None,
        memory: None,
        cpu: None,
    }
}

/// A device that declares no container writes the document it wrote before a
/// container could be declared: the field is skipped when empty, which is what
/// makes an A/B rollback survive this addition.
#[test]
pub(super) fn a_device_with_no_containers_serializes_as_it_did_before() {
    let settings = micad_settings::Settings::default();
    let json = serde_json::to_value(&settings.container).unwrap();

    assert_eq!(json, serde_json::json!({ "enabled": false }));
}

#[test]
pub(super) fn a_declared_container_round_trips_through_the_tree() {
    let mut settings = micad_settings::Settings::default();
    settings
        .set(
            "container.units",
            serde_json::json!({
                "node-red": {
                    "image": "docker.io/nodered/node-red:4.0.9",
                    "publish": [{ "host": 1880, "container": 1880 }],
                    "volumes": [{ "host": "/mica/apps/node-red", "container": "/data" }],
                    "restart": "always",
                    "autoStart": true,
                },
            }),
        )
        .expect("a declared container is accepted");

    let unit = &settings.container.units["node-red"];
    assert_eq!(unit.image, "docker.io/nodered/node-red:4.0.9");
    assert_eq!(unit.publish[0].host, 1880);
    assert_eq!(unit.publish[0].protocol, micad_settings::PortProtocol::Tcp);
    assert_eq!(unit.restart, micad_settings::RestartPolicy::Always);
    assert!(unit.auto_start);
}

/// The four refusals, each of them a container the device could not run.
#[test]
pub(super) fn a_container_map_the_device_could_not_run_is_refused() {
    let mut units = std::collections::BTreeMap::new();

    // A name that cannot be a unit.
    units.insert("node red".to_string(), container_unit("alpine:3"));
    assert!(micad_settings::validate_container_units(&units).is_err());
    units.clear();

    // No image.
    units.insert("app".to_string(), container_unit("   "));
    assert!(micad_settings::validate_container_units(&units).is_err());
    units.clear();

    // Two containers claiming one host port.
    let mut first = container_unit("alpine:3");
    first.publish = vec![micad_settings::PublishedPort {
        host: 8080,
        container: 80,
        protocol: micad_settings::PortProtocol::Tcp,
    }];
    let mut second = container_unit("alpine:3");
    second.publish = first.publish.clone();
    units.insert("a".to_string(), first);
    units.insert("b".to_string(), second);
    let message = micad_settings::validate_container_units(&units).unwrap_err();
    assert!(message.contains("8080/tcp"), "{message}");
    units.clear();

    // A volume outside the operator's half of the device, and one that climbs
    // out of it.
    for host in ["/etc", "/mica/../etc"] {
        let mut unit = container_unit("alpine:3");
        unit.volumes = vec![micad_settings::VolumeMount {
            host: host.to_string(),
            container: "/data".to_string(),
            read_only: false,
        }];
        units.insert("app".to_string(), unit);
        let message = micad_settings::validate_container_units(&units).unwrap_err();
        assert!(
            message.contains(micad_settings::CONTAINER_VOLUME_ROOT),
            "{message}"
        );
        units.clear();
    }
}

/// The bound is on the write and not only on apid, because the document is
/// writable without apid.
#[test]
pub(super) fn a_volume_outside_mica_is_refused_by_the_tree_itself() {
    let mut settings = micad_settings::Settings::default();
    let error = settings
        .set(
            "container.units",
            serde_json::json!({
                "app": {
                    "image": "alpine:3",
                    "volumes": [{ "host": "/etc", "container": "/host-etc" }],
                },
            }),
        )
        .expect_err("a root bind must be refused");

    assert!(format!("{error}").contains("/mica/"), "{error}");
    assert!(settings.container.units.is_empty());
}

// --- Bluetooth ---------------------------------------------------------------

/// A limit podman would refuse or misread is refused here, naming the
/// container; the ones it takes are kept.
#[test]
pub(super) fn container_limits_are_held_to_what_podman_takes() {
    let with = |pids: Option<u32>, memory: Option<&str>, cpu: Option<&str>| {
        let mut unit = container_unit("docker.io/library/busybox:1");
        unit.pids = pids;
        unit.memory = memory.map(str::to_string);
        unit.cpu = cpu.map(str::to_string);
        micad_settings::validate_container_units(&std::collections::BTreeMap::from([(
            "app".to_string(),
            unit,
        )]))
    };
    for (pids, memory, cpu) in [
        (Some(1), None, None),
        (Some(65536), None, None),
        (None, Some("6m"), None),
        (None, Some("512M"), None),
        (None, Some("2g"), None),
        (None, Some("65536k"), None),
        (None, None, Some("0.5")),
        (None, None, Some("2")),
        (None, None, Some("1.125")),
    ] {
        assert!(
            with(pids, memory, cpu).is_ok(),
            "{pids:?} {memory:?} {cpu:?}"
        );
    }
    for (pids, memory, cpu, word) in [
        (Some(0), None, None, "pids"),
        (Some(65537), None, None, "pids"),
        (None, Some("5m"), None, "memory"),
        (None, Some("512"), None, "memory"),
        (None, Some("0512m"), None, "memory"),
        (None, Some("1.5g"), None, "memory"),
        (None, Some("-1m"), None, "memory"),
        (None, None, Some("0"), "cpu"),
        (None, None, Some(".5"), "cpu"),
        (None, None, Some("0.1234"), "cpu"),
        (None, None, Some("1."), "cpu"),
        (None, None, Some("2 --privileged"), "cpu"),
        (None, None, Some("5000"), "cpu"),
    ] {
        let error = with(pids, memory, cpu).unwrap_err();
        assert!(error.contains("\"app\"") && error.contains(word), "{error}");
    }
}

//! The status document and the boot policy's tiers.

use super::*;

/// The served shape, member by member. The consumers are apid's route in
/// another crate and the UI beyond it, and they read these names.
#[test]
pub(super) fn the_status_json_carries_every_tier_and_names_the_absent_ones() {
    let mut tiers = BTreeMap::new();
    tiers.insert(
        "data".to_string(),
        TierEvidence {
            device: Some("/dev/mmcblk0p11".to_string()),
            partition_bytes: Some(30_000_000_000),
            mount: Some(mounted("/dev/mmcblk0p11", DATA_MOUNT)),
            space: Some(space(1000, 850, 100)),
            check: Some(CheckEvidence {
                unit: "systemd-fsck@dev-mmcblk0p11.service".to_string(),
                active_state: Some("inactive".to_string()),
                result: Some("success".to_string()),
                exit_status: Some(1),
            }),
            partition_label: None,
        },
    );
    tiers.insert(
        "system".to_string(),
        TierEvidence {
            device: Some("/dev/mmcblk0p6".to_string()),
            partition_bytes: Some(268_435_456),
            mount: Some(MountEvidence {
                device: "/dev/mmcblk0p6".to_string(),
                root: "/".to_string(),
                mount: "/mnt/system".to_string(),
                fstype: "ext4".to_string(),
                read_only: true,
            }),
            ..TierEvidence::default()
        },
    );
    let evidence = StorageEvidence {
        directory_bytes: BTreeMap::new(),
        project_quotas: None,
        tiers,
        binds: [("mica".to_string(), bound("/dev/mmcblk0p11"))]
            .into_iter()
            .collect(),
        media: vec![MediumEvidence {
            name: "mmcblk0".to_string(),
            kind: "mmc".to_string(),
            size_bytes: Some(31_000_000_000),
            model: Some("SDINBDA4".to_string()),
            rotational: Some(false),
            health: MediaHealth::Emmc {
                life_time_raw: "0x01 0x01".to_string(),
                pre_eol_raw: Some("0x01".to_string()),
            },
        }],
    };
    let value = status_json(&evidence, &PressureTracker::default());

    let tiers = value["tiers"].as_array().expect("tiers is an array");
    assert_eq!(tiers.len(), TIERS.len(), "every tier is reported: {value}");
    let by_name = |name: &str| {
        tiers
            .iter()
            .find(|tier| tier["name"] == name)
            .unwrap_or_else(|| panic!("{name} is missing from {value}"))
            .clone()
    };

    let data = by_name("data");
    assert_eq!(data["present"], true);
    assert_eq!(data["mounted"], true);
    // The DATA partition's own mountpoint, not either bind's: the layout
    // puts the filesystem at /mnt/data and exposes /mica and /srv on top.
    assert_eq!(data["mount"], DATA_MOUNT);
    assert_eq!(data["readOnly"], false);
    assert_eq!(data["role"], "ext4");
    assert_eq!(data["space"]["reservedBytes"], 50);
    assert_eq!(data["space"]["usedPercent"], 85);
    assert_eq!(data["pressure"], "warning");

    // One filesystem, multiple namespaces. The binds carry no capacity of
    // their own -- a `space` object on either would be the DATA tier's
    // bytes reported a second time, and a reader summing the three would
    // get three times the disk.
    let namespaces = &value["namespaces"];
    assert_eq!(namespaces["sharedCapacityTier"], DATA_TIER);
    let binds = namespaces["binds"].as_array().expect("binds is an array");
    assert_eq!(binds.len(), BINDS.len());
    for bind in binds {
        assert!(
            bind.get("space").is_none(),
            "a bind reported capacity: {bind}"
        );
        assert!(
            bind.get("partitionBytes").is_none(),
            "a bind reported a partition size: {bind}"
        );
    }
    // fsck exit 1 is "errors were corrected", which is real repair
    // evidence and is surfaced rather than folded into a boolean.
    assert_eq!(data["check"]["exitStatus"], 1);
    assert_eq!(data["check"]["result"], "success");

    // SYSTEM holds immutable deployment objects and stays read-only.
    let system = by_name("system");
    assert_eq!(system["mount"], "/mnt/system");
    assert_eq!(system["readOnly"], true);
    assert_eq!(system["partitionBytes"], 268_435_456u64);
    // No mounted filesystem to measure, so no invented space object.
    assert!(system.get("space").is_none(), "{system}");
    // Never checked, and it says so rather than reading as clean.
    assert_eq!(system["check"]["recorded"], false);

    // A tier this board does not have is present in the array and says
    // it is absent; omitting it would leave the reader to guess.
    let esp = by_name("esp");
    assert_eq!(esp["present"], false);
    assert!(esp["detail"].as_str().unwrap().contains("esp"), "{esp}");

    // Only the watched tiers carry a pressure classification; the others
    // would need thresholds nobody has set.
    assert!(by_name("system").get("pressure").is_none());

    assert_eq!(value["media"][0]["kind"], "mmc");
    assert_eq!(value["media"][0]["health"]["supported"], true);
    assert_eq!(value["policy"]["warningPercent"], WARNING_ENTER_PERCENT);
}

#[test]
pub(super) fn storage_does_not_invent_an_update_space_reservation() {
    let mut evidence = StorageEvidence::default();
    evidence.tiers.insert(
        "data".into(),
        TierEvidence {
            space: Some(space(1_000_000_000, 100_000_000, 900_000_000)),
            ..TierEvidence::default()
        },
    );
    let value = status_json(&evidence, &PressureTracker::default());
    let data = value["tiers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "data")
        .unwrap();
    assert!(data.get("updateWorkspace").is_none());
    assert!(
        value["policy"]
            .get("updateWorkspaceReservedBytes")
            .is_none()
    );
}

/// Every lifecycle decision is answered, and every current
/// answer is `unsupported`. The list is asserted by name so that adding a
/// capability without deciding its lifecycle answer fails here.
#[test]
pub(super) fn every_lifecycle_decision_is_explicit_and_currently_unsupported() {
    let value = status_json(&StorageEvidence::default(), &PressureTracker::default());
    let lifecycle = value["lifecycle"]
        .as_object()
        .expect("lifecycle is an object");
    let mut names: Vec<&str> = lifecycle.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "backupRestore",
            "dataPreservingReplacement",
            "encryption",
            "factoryReset",
            "offlineRepair",
            "removableMedia",
            "secureErase",
        ]
    );
    for (name, decision) in lifecycle {
        assert_eq!(decision, "unsupported", "{name} claims more than it has");
    }
}

/// The observer's assembly — label to device to mount to medium — over a
/// fixture sysfs, so the pairing that would break silently on a device is
/// A board whose disk names its partitions its own way and puts a vendor
/// partition ahead of SYSTEM: the signed boot policy's UUIDs and numbers
/// find every tier, and the names reported are the disk's own.
#[tokio::test]
pub(super) async fn the_boot_policy_finds_the_tiers_whatever_the_board_names_them() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path();
    let write = |relative: &str, contents: &str| {
        let target = path.join(relative);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
        std::fs::write(target, contents).expect("write");
    };
    write("sys/block/mmcblk0/size", "60000000\n");
    for (number, name, size) in [
        (1, "loader", 36800),
        (2, "vendor", 2048),
        (3, "rootfs-store", 524288),
        (4, "userdata", 40000000),
    ] {
        let part = format!("sys/block/mmcblk0/mmcblk0p{number}");
        write(&format!("{part}/partition"), &format!("{number}\n"));
        write(&format!("{part}/size"), &format!("{size}\n"));
        write(
            &format!("{part}/uevent"),
            &format!("MAJOR=179\nPARTN={number}\nPARTNAME={name}\n"),
        );
        std::fs::create_dir_all(path.join("sys/class/block")).expect("mkdir");
        std::os::unix::fs::symlink(
            format!("../../block/mmcblk0/mmcblk0p{number}"),
            path.join(format!("sys/class/block/mmcblk0p{number}")),
        )
        .expect("symlink");
    }
    std::fs::create_dir_all(path.join("dev/disk/by-partuuid")).expect("mkdir");
    std::os::unix::fs::symlink(
        "../../mmcblk0p3",
        path.join("dev/disk/by-partuuid/5ac35760-0003"),
    )
    .expect("symlink");
    std::os::unix::fs::symlink(
        "../../mmcblk0p4",
        path.join("dev/disk/by-partuuid/5ac35760-0004"),
    )
    .expect("symlink");
    write(
        BOOT_POLICY_PATH,
        r#"{"board":{"boot":"uboot-fit","kernel":"fit","partitions":{"boot":1,"system":3,"data":4}},
            "systemPartUuid":"5AC35760-0003","dataPartUuid":"5ac35760-0004","identity":{}}"#,
    );
    write("proc/self/mountinfo", "");

    let evidence = HostStorage::at(path)
        .observe()
        .await
        .expect("the fixture observes");
    let mut names: Vec<&str> = evidence.tiers.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["data", "firmware", "system"],
        "no esp on a FIT board"
    );
    for (tier, device, label) in [
        ("firmware", "/dev/mmcblk0p1", "loader"),
        ("system", "/dev/mmcblk0p3", "rootfs-store"),
        ("data", "/dev/mmcblk0p4", "userdata"),
    ] {
        assert_eq!(
            evidence.tiers[tier].device.as_deref(),
            Some(device),
            "{tier}"
        );
        assert_eq!(
            evidence.tiers[tier].partition_label.as_deref(),
            Some(label),
            "{tier}"
        );
    }
    let status = status_json(&evidence, &PressureTracker::default());
    let firmware = status["tiers"]
        .as_array()
        .expect("tiers")
        .iter()
        .find(|tier| tier["name"] == "firmware")
        .expect("the firmware tier");
    assert_eq!(firmware["partitionLabel"], "loader");
}

#[test]
pub(super) fn a_boot_policy_missing_what_the_observer_needs_is_not_read() {
    let full = r#"{"board":{"boot":"uefi","partitions":{"boot":1}},"systemPartUuid":"aa-1","dataPartUuid":"aa-2"}"#;
    assert_eq!(
        PolicyPartitions::parse(full),
        Some(PolicyPartitions {
            boot_tier: "esp",
            boot_number: 1,
            system_uuid: "aa-1".to_string(),
            data_uuid: "aa-2".to_string(),
        })
    );
    for broken in [
        r#"{"board":{"boot":"grub","partitions":{"boot":1}},"systemPartUuid":"aa-1","dataPartUuid":"aa-2"}"#,
        r#"{"board":{"boot":"uefi"},"systemPartUuid":"aa-1","dataPartUuid":"aa-2"}"#,
        r#"{"board":{"boot":"uefi","partitions":{"boot":1}},"systemPartUuid":"../x","dataPartUuid":"aa-2"}"#,
        "not json",
    ] {
        assert_eq!(PolicyPartitions::parse(broken), None, "{broken}");
    }
}

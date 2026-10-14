//! The host observer.

use serde_json::json;

use super::*;

/// exercised on the build host.
#[tokio::test]
pub(super) async fn the_host_observer_pairs_tiers_with_their_devices_and_media() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path();
    let write = |relative: &str, contents: &str| {
        let target = path.join(relative);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
        std::fs::write(target, contents).expect("write");
    };

    // One eMMC with the two tiers this test cares about.
    write("sys/block/mmcblk0/size", "60000000\n");
    write("sys/block/mmcblk0/queue/rotational", "0\n");
    write("sys/block/mmcblk0/device/name", "SDINBDA4\n");
    write("sys/block/mmcblk0/device/life_time", "0x02 0x01\n");
    write("sys/block/mmcblk0/device/pre_eol_info", "0x01\n");
    write("sys/block/mmcblk0/mmcblk0p6/partition", "6\n");
    write("sys/block/mmcblk0/mmcblk0p6/size", "524288\n");
    write("sys/block/mmcblk0/mmcblk0p11/partition", "11\n");
    write("sys/block/mmcblk0/mmcblk0p11/size", "40000000\n");
    // A loop device, which is not a medium and must not be reported as one.
    write("sys/block/loop0/size", "1024\n");
    // The verity device the root is mounted from, and the slot under it.
    write("sys/block/dm-0/slaves/loop0/.keep", "");

    std::fs::create_dir_all(path.join("dev/disk/by-partlabel")).expect("mkdir");
    std::os::unix::fs::symlink("../../mmcblk0p11", path.join("dev/disk/by-partlabel/data"))
        .expect("symlink");
    std::os::unix::fs::symlink("../../mmcblk0p6", path.join("dev/disk/by-partlabel/system"))
        .expect("symlink");

    // The layout: DATA at /mnt/data, and the same device bound
    // twice on top of it. The DATA row is listed LAST on purpose -- a
    // tier lookup that matched on the device alone would pick /mica here
    // and report it as the DATA tier's own mountpoint.
    write(
        "proc/self/mountinfo",
        concat!(
            "25 1 254:0 / / ro,noatime shared:1 - squashfs /dev/dm-0 ro\n",
            "32 25 179:6 / /mnt/system ro - ext4 /dev/mmcblk0p6 ro\n",
            "33 25 179:11 /mica /mica rw,noatime - ext4 /dev/mmcblk0p11 rw\n",
            "34 25 179:11 /srv /srv rw,noatime - ext4 /dev/mmcblk0p11 rw\n",
            "35 25 179:11 / /mnt/data rw,noatime - ext4 /dev/mmcblk0p11 rw\n",
        ),
    );
    // The two bind sources mica-data-layout creates, and the probe subtree
    // under the system one.
    std::fs::create_dir_all(path.join("mnt/data/mica")).expect("mkdir");
    std::fs::create_dir_all(path.join("mnt/data/srv")).expect("mkdir");
    std::fs::create_dir_all(path.join("mica/updates/staging")).expect("mkdir");

    let observer = HostStorage::at(path)
        .with_space_reader(|mount| (mount == DATA_MOUNT).then(|| space(1000, 100, 850)));
    let evidence = observer.observe().await.expect("the fixture observes");

    // Only the two labelled tiers exist here; the rest are absent, which
    // status_json renders as `present: false`.
    let mut names: Vec<&str> = evidence.tiers.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(names, ["data", "system"]);

    let data = &evidence.tiers["data"];
    assert_eq!(data.device.as_deref(), Some("/dev/mmcblk0p11"));
    assert_eq!(data.partition_bytes, Some(40_000_000 * 512));
    assert_eq!(
        data.mount.as_ref().map(|mount| mount.mount.as_str()),
        Some(DATA_MOUNT),
        "the DATA tier reported a bind's mountpoint as its own"
    );
    assert_eq!(data.space.map(|space| space.used), Some(100));

    // Both binds observed, both on the DATA device, and the probe ran in
    // the system namespace only.
    let mica = &evidence.binds["mica"];
    assert_eq!(
        mica.mount.as_ref().map(|mount| mount.device.as_str()),
        Some("/dev/mmcblk0p11")
    );
    assert_eq!(mica.source_is_directory, Some(true));
    assert_eq!(mica.probe, Some(ProbeOutcome::Passed));
    // The probe cleans up after itself: a readiness check that leaves
    // files behind is a slow leak on the filesystem it is vouching for.
    let leftovers: Vec<_> = std::fs::read_dir(path.join("mica/updates/staging"))
        .expect("read staging")
        .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
        .collect();
    assert!(leftovers.is_empty(), "probe left {leftovers:?} behind");

    let srv = &evidence.binds["srv"];
    assert_eq!(srv.source_is_directory, Some(true));
    // No probe in the user-owned namespace, and it says why rather than
    // reporting a pass nobody earned.
    match &srv.probe {
        Some(ProbeOutcome::NotAttempted(reason)) => {
            assert!(reason.contains("no probe write"), "{reason}")
        }
        other => panic!("expected /srv to be un-probed, got {other:?}"),
    }

    assert_eq!(
        classify_readiness(mica, evidence.data_tier(), Pressure::Normal),
        Readiness::Ready
    );

    // SYSTEM is the physical filesystem containing immutable images.
    let rootfs = &evidence.tiers["system"];
    assert_eq!(
        rootfs.mount.as_ref().map(|mount| mount.mount.as_str()),
        Some("/mnt/system")
    );
    assert!(rootfs.mount.as_ref().is_some_and(|mount| mount.read_only));
    // No space reader answers for `/`, and none is invented.
    assert_eq!(rootfs.space, None);

    assert_eq!(evidence.media.len(), 1, "loop0 is not a medium");
    let medium = &evidence.media[0];
    assert_eq!(medium.name, "mmcblk0");
    assert_eq!(medium.size_bytes, Some(60_000_000 * 512));
    assert_eq!(medium.model.as_deref(), Some("SDINBDA4"));
    assert_eq!(medium.rotational, Some(false));
    assert_eq!(
        medium.health,
        MediaHealth::Emmc {
            life_time_raw: "0x02 0x01".to_string(),
            pre_eol_raw: Some("0x01".to_string()),
        }
    );
}

/// The fail-closed side of the layout contract, over a fixture where
/// `mica-data-layout` has NOT run: the bind sources are absent, the probe
/// subtree is absent, and every one of those is reported as the reason it
/// is rather than as a quiet pass.
#[tokio::test]
pub(super) async fn an_uninitialized_layout_reports_unavailable_and_an_unattempted_probe() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path();
    std::fs::create_dir_all(path.join("dev/disk/by-partlabel")).expect("mkdir");
    std::fs::create_dir_all(path.join("sys/block/mmcblk0/mmcblk0p11")).expect("mkdir");
    std::fs::write(path.join("sys/block/mmcblk0/size"), "60000000\n").expect("write");
    std::fs::write(path.join("sys/block/mmcblk0/mmcblk0p11/partition"), "11\n").expect("write");
    std::fs::write(path.join("sys/block/mmcblk0/mmcblk0p11/size"), "40000000\n").expect("write");
    std::os::unix::fs::symlink("../../mmcblk0p11", path.join("dev/disk/by-partlabel/data"))
        .expect("symlink");
    // DATA is mounted, but nothing has created the namespaces on it.
    std::fs::create_dir_all(path.join("proc/self")).expect("mkdir");
    std::fs::write(
        path.join("proc/self/mountinfo"),
        "35 25 179:11 / /mnt/data rw,noatime - ext4 /dev/mmcblk0p11 rw\n",
    )
    .expect("write");

    let evidence = HostStorage::at(path)
        .observe()
        .await
        .expect("the fixture observes");

    for name in ["mica", "srv"] {
        let bind = &evidence.binds[name];
        assert_eq!(bind.mount, None, "{name} is not mounted in this fixture");
        assert_eq!(bind.source_is_directory, None, "{name} has no source yet");
        assert_eq!(
            classify_readiness(bind, evidence.data_tier(), Pressure::Normal),
            Readiness::Unavailable,
            "{name} must be unavailable, so no writer falls back elsewhere"
        );
    }
    match &evidence.binds["mica"].probe {
        Some(ProbeOutcome::NotAttempted(reason)) => {
            assert!(reason.contains("/mica"), "{reason}")
        }
        other => panic!("expected an unattempted probe, got {other:?}"),
    }
}

/// A medium with no wear registers in sysfs takes the unsupported path
/// with the reason attached — the negative control for the test above.
#[tokio::test]
pub(super) async fn a_medium_without_sysfs_wear_registers_is_reported_unsupported() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path();
    std::fs::create_dir_all(path.join("sys/block/nvme0n1/device")).expect("mkdir");
    std::fs::write(path.join("sys/block/nvme0n1/size"), "2000000\n").expect("write");
    std::fs::write(path.join("sys/block/nvme0n1/device/model"), "SOME SSD\n").expect("write");

    let evidence = HostStorage::at(path)
        .observe()
        .await
        .expect("the fixture observes");
    assert!(
        evidence.tiers.is_empty(),
        "no partition labels in the fixture"
    );
    assert_eq!(evidence.media.len(), 1);
    match &evidence.media[0].health {
        MediaHealth::Unsupported(reason) => assert!(reason.contains("SMART"), "{reason}"),
        other => panic!("expected unsupported health, got {other:?}"),
    }
}

/// The default observer inspects nothing at all, which is what makes it
/// safe as the daemon's default.
#[tokio::test]
pub(super) async fn the_unavailable_observer_answers_no_evidence() {
    assert!(UnavailableStorageStatus.observe().await.is_err());
}

#[test]
pub(super) fn physical_tiers_follow_the_file_deployment_layout() {
    assert_eq!(
        TIERS.iter().map(|tier| tier.name).collect::<Vec<_>>(),
        ["esp", "firmware", "system", "data"]
    );
    let status = status_json(&StorageEvidence::default(), &PressureTracker::default());
    assert_eq!(status["policy"]["watchedTiers"], json!(["data"]));
}

#[test]
pub(super) fn a_bind_of_another_data_directory_is_unavailable() {
    let mut bind = bound("/dev/vda3");
    bind.mount = parse_mountinfo("33 25 254:3 /state /mica rw - ext4 /dev/vda3 rw\n").pop();
    assert_eq!(
        classify_readiness(&bind, Some(&data_tier("/dev/vda3")), Pressure::Normal),
        Readiness::Unavailable
    );
}

#[test]
pub(super) fn project_reports_use_bytes_and_inodes_without_duplicating_data_capacity() {
    let usage = project_usage(lifecycle_sys::ProjectQuota {
        block_limit_kib: 32768,
        inode_limit: 2048,
        used_bytes: 92 * 1024,
        used_inodes: 23,
    })
    .unwrap();
    assert_eq!(usage.used_bytes, 92 * 1024);
    assert_eq!(usage.limit_bytes, 32 * 1024 * 1024);
    assert_eq!(usage.used_inodes, 23);
    assert_eq!(usage.limit_inodes, 2048);
    let unlimited = project_usage(lifecycle_sys::ProjectQuota::default()).unwrap();
    assert_eq!((unlimited.limit_bytes, unlimited.limit_inodes), (0, 0));
    assert!(
        project_usage(lifecycle_sys::ProjectQuota {
            block_limit_kib: u64::MAX,
            ..Default::default()
        })
        .is_none()
    );
}

#[test]
pub(super) fn a_filesystem_without_project_quotas_reports_none() {
    let dir = tempfile::tempdir().unwrap();
    assert!(project_quotas(dir.path()).is_none());
}

#[test]
pub(super) fn writable_var_usage_belongs_to_the_bounded_variable_project() {
    let mut evidence = StorageEvidence::default();
    evidence.directory_bytes.insert("var".into(), 4096);
    let status = status_json(&evidence, &PressureTracker::default());
    let directories = status["namespaces"]["directories"].as_array().unwrap();
    let var = directories
        .iter()
        .find(|entry| entry["name"] == "var")
        .unwrap();
    assert_eq!(var["usedBytes"], 4096);
    assert_eq!(var["project"], 101);
}
#[test]
pub(super) fn container_storage_is_an_independent_bind_and_project() {
    let status = status_json(&StorageEvidence::default(), &PressureTracker::default());
    let dirs = status["namespaces"]["directories"].as_array().unwrap();
    assert!(
        dirs.iter()
            .any(|entry| entry["name"] == "containers" && entry["project"] == 102)
    );
    assert!(
        BINDS
            .iter()
            .any(|spec| spec.mount == "/mica/containers" && spec.source == "/mnt/data/containers")
    );
}

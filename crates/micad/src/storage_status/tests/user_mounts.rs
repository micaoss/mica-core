//! The user's own mounts and swaps.

use super::*;
use crate::storage_status::user_mounts::{Declared, in_user_area, parse_swaps};

#[test]
fn the_user_areas_are_the_plans() {
    for target in [
        "/srv",
        "/srv/backup",
        "/var/mnt/disk",
        "/home/alice",
        "/home/alice/disk",
        "/mica/containers/storage",
        "/mica/containers/docker",
    ] {
        assert!(in_user_area(target), "{target}");
    }
    for target in [
        "/",
        "/srvx",
        "/srv/",
        "/var/mnt",
        "/home",
        "/home/",
        "/mnt/data",
        "/mica",
        "/mica/containers",
        "/mica/containers/storage/overlay",
        "/var/lib/mica",
        "/run/mica/media/abc",
    ] {
        assert!(!in_user_area(target), "{target}");
    }
}

#[test]
fn units_and_fstab_declare_targets_and_swaps() {
    let mut declared = Declared::default();
    declared.unit(
        "srv-backup.mount",
        "[Mount]\nWhat=/dev/disk/by-uuid/1\nWhere=/srv/backup/\nType=ext4\n",
    );
    declared.unit(
        "var-mnt-media.automount",
        "[Automount]\nWhere=/var/mnt/media\n",
    );
    declared.unit("dev-sdb2.swap", "[Swap]\nWhat=/dev/sdb2\n");
    declared.unit("x.service", "[Service]\nWhere=/nope\n");
    declared.fstab(concat!(
        "# a comment\n",
        "\n",
        "UUID=2 /home/alice/disk ext4 nosuid,nodev 0 2\n",
        "/srv/swap\\040file none swap sw 0 0\n",
    ));
    assert_eq!(
        declared
            .targets
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["/home/alice/disk", "/srv/backup", "/var/mnt/media"]
    );
    assert_eq!(
        declared
            .swaps
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["/dev/sdb2", "/srv/swap file"]
    );

    let swaps = parse_swaps(
        concat!(
            "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority\n",
            "/dev/sdb2                               partition\t1048572\t\t0\t\t-2\n",
            "/var/swapfile                           file\t\t4096\t\t1024\t\t-3\n",
        ),
        &declared,
    );
    assert_eq!(swaps.len(), 2);
    assert_eq!(swaps[0].source, "/dev/sdb2");
    assert_eq!(swaps[0].kind, "partition");
    assert_eq!(swaps[0].size_bytes, 1_048_572 * 1024);
    assert!(swaps[0].user);
    assert_eq!(swaps[1].used_bytes, 1024 * 1024);
    assert!(!swaps[1].user);
}

#[tokio::test]
async fn the_host_lists_mounts_in_user_areas_but_the_default_srv_bind() {
    let root = tempfile::tempdir().expect("tempdir");
    let path = root.path();
    let write = |relative: &str, contents: &str| {
        let target = path.join(relative);
        std::fs::create_dir_all(target.parent().expect("a parent")).expect("mkdir");
        std::fs::write(target, contents).expect("write");
    };
    write("sys/block/mmcblk0/size", "60000000\n");
    write("sys/block/mmcblk0/mmcblk0p11/partition", "11\n");
    write("sys/block/mmcblk0/mmcblk0p11/size", "40000000\n");
    std::fs::create_dir_all(path.join("dev/disk/by-partlabel")).expect("mkdir");
    std::os::unix::fs::symlink("../../mmcblk0p11", path.join("dev/disk/by-partlabel/data"))
        .expect("symlink");
    write(
        "proc/self/mountinfo",
        concat!(
            "35 25 179:11 / /mnt/data rw,noatime - ext4 /dev/mmcblk0p11 rw\n",
            "34 25 179:11 /srv /srv rw,nosuid,nodev - ext4 /dev/mmcblk0p11 rw\n",
            "40 34 8:17 / /srv/backup rw,nosuid,nodev,noatime - ext4 /dev/sdb1 rw\n",
            "41 25 0:50 / /var/mnt/media rw,relatime - autofs systemd-1 rw\n",
            "42 25 8:33 / /mica/containers/storage rw,nosuid,nodev - xfs /dev/sdc1 rw\n",
            "43 25 0:60 / /var/mnt/nas rw - nfs4 nas:/export rw\n",
            "44 25 8:18 / /mnt/other rw - ext4 /dev/sdb3 rw\n",
        ),
    );
    write(
        "usr/local/lib/systemd/system/srv-backup.mount",
        "[Mount]\nWhat=/dev/sdb1\nWhere=/srv/backup\n",
    );
    write(
        "usr/local/lib/systemd/system/dev-sdb2.swap",
        "[Swap]\nWhat=/dev/sdb2\n",
    );
    write(
        "proc/swaps",
        "Filename Type Size Used Priority\n/dev/sdb2 partition 2048 0 -2\n",
    );

    let observer = HostStorage::at(path).with_space_reader(|mount| {
        assert_ne!(mount, "/var/mnt/media", "an automount is never statted");
        assert_ne!(mount, "/var/mnt/nas", "a network mount is never statted");
        Some(space(1000, 100, 850))
    });
    let evidence = observer.observe().await.expect("the fixture observes");

    let listed: Vec<(&str, &str, bool)> = evidence
        .user_mounts
        .iter()
        .map(|mount| (mount.target.as_str(), mount.source.as_str(), mount.user))
        .collect();
    assert_eq!(
        listed,
        [
            ("/srv/backup", "/dev/sdb1", true),
            ("/var/mnt/media", "systemd-1", false),
            ("/mica/containers/storage", "/dev/sdc1", false),
            ("/var/mnt/nas", "nas:/export", false),
        ]
    );
    assert_eq!(evidence.user_mounts[0].options, "rw,nosuid,nodev,noatime");
    assert!(evidence.user_mounts[0].space.is_some());
    assert!(evidence.user_mounts[1].space.is_none());
    assert_eq!(evidence.swaps.len(), 1);
    assert!(evidence.swaps[0].user);

    let status = status_json(&evidence, &PressureTracker::default());
    assert_eq!(status["userMounts"][0]["target"], "/srv/backup");
    assert_eq!(status["userMounts"][0]["user"], true);
    assert_eq!(status["userMounts"][0]["usedBytes"], 100);
    assert!(status["userMounts"][1].get("usedBytes").is_none());
    assert_eq!(status["swaps"][0]["sizeBytes"], 2048 * 1024);
}

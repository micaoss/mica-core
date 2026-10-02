//! Media wear, fsck units, mountinfo and df.

use super::*;

/// The JEDEC lifetime register is a 10% bucket, and it is reported as one.
/// `0x00` is "not defined" and must not become a bucket.
#[test]
pub(super) fn the_emmc_lifetime_register_decodes_to_the_bucket_it_is() {
    assert_eq!(parse_life_time("0x01"), Some((0, 10)));
    assert_eq!(parse_life_time("0x02"), Some((10, 20)));
    assert_eq!(parse_life_time("0x0A"), Some((90, 100)));
    assert_eq!(parse_life_time("0x0B"), Some((100, 100)));
    assert_eq!(parse_life_time("0x00"), None);
    assert_eq!(parse_life_time("0x0F"), None);
    assert_eq!(parse_life_time("nonsense"), None);

    assert_eq!(parse_pre_eol("0x01"), "normal");
    assert_eq!(parse_pre_eol("0x02"), "warning");
    assert_eq!(parse_pre_eol("0x03"), "urgent");
    // 0x00 is the device declining to answer, which is NOT "normal".
    assert_eq!(parse_pre_eol("0x00"), "undefined");
    assert_eq!(parse_pre_eol(""), "undefined");
}

/// A device with no wear registers reports unsupported WITH the reason,
/// and the reason names the build decision rather than the device.
#[test]
pub(super) fn a_medium_without_wear_registers_reports_unsupported_and_why() {
    let health = health_json(&MediaHealth::Unsupported(
        unsupported_reason("nvme0n1").to_string(),
    ));
    assert_eq!(health["supported"], false);
    assert!(
        health["reason"].as_str().unwrap().contains("smartctl"),
        "{health}"
    );
    assert!(health.get("lifetimeEstimates").is_none(), "{health}");

    let emmc = health_json(&MediaHealth::Emmc {
        life_time_raw: "0x03 0x02".to_string(),
        pre_eol_raw: Some("0x02".to_string()),
    });
    assert_eq!(emmc["supported"], true);
    assert_eq!(emmc["preEol"], "warning");
    assert_eq!(emmc["lifetimeEstimates"][0]["usedPercentMin"], 20);
    assert_eq!(emmc["lifetimeEstimates"][0]["usedPercentMax"], 30);
    assert_eq!(emmc["lifetimeEstimates"][1]["usedPercentMax"], 20);
    // The raw registers survive normalization for support to read.
    assert_eq!(emmc["raw"]["lifeTime"], "0x03 0x02");
    assert_eq!(emmc["raw"]["preEolInfo"], "0x02");
}

/// A unit name is a path, and a partuuid path is full of literal hyphens:
/// a plain `-` to `/` replacement would name a device that does not exist.
#[test]
pub(super) fn an_fsck_unit_name_decodes_to_the_device_it_checked() {
    assert_eq!(
        fsck_unit_device(r"systemd-fsck@dev-disk-by\x2dpartuuid-5ac35760\x2d0002\x2d4000.service")
            .as_deref(),
        Some("/dev/disk/by-partuuid/5ac35760-0002-4000")
    );
    assert_eq!(
        fsck_unit_device("systemd-fsck@dev-mmcblk0p9.service").as_deref(),
        Some("/dev/mmcblk0p9")
    );
    assert_eq!(fsck_unit_device("systemd-fsck-root.service"), None);
    assert_eq!(fsck_unit_device("micad.service"), None);
    assert_eq!(fsck_unit_device("systemd-fsck@.service"), None);
}

/// The pairing from unit to tier device, through the symlink `/etc/fstab`
/// actually names the tiers by.
#[test]
pub(super) fn recorded_checks_pair_with_the_devices_they_checked() {
    let units = vec![
        CheckEvidence {
            unit: r"systemd-fsck@dev-disk-by\x2dpartuuid-state\x2duuid.service".to_string(),
            active_state: Some("inactive".to_string()),
            result: Some("success".to_string()),
            exit_status: Some(0),
        },
        CheckEvidence {
            unit: "not-an-fsck-unit.service".to_string(),
            active_state: None,
            result: None,
            exit_status: None,
        },
    ];
    let paired = checks_by_device(&units, |device| {
        (device == "/dev/disk/by-partuuid/state-uuid").then(|| "/dev/mmcblk0p9".to_string())
    });
    assert_eq!(paired.len(), 1);
    assert_eq!(paired["/dev/mmcblk0p9"].exit_status, Some(0));

    // A unit whose symlink no longer resolves keeps the name it had,
    // rather than being dropped: unmatched evidence is still evidence.
    let unresolved = checks_by_device(&units, |_| None);
    assert!(unresolved.contains_key("/dev/disk/by-partuuid/state-uuid"));
}

#[test]
pub(super) fn mountinfo_yields_the_device_mount_and_read_only_flag() {
    let text = concat!(
        "25 1 254:0 / / ro,noatime shared:1 - squashfs /dev/dm-0 ro\n",
        "31 25 179:9 / /mnt/state rw,noatime shared:2 - ext4 /dev/mmcblk0p9 rw\n",
        r"33 25 179:11 / /srv\040data rw,noatime - ext4 /dev/mmcblk0p11 rw",
        "\n",
    );
    let mounts = parse_mountinfo(text);
    assert_eq!(mounts.len(), 3);
    assert_eq!(mounts[0].device, "/dev/dm-0");
    assert!(mounts[0].read_only);
    assert_eq!(mounts[0].fstype, "squashfs");
    assert_eq!(mounts[1].mount, "/mnt/state");
    assert!(!mounts[1].read_only);
    // The optional-tag run before the `-` varies in length, so the fields
    // after it cannot be indexed from the left; line 3 has none at all.
    assert_eq!(mounts[2].device, "/dev/mmcblk0p11");
    // Octal escapes are the kernel's, and a path is not a mount point
    // until they are decoded.
    assert_eq!(mounts[2].mount, "/srv data");
}

#[test]
pub(super) fn df_output_yields_the_reserved_pool_as_its_own_number() {
    let output = concat!(
        "Filesystem     1B-blocks       Used  Available Capacity Mounted on\n",
        "/dev/mmcblk0p11 1000000000 700000000  250000000      74% /srv\n",
    );
    let space = parse_df(output).expect("the POSIX layout parses");
    assert_eq!(space.total, 1_000_000_000);
    assert_eq!(space.used, 700_000_000);
    assert_eq!(space.free, 250_000_000);
    // Neither free nor used: the pool only root can write into.
    assert_eq!(space.reserved, 50_000_000);
    // 70%, not 73%: the reserved pool is space the tier does not have.
    assert_eq!(space.used_percent(), 70);

    assert_eq!(parse_df("Filesystem\n"), None);
    assert_eq!(parse_df(""), None);
}

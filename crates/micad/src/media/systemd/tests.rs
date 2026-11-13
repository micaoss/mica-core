use std::io;
use std::path::Path;

use zbus::Variant;

use super::{escape_path, job_outcome, properties, unit_name};

#[test]
fn unit_names_escape_the_place_as_systemd_does() {
    assert_eq!(
        unit_name(Path::new("/run/mica/media/0a1b2c3d4e5f")),
        "run-mica-media-0a1b2c3d4e5f.mount"
    );
    assert_eq!(escape_path(Path::new("/dev/sda1")), "dev-sda1");
    assert_eq!(escape_path(Path::new("/dev/mmcblk1p1")), "dev-mmcblk1p1");
    assert_eq!(escape_path(Path::new("/a-b/.c d")), "a\\x2db-.c\\x20d");
    assert_eq!(escape_path(Path::new("/.x")), "\\x2ex");
}

#[test]
fn the_unit_mounts_read_only_bound_to_its_device() {
    let unit = properties(
        Path::new("/dev/sda1"),
        Path::new("/run/mica/media/abc"),
        "vfat",
    )
    .unwrap_or_default();
    let get = |name: &str| {
        unit.iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(get("What"), Some(Variant::from("/dev/sda1")));
    assert_eq!(get("Where"), Some(Variant::from("/run/mica/media/abc")));
    assert_eq!(get("Type"), Some(Variant::from("vfat")));
    assert_eq!(
        get("Options"),
        Some(Variant::from(
            "ro,nosuid,nodev,noexec,utf8,uid=0,gid=0,fmask=0133,dmask=0022"
        ))
    );
    let device = Variant::from(vec!["dev-sda1.device".to_owned()]);
    assert_eq!(get("BindsTo"), Some(device.clone()));
    assert_eq!(get("After"), Some(device));
    assert_eq!(
        get("CollectMode"),
        Some(Variant::from("inactive-or-failed"))
    );

    let ext = properties(
        Path::new("/dev/sdb"),
        Path::new("/run/mica/media/x"),
        "ext3",
    )
    .unwrap_or_default();
    assert!(ext.contains(&("Type", Variant::from("ext4"))));
    assert!(ext.contains(&("Options", Variant::from("ro,nosuid,nodev,noexec,noload"))));
}

#[test]
fn an_unsupported_filesystem_queues_nothing() {
    let refused = properties(
        Path::new("/dev/sda1"),
        Path::new("/run/mica/media/a"),
        "ntfs",
    );
    assert_eq!(
        refused.err().map(|error| error.kind()),
        Some(io::ErrorKind::Unsupported)
    );
}

#[test]
fn job_results_map_to_the_mount_errors() {
    assert!(job_outcome("done", false).is_ok());
    assert!(job_outcome("done", true).is_ok());
    let busy = job_outcome("failed", true).err();
    assert_eq!(
        busy.and_then(|error| error.raw_os_error()),
        Some(rustix::io::Errno::BUSY.raw_os_error())
    );
    let failed = job_outcome("failed", false).err();
    assert!(failed.is_some_and(|error| error.raw_os_error().is_none()));
}

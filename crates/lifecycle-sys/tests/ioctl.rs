// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use lifecycle_sys::{
    clear_loop, loop_status, watchdog_keepalive, watchdog_support, watchdog_timeout,
};

#[test]
fn unrelated_regular_descriptor_returns_errno_without_writes_or_close() {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(b"must remain intact").unwrap();
    assert_eq!(
        watchdog_support(&file).unwrap_err(),
        rustix::io::Errno::NOTTY
    );
    assert_eq!(
        watchdog_timeout(&file).unwrap_err(),
        rustix::io::Errno::NOTTY
    );
    assert_eq!(
        watchdog_keepalive(&file).unwrap_err(),
        rustix::io::Errno::NOTTY
    );
    assert_eq!(loop_status(&file).unwrap_err(), rustix::io::Errno::NOTTY);
    assert_eq!(clear_loop(&file).unwrap_err(), rustix::io::Errno::NOTTY);
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut data = String::new();
    file.read_to_string(&mut data).unwrap();
    assert_eq!(data, "must remain intact");
}

#[test]
fn device_mapper_requests_refuse_unrelated_descriptors() {
    let file = tempfile::tempfile().unwrap();
    assert_eq!(
        lifecycle_sys::dm_status(&file, 0xfd00).unwrap_err(),
        rustix::io::Errno::NOTTY
    );
    let status = lifecycle_sys::DmStatus {
        device: 0xfd00,
        name: "mica-root".into(),
        uuid: "CRYPT-VERITY-owned".into(),
        targets: 1,
        open_count: 0,
        event: 0,
    };
    assert_eq!(
        lifecycle_sys::dm_table(&file, &status).unwrap_err(),
        rustix::io::Errno::NOTTY
    );
    assert_eq!(
        lifecycle_sys::dm_remove(&file, &status).unwrap_err(),
        rustix::io::Errno::NOTTY
    );
    assert_eq!(file.metadata().unwrap().len(), 0);
}

#[test]
fn project_quota_requests_refuse_a_filesystem_without_quotas() {
    let dir = tempfile::tempdir().unwrap();
    let fs = std::fs::File::open(dir.path()).unwrap();
    assert!(lifecycle_sys::project_quota(&fs, 101).is_err());
    assert!(lifecycle_sys::set_project_limits(&fs, 101, 32768, 2048).is_err());
}

#[test]
fn the_clock_synchronization_bit_reads_without_privileges() {
    // Either answer is the host's; what is proven is that the read succeeds
    // without changing anything.
    lifecycle_sys::clock_synchronized().unwrap();
}

#[test]
fn ubi_and_mtd_requests_refuse_unrelated_descriptors() {
    use lifecycle_sys::*;
    use rustix::io::Errno;
    let file = tempfile::tempfile().unwrap();
    assert_eq!(
        ubi_mkvol(&file, "p.r-0", 4096, UbiVolumeKind::Static).unwrap_err(),
        Errno::NOTTY
    );
    assert_eq!(ubi_rmvol(&file, 3).unwrap_err(), Errno::NOTTY);
    assert_eq!(
        ubi_rename(&file, &[(3, "r-0"), (4, "k-0")]).unwrap_err(),
        Errno::NOTTY
    );
    assert_eq!(ubi_volume_update(&file, 4096).unwrap_err(), Errno::NOTTY);
    assert_eq!(ubi_leb_change(&file, 0, 65536).unwrap_err(), Errno::NOTTY);
    assert_eq!(ubi_block_create(&file).unwrap_err(), Errno::NOTTY);
    assert_eq!(ubi_block_remove(&file).unwrap_err(), Errno::NOTTY);
    assert_eq!(mtd_bad_block(&file, 0).unwrap_err(), Errno::NOTTY);
    assert_eq!(file.metadata().unwrap().len(), 0);
}

#[test]
fn ubi_requests_refuse_what_the_kernel_could_not_take_before_any_call() {
    use lifecycle_sys::*;
    use rustix::io::Errno;
    let file = tempfile::tempfile().unwrap();
    let long = "v".repeat(UBI_MAX_VOLUME_NAME + 1);
    for name in ["", "a\0b", long.as_str()] {
        assert_eq!(
            ubi_mkvol(&file, name, 4096, UbiVolumeKind::Static).unwrap_err(),
            Errno::INVAL
        );
        assert_eq!(ubi_rename(&file, &[(1, name)]).unwrap_err(), Errno::INVAL);
    }
    assert_eq!(
        ubi_mkvol(&file, "v", 0, UbiVolumeKind::Dynamic).unwrap_err(),
        Errno::INVAL
    );
    assert_eq!(ubi_rename(&file, &[]).unwrap_err(), Errno::INVAL);
    let many: Vec<(u32, &str)> = (0..=UBI_MAX_RENAME as u32).map(|v| (v, "v")).collect();
    assert_eq!(ubi_rename(&file, &many).unwrap_err(), Errno::INVAL);
    assert_eq!(ubi_rmvol(&file, u32::MAX).unwrap_err(), Errno::INVAL);
    assert_eq!(mtd_bad_block(&file, u64::MAX).unwrap_err(), Errno::INVAL);
}

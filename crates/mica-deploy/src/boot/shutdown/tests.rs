use super::*;
use std::collections::BTreeSet;
use std::{
    fs,
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn typed_verity_table_preserves_live_device_provider_and_complete_coverage() {
    let device = Device {
        major: 253,
        minor: 0,
    };
    let provider = Device { major: 7, minor: 0 };
    let status = lifecycle_sys::DmStatus {
        device: rustix::fs::makedev(253, 0),
        name: "mica-root".into(),
        uuid: "CRYPT-VERITY-owned".into(),
        targets: 1,
        open_count: 0,
        event: 7,
    };
    let target = lifecycle_sys::DmTarget {
        sector: 0,
        length: 80,
        kind: "verity".into(),
        parameters: "1 7:0 7:0 4096 4096 10 10 sha256 abcd - 1 restart_on_corruption".into(),
    };
    let slaves = BTreeSet::from([provider]);
    assert_eq!(
        checked_verity_table(
            &status,
            std::slice::from_ref(&target),
            device,
            "mica-root",
            "CRYPT-VERITY-owned",
            &slaves,
            80
        )
        .unwrap(),
        format!("0 80 verity {}", target.parameters)
    );
    for case in 0..11 {
        let mut status = status.clone();
        let mut target = target.clone();
        let mut slaves = slaves.clone();
        match case {
            0 => status.device = rustix::fs::makedev(253, 1),
            1 => status.name = "mica-support".into(),
            2 => status.uuid = "reused".into(),
            3 => status.targets = 2,
            4 => target.sector = 1,
            5 => target.length = 79,
            6 => target.kind = "linear".into(),
            7 => target.parameters = "1 7:0".into(),
            8 => target.parameters = target.parameters.replacen("1 ", "0 ", 1),
            9 => target.parameters = target.parameters.replace("7:0", "7:1"),
            _ => {
                slaves.insert(Device { major: 7, minor: 1 });
            }
        }
        assert!(
            checked_verity_table(
                &status,
                &[target],
                device,
                "mica-root",
                "CRYPT-VERITY-owned",
                &slaves,
                80
            )
            .is_err(),
            "case={case}"
        );
    }
}

#[test]
fn diagnostic_output_never_waits_for_a_full_pipe() {
    use std::io::Write;
    let (mut writer, _reader) = std::os::unix::net::UnixStream::pair().unwrap();
    writer.set_nonblocking(true).unwrap();
    while writer.write(&[0_u8; 4096]).is_ok() {}
    let started = Instant::now();
    assert!(write_diagnostic(&writer, "bounded failure").is_err());
    assert!(started.elapsed() < Duration::from_millis(100));
}

#[test]
fn partition_sysfs_has_holders_but_no_slaves_directory() {
    let root = tempfile::tempdir().unwrap();
    assert!(slave_links(root.path()).is_err());
    fs::write(root.path().join("partition"), "2\n").unwrap();
    assert!(slave_links(root.path()).unwrap().is_empty());
    fs::write(root.path().join("partition"), "0\n").unwrap();
    assert!(slave_links(root.path()).is_err());
}

#[test]
fn watchdog_errors_and_short_metadata_never_create_a_fresh_budget() {
    let mut supervisor = Supervisor::new();
    assert!(supervisor.begin_shutdown().is_err());
    supervisor.watchdog = Some(tempfile::tempfile().unwrap());
    supervisor.armed = true;
    assert!(supervisor.begin_shutdown().is_err());
    supervisor.last_kick = 0;
    supervisor.started = Instant::now() - Duration::from_secs(2);
    assert!(supervisor.kick().is_err());
    supervisor.budget = Some(Budget::new(0, 60).unwrap());
    assert!(supervisor.limit_shutdown(Some(2999)).is_err());
    let first = supervisor.limit_shutdown(Some(5000)).unwrap();
    let second = supervisor.limit_shutdown(Some(60000)).unwrap();
    assert_eq!(first.deadline_ms, second.deadline_ms);
    assert_eq!(first.cleanup_deadline_ms, second.cleanup_deadline_ms);
    supervisor.poisoned = true;
    assert!(
        supervisor
            .terminal(Action::Reboot, Released { _private: () })
            .is_err()
    );
}

#[test]
fn supervisor_drains_large_output_without_blocking_wait() {
    let mut supervisor = Supervisor::new();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "head -c 200000 /dev/zero"]);
    assert_eq!(supervisor.run(command, 5000, 200000).unwrap().len(), 200000);
}

#[test]
fn exited_parent_with_live_output_writer_is_not_completion() {
    let mut supervisor = Supervisor::new();
    let mut command = Command::new("/bin/sh");
    command.args(["-c", "sleep 10 & exit 0"]);
    assert!(supervisor.run(command, 1500, 16384).is_err());
    assert!(supervisor.now_ms() < 2000);
}

#[test]
fn supervisor_bounds_output_refusal_and_uncooperative_children() {
    for script in [
        "head -c 200000 /dev/zero",
        "trap '' TERM; while :; do :; done",
    ] {
        let mut supervisor = Supervisor::new();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        assert!(supervisor.run(command, 1500, 16384).is_err());
        assert!(supervisor.now_ms() < 2000);
    }
}

/// The dirty count is the three writeback-pending lines of `meminfo`, and a
/// file missing one of them is refused rather than read as clean.
#[test]
fn the_dirty_count_is_what_meminfo_holds_pending_writeback() {
    let meminfo = "MemTotal: 1000 kB\nDirty: 4 kB\nWriteback: 8 kB\nNFS_Unstable: 0 kB\n";
    assert_eq!(dirty_kib(meminfo).unwrap(), 12);
    assert!(dirty_kib("MemTotal: 1000 kB\nDirty: 4 kB\nWriteback: 0 kB\n").is_err());
    assert!(dirty_kib("Dirty: x kB\nWriteback: 0 kB\nNFS_Unstable: 0 kB\n").is_err());
}

/// The per-CPU counters are folded before the count is read: a CPU still
/// holding a page's accounting is not a page left to write, and a sync never
/// changes it. The write goes to the kernel's own file and never creates one.
#[test]
fn the_vm_counters_are_folded_before_the_dirty_count_is_read() {
    let proc = tempfile::tempdir().unwrap();
    refresh_vm_stats(proc.path());
    assert!(!proc.path().join("sys/vm/stat_refresh").exists());
    fs::create_dir_all(proc.path().join("sys/vm")).unwrap();
    fs::write(proc.path().join("sys/vm/stat_refresh"), "").unwrap();
    refresh_vm_stats(proc.path());
    assert_eq!(
        fs::read_to_string(proc.path().join("sys/vm/stat_refresh")).unwrap(),
        "1\n"
    );
}

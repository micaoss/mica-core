//! The `board` section of the signed boot policy: what a device reads instead
//! of its board's name, and every way a policy can be refused.

#[path = "support/boards.rs"]
mod boards;

use mica_deploy::board::BoardFacts;
use serde_json::{Value, json};

/// A board's section as the cases carry it, to mutate.
fn section(board: &str) -> Value {
    serde_json::to_value(boards::policy(board)).unwrap()
}

/// What a device does with a section: its shape, then its rules.
fn read(section: &Value, arch: &str) -> anyhow::Result<BoardFacts> {
    let facts: BoardFacts = serde_json::from_value(section.clone())?;
    facts.validate(arch)?;
    Ok(facts)
}

#[test]
fn every_board_in_the_cases_reads_and_validates() {
    for board in boards::names() {
        read(&section(&board), &boards::arch(&board)).unwrap_or_else(|e| panic!("{board}: {e:#}"));
    }
}

/// The plan's refusals, each by name: an unknown backend or kernel format,
/// the backend disagreeing with the kernel or the firmware, records on the
/// wrong backend, impossible partition numbers, and firmware that leaves its
/// partition or reaches the records.
#[test]
fn an_inconsistent_board_section_is_refused_by_name() {
    let refusals: Vec<(&str, &str, Value, &str)> = vec![
        ("cx3576", "/boot", json!("grub"), "unknown variant"),
        ("cx3576", "/kernel", json!("zimage"), "unknown variant"),
        (
            "cx3576",
            "/kernel",
            json!("uki"),
            "boot backend and kernel format disagree",
        ),
        (
            "uefi-x64",
            "/kernel",
            json!("fit"),
            "boot backend and kernel format disagree",
        ),
        (
            "uefi-x64",
            "/firmware",
            json!({"format": "disk-range", "diskOffset": 32768, "maxBytes": 65536}),
            "firmware format disagrees with the boot backend",
        ),
        (
            "cx3576",
            "/firmware",
            json!({"format": "efi", "partition": 1, "path": "EFI/BOOT/BOOTAA64.EFI"}),
            "firmware format disagrees with the boot backend",
        ),
        (
            "uefi-x64",
            "/records",
            json!({"startSector": 64, "sectors": 36800, "offsets": [16744448, 17793024], "size": 65536}),
            "a UEFI board carries no FIT boot records",
        ),
        (
            "cx3576",
            "/partitions/data",
            json!(2),
            "invalid partition numbers",
        ),
        (
            "cx3576",
            "/partitions/boot",
            json!(0),
            "invalid partition numbers",
        ),
        (
            "cx3576",
            "/partitions/system",
            json!(129),
            "invalid partition numbers",
        ),
        (
            "uefi-x64",
            "/firmware",
            json!({"format": "efi", "partition": 2, "path": "EFI/BOOT/BOOTX64.EFI"}),
            "invalid EFI destination",
        ),
        (
            "uefi-x64",
            "/firmware",
            json!({"format": "efi", "partition": 1, "path": "EFI/../BOOTX64.EFI"}),
            "invalid EFI destination",
        ),
        (
            "uefi-x64",
            "/firmware",
            json!({"format": "efi", "partition": 1, "path": "boot/BOOTX64.EFI"}),
            "invalid EFI destination",
        ),
        // One byte into the first record copy.
        (
            "cx3576",
            "/firmware",
            json!({"format": "disk-range", "diskOffset": 32768, "maxBytes": 16744449}),
            "invalid loader write range",
        ),
        // Inside the primary GPT.
        (
            "cx3576",
            "/firmware",
            json!({"format": "disk-range", "diskOffset": 512, "maxBytes": 65536}),
            "invalid loader write range",
        ),
        // Past the end of the boot partition.
        (
            "cx3576",
            "/firmware",
            json!({"format": "disk-range", "diskOffset": 18874368, "maxBytes": 65536}),
            "invalid loader write range",
        ),
        (
            "s905x5m",
            "/firmware",
            json!({"format": "emmc-boot", "area": "boot0", "payloadOffset": 512, "maxBytes": 0}),
            "invalid eMMC boot payload",
        ),
        (
            "s905x5m",
            "/firmware",
            json!({"format": "emmc-boot", "area": "boot2", "payloadOffset": 512, "maxBytes": 4193792}),
            "unknown variant",
        ),
        // A format name this device does not implement is refused, with no alias.
        (
            "cx3576",
            "/firmware",
            json!({"format": "rockchip-loader", "diskOffset": 32768, "maxBytes": 16744448}),
            "unknown variant",
        ),
        (
            "cx3576",
            "/records/size",
            json!(4096),
            "boot record size is not 65536",
        ),
    ];
    for (board, pointer, value, want) in refusals {
        let mut s = section(board);
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        let target = if parent.is_empty() {
            &mut s
        } else {
            s.pointer_mut(parent).unwrap()
        };
        target
            .as_object_mut()
            .unwrap()
            .insert(field.to_owned(), value);
        let error = format!("{:#}", read(&s, &boards::arch(board)).expect_err(pointer));
        assert!(
            error.contains(want),
            "{board} {pointer}: expected {want:?}, got {error:?}"
        );
    }
}

#[test]
fn fit_records_are_required_on_a_fit_board_and_unknown_keys_are_refused() {
    let mut s = section("cx3576");
    s.as_object_mut().unwrap().remove("records");
    let error = format!("{:#}", read(&s, "arm64").expect_err("no records"));
    assert!(error.contains("FIT boot records absent"), "{error}");

    let mut s = section("uefi-x64");
    s["vendor"] = json!("x");
    assert!(read(&s, "amd64").is_err(), "an unknown key is refused");

    let mut s = section("uefi-x64");
    s.as_object_mut().unwrap().remove("partitions");
    assert!(read(&s, "amd64").is_err(), "partitions are required");
}

#[test]
fn an_architecture_the_device_does_not_run_is_refused() {
    let error = format!(
        "{:#}",
        read(&section("uefi-x64"), "riscv64").expect_err("riscv64")
    );
    assert!(error.contains("unsupported architecture"), "{error}");
}

/// A sysfs disk with the given partitions: `(name, number, start, size)`.
fn disk(parts: &[(&str, u32, u64, u64)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, number, start, size) in parts {
        let node = dir.path().join(name);
        std::fs::create_dir(&node).unwrap();
        std::fs::write(node.join("partition"), format!("{number}\n")).unwrap();
        std::fs::write(node.join("start"), format!("{start}\n")).unwrap();
        std::fs::write(node.join("size"), format!("{size}\n")).unwrap();
    }
    dir
}

/// The fail-closed guard: the boot partition is found by the number the
/// policy names and held to the geometry it names, before anything is
/// written. A later kernel whose policy disagrees with the disk refuses.
#[test]
fn the_boot_partition_is_held_to_the_signed_geometry() {
    use mica_deploy::deployments::boot_partition;
    let policy = boards::policy("cx3576");
    let layout = policy.records.unwrap();
    let error = |parts: &[(&str, u32, u64, u64)], system: &str| {
        let dir = disk(parts);
        format!(
            "{:#}",
            boot_partition(&dir.path().join(system), &policy).expect_err(system)
        )
    };
    let firmware = ("zz-firmware", 1, layout.start_sector(), layout.sectors());
    let system = ("zz-system", 2, 36864, 4_194_304);
    let data = ("zz-data", 3, 4_231_168, 1_000_000);

    // The geometry matches: what stops this tree is only that its device
    // node is not a real block device.
    let e = error(&[firmware, system, data], "zz-system");
    assert!(
        !e.contains("geometry") && !e.contains("partition is absent"),
        "{e}"
    );

    let e = error(
        &[("zz-firmware", 1, 2048, layout.sectors()), system, data],
        "zz-system",
    );
    assert!(
        e.contains("FIRMWARE geometry differs from the signed boot policy"),
        "{e}"
    );
    let e = error(
        &[
            ("zz-firmware", 1, layout.start_sector(), 1024),
            system,
            data,
        ],
        "zz-system",
    );
    assert!(
        e.contains("FIRMWARE geometry differs from the signed boot policy"),
        "{e}"
    );

    // SYSTEM is not the partition the policy numbers.
    let e = error(&[firmware, ("zz-system", 3, 36864, 4_194_304)], "zz-system");
    assert!(e.contains("invalid SYSTEM partition"), "{e}");

    // No partition carries the boot number.
    let e = error(
        &[
            ("zz-firmware", 4, layout.start_sector(), layout.sectors()),
            system,
            data,
        ],
        "zz-system",
    );
    assert!(e.contains("boot partition is absent or ambiguous"), "{e}");
}

/// A board whose disk carries a partition of its own ahead of SYSTEM: the
/// numbers are the policy's, so nothing in the device assumes 1, 2 and 3.
#[test]
fn partition_numbers_come_from_the_policy() {
    use mica_deploy::deployments::boot_partition;
    let mut policy = boards::policy("cx3576");
    policy.partitions = mica_deploy::board::Partitions {
        boot: 1,
        system: 3,
        data: 4,
    };
    policy.validate("arm64").unwrap();
    let layout = policy.records.unwrap();
    let dir = disk(&[
        ("zz-firmware", 1, layout.start_sector(), layout.sectors()),
        ("zz-vendor", 2, 36864, 2048),
        ("zz-system", 3, 38912, 4_194_304),
        ("zz-data", 4, 4_233_216, 1_000_000),
    ]);
    let e = format!(
        "{:#}",
        boot_partition(&dir.path().join("zz-system"), &policy).expect_err("no device node")
    );
    assert!(
        !e.contains("SYSTEM") && !e.contains("geometry") && !e.contains("absent"),
        "{e}"
    );
}

/// The watchdog is the board's declaration: absent, `watchdog0` as always;
/// named, the one watchdog whose driver reports that identity, and a name that
/// matches none or several is refused rather than guessed.
#[test]
fn the_watchdog_is_the_one_the_policy_names() {
    use mica_deploy::boot::shutdown::resolve_watchdog;
    let class = tempfile::tempdir().unwrap();
    for (name, identity) in [
        ("watchdog0", "Software Watchdog"),
        ("watchdog1", "Synopsys DesignWare Watchdog"),
        ("watchdog2", "iTCO_wdt"),
        ("watchdog3", "iTCO_wdt"),
    ] {
        std::fs::create_dir(class.path().join(name)).unwrap();
        std::fs::write(
            class.path().join(name).join("identity"),
            format!("{identity}\n"),
        )
        .unwrap();
    }
    assert_eq!(resolve_watchdog(class.path(), None).unwrap(), "watchdog0");
    assert_eq!(
        resolve_watchdog(class.path(), Some("Synopsys DesignWare Watchdog")).unwrap(),
        "watchdog1"
    );
    for refused in ["iTCO_wdt", "absent", "Synopsys"] {
        let error = resolve_watchdog(class.path(), Some(refused))
            .expect_err(refused)
            .to_string();
        assert!(error.contains("absent or ambiguous"), "{refused}: {error}");
    }

    let mut s = section("cx3576");
    s["watchdog"] = json!({"identity": "Synopsys DesignWare Watchdog"});
    let facts = read(&s, "arm64").unwrap();
    assert_eq!(
        facts.watchdog.map(|w| w.identity).as_deref(),
        Some("Synopsys DesignWare Watchdog")
    );
    for bad in [
        json!({"identity": ""}),
        json!({"identity": "x".repeat(33)}),
        json!({"identity": "a\nb"}),
    ] {
        let mut s = section("cx3576");
        s["watchdog"] = bad;
        let error = format!("{:#}", read(&s, "arm64").expect_err("a bad identity"));
        assert!(error.contains("invalid watchdog identity"), "{error}");
    }
}

/// A loader a boot ROM reads from before the first partition is declared
/// like any other range: the device needs no code for where a ROM looks.
#[test]
fn a_loader_before_the_boot_partition_is_a_declaration() {
    let mut s = section("cx3576");
    s["records"] = json!({"startSector": 2048, "sectors": 36800, "offsets": [16744448, 17793024], "size": 65536});
    s["firmware"] =
        json!({"format": "disk-range", "diskOffset": 32768, "maxBytes": 1048576 - 32768});
    read(&s, "arm64").unwrap();
}

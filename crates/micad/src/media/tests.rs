use std::fs;
use std::os::unix::fs::symlink;

use super::*;

const BOOT: &str = "3f0c7d1e-8f44-4c2a-9a55-0d6c1f2e7b10";

/// An ext4 superblock with `label`.
fn ext4(label: &str) -> Vec<u8> {
    let mut b = vec![0u8; 8192];
    b[1024 + 56..1024 + 58].copy_from_slice(&0xEF53u16.to_le_bytes());
    b[1024 + 58..1024 + 60].copy_from_slice(&1u16.to_le_bytes());
    b[1024 + 92..1024 + 96].copy_from_slice(&0x4u32.to_le_bytes());
    b[1024 + 96..1024 + 100].copy_from_slice(&0x2c2u32.to_le_bytes());
    b[1024 + 120..1024 + 120 + label.len()].copy_from_slice(label.as_bytes());
    b
}

/// A FAT32 boot sector with an 11-byte `label`.
fn fat32(label: &[u8; 11]) -> Vec<u8> {
    let mut b = vec![0u8; 4096];
    b[3..11].copy_from_slice(b"mkfs.fat");
    b[11..13].copy_from_slice(&512u16.to_le_bytes());
    b[71..82].copy_from_slice(label);
    b[82..90].copy_from_slice(b"FAT32   ");
    b[510] = 0x55;
    b[511] = 0xAA;
    b
}

/// An exFAT volume whose root directory (cluster 4, 512-byte sectors, 8
/// sectors a cluster, heap at sector 64) carries `label`.
fn exfat(label: &str) -> Vec<u8> {
    let mut b = vec![0u8; 128 * 1024];
    b[3..11].copy_from_slice(b"EXFAT   ");
    b[88..92].copy_from_slice(&64u32.to_le_bytes());
    b[96..100].copy_from_slice(&4u32.to_le_bytes());
    b[108] = 9;
    b[109] = 3;
    let root = (64 + (2 << 3)) << 9;
    b[root] = 0x81; // an allocation bitmap entry first
    let entry = root + 32;
    let units: Vec<u16> = label.encode_utf16().collect();
    b[entry] = 0x83;
    b[entry + 1] = u8::try_from(units.len()).unwrap();
    for (i, unit) in units.iter().enumerate() {
        b[entry + 2 + 2 * i..entry + 4 + 2 * i].copy_from_slice(&unit.to_le_bytes());
    }
    b
}

fn ntfs() -> Vec<u8> {
    let mut b = vec![0u8; 4096];
    b[3..11].copy_from_slice(b"NTFS    ");
    b[510] = 0x55;
    b[511] = 0xAA;
    b
}

fn probe_bytes(bytes: &[u8]) -> std::io::Result<(&'static str, Option<String>)> {
    probed(bytes).map(|p| (p.filesystem, p.label))
}

fn probed(bytes: &[u8]) -> std::io::Result<Probed> {
    probe(|offset, buf| {
        let start = usize::try_from(offset).unwrap();
        let end = start + buf.len();
        if end > bytes.len() {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.copy_from_slice(&bytes[start..end]);
        Ok(())
    })
}

#[test]
fn a_filesystem_is_known_by_its_superblock() {
    assert_eq!(
        probe_bytes(&ext4("logs")).unwrap(),
        ("ext4", Some("logs".into()))
    );
    let mut ext2 = ext4("");
    ext2[1024 + 92..1024 + 100].fill(0);
    assert_eq!(probe_bytes(&ext2).unwrap(), ("ext2", None));
    assert_eq!(
        probe_bytes(&fat32(b"UPDATE     ")).unwrap(),
        ("vfat", Some("UPDATE".into()))
    );
    assert_eq!(probe_bytes(&fat32(b"NO NAME    ")).unwrap(), ("vfat", None));
    assert_eq!(
        probe_bytes(&exfat("Ægean 2026")).unwrap(),
        ("exfat", Some("Ægean 2026".into()))
    );
    assert_eq!(probe_bytes(&ntfs()).unwrap(), ("ntfs", None));
    assert_eq!(probe_bytes(&vec![0u8; 4096]).unwrap(), ("unknown", None));
    assert!(probe_bytes(&[0u8; 100]).is_err());
}

#[test]
fn a_volume_that_needs_repair_says_so() {
    assert!(!probed(&ext4("x")).unwrap().dirty);
    let mut journal = ext4("x");
    journal[1024 + 96] |= 0x4;
    assert!(probed(&journal).unwrap().dirty, "a journal to replay");
    let mut errors = ext4("x");
    errors[1024 + 58] = 3;
    assert!(probed(&errors).unwrap().dirty, "recorded errors");
    let mut fat = fat32(b"UPDATE     ");
    assert!(!probed(&fat).unwrap().dirty);
    fat[65] = 1;
    assert!(probed(&fat).unwrap().dirty);
    let mut ex = exfat("x");
    ex[106] = 2;
    assert!(probed(&ex).unwrap().dirty);
}

#[test]
fn a_label_is_display_text_only() {
    let mut hostile = fat32(b"../\x1b[31m\n..");
    assert_eq!(
        probe_bytes(&hostile).unwrap().1.as_deref(),
        Some("../[31m..")
    );
    hostile[71..82].copy_from_slice(b"           ");
    assert_eq!(probe_bytes(&hostile).unwrap().1, None);
    let long = exfat(&"x".repeat(30));
    assert_eq!(probe_bytes(&long).unwrap().1.unwrap().len(), 11);
}

/// The kernel as the host tree shows it: a mount is a mountinfo line.
struct FakeMounter {
    root: PathBuf,
    /// Targets whose unmount is refused as busy.
    busy: std::sync::Mutex<Vec<PathBuf>>,
}

impl FakeMounter {
    fn point(&self, target: &Path) -> String {
        format!("/{}", target.strip_prefix(&self.root).unwrap().display())
    }
}

impl Mounter for FakeMounter {
    fn mount(&self, device: &Path, target: &Path, filesystem: &str) -> std::io::Result<()> {
        let name = device.file_name().unwrap().to_str().unwrap();
        let devnum = fs::read_to_string(self.root.join(format!("sys/class/block/{name}/dev")))?;
        let (driver, data) = mount::options(filesystem).unwrap();
        let info = self.root.join("proc/self/mountinfo");
        let mut text = fs::read_to_string(&info)?;
        text.push_str(&format!(
            "60 1 {} / {} ro,nosuid,nodev,noexec - {driver} /dev/{name} ro,{}\n",
            devnum.trim(),
            self.point(target),
            data.to_str().unwrap()
        ));
        fs::write(info, text)
    }

    fn unmount(&self, target: &Path, _detach: bool) -> std::io::Result<()> {
        if self.busy.lock().unwrap().iter().any(|b| b == target) {
            return Err(std::io::Error::from_raw_os_error(
                rustix::io::Errno::BUSY.raw_os_error(),
            ));
        }
        let info = self.root.join("proc/self/mountinfo");
        let point = format!(" {} ", self.point(target));
        let text: String = fs::read_to_string(&info)?
            .lines()
            .filter(|line| !line.contains(&point))
            .map(|line| format!("{line}\n"))
            .collect();
        fs::write(info, text)
    }

    fn available(&self, _target: &Path) -> Option<u64> {
        Some(4096)
    }
}

/// A host tree: a soldered eMMC holding the device's own partitions (DATA
/// through a crypt mapping), an NVMe disk inside, a two-partition USB stick
/// and an SD card with no partition table.
struct Host {
    dir: tempfile::TempDir,
}

impl Host {
    fn new() -> Self {
        let host = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        host.write("proc/sys/kernel/random/boot_id", BOOT);
        host.link("proc/self/ns/mnt", "mnt:[4026531841]");
        host.write(
            "proc/filesystems",
            "nodev\tsysfs\n\text4\n\tvfat\n\texfat\n",
        );
        host.mounts("");
        let emmc = "devices/platform/fe2e0000.mmc/mmc_host/mmc0/mmc0:0001";
        host.disk(emmc, "mmcblk0", "179:0", 1, &[(3, "179:3"), (4, "179:4")]);
        host.write(&format!("sys/{emmc}/block/mmcblk0/device/type"), "MMC");
        host.disk(
            "devices/pci0000:00/0000:00:1d.0/nvme/nvme0",
            "nvme0n1",
            "259:0",
            2,
            &[],
        );
        host.usb_stick(5);
        let sd = "devices/platform/fe2c0000.mmc/mmc_host/mmc1/mmc1:aaaa";
        host.disk(sd, "mmcblk1", "179:32", 7, &[]);
        host.write(&format!("sys/{sd}/block/mmcblk1/device/type"), "SD");
        host.dev("mmcblk1", &exfat("CHARTS"));
        // DATA through `mica-data` on mmcblk0p4.
        host.write("sys/devices/virtual/block/dm-0/dev", "253:0");
        host.link("sys/class/block/dm-0", "../../devices/virtual/block/dm-0");
        host.link("sys/dev/block/253:0", "../../devices/virtual/block/dm-0");
        host.link(
            "sys/devices/virtual/block/dm-0/slaves/mmcblk0p4",
            &format!("../../../../{emmc}/block/mmcblk0/mmcblk0p4"),
        );
        host.mounts(
            "30 1 179:3 / /mnt/system ro - ext4 /dev/mmcblk0p3 ro\n\
             31 1 253:0 / /mnt/data rw - ext4 /dev/mapper/mica-data rw\n",
        );
        host
    }

    fn usb_stick(&self, seq: u32) {
        let usb = "devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/host0/target0:0:0/0:0:0:0";
        self.disk(usb, "sda", "8:0", seq, &[(1, "8:1"), (2, "8:2")]);
        self.dev("sda1", &fat32(b"UPDATE     "));
        self.dev("sda2", &ntfs());
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.dir.path().join(relative)
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn link(&self, relative: &str, target: &str) {
        let path = self.path(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let _ = fs::remove_file(&path);
        symlink(target, path).unwrap();
    }

    fn mounts(&self, extra: &str) {
        self.write(
            "proc/self/mountinfo",
            &format!("20 1 0:20 / / ro - overlay overlay ro\n{extra}"),
        );
    }

    fn dev(&self, name: &str, bytes: &[u8]) {
        self.write(&format!("dev/{name}"), "");
        fs::write(self.path(&format!("dev/{name}")), bytes).unwrap();
    }

    /// A disk under `parent` with partitions `(number, M:m)`.
    fn disk(&self, parent: &str, name: &str, devnum: &str, seq: u32, parts: &[(u32, &str)]) {
        let disk = format!("sys/{parent}/block/{name}");
        self.write(&format!("{disk}/diskseq"), &seq.to_string());
        self.write(&format!("{disk}/size"), "2048000");
        self.write(&format!("{disk}/dev"), devnum);
        self.link(&format!("sys/block/{name}"), &format!("../{}", &disk[4..]));
        self.link(
            &format!("sys/class/block/{name}"),
            &format!("../../{}", &disk[4..]),
        );
        self.link(
            &format!("sys/dev/block/{devnum}"),
            &format!("../../{}", &disk[4..]),
        );
        for (number, devnum) in parts {
            let part = if name.starts_with("mmcblk") || name.starts_with("nvme") {
                format!("{name}p{number}")
            } else {
                format!("{name}{number}")
            };
            let dir = format!("{disk}/{part}");
            self.write(&format!("{dir}/partition"), &number.to_string());
            self.write(&format!("{dir}/size"), "1024000");
            self.write(&format!("{dir}/dev"), devnum);
            self.link(
                &format!("sys/class/block/{part}"),
                &format!("../../{}", &dir[4..]),
            );
            self.link(
                &format!("sys/dev/block/{devnum}"),
                &format!("../../{}", &dir[4..]),
            );
        }
    }

    fn media(&self) -> HostMedia {
        HostMedia::at(
            self.dir.path(),
            Box::new(FakeMounter {
                root: self.dir.path().to_path_buf(),
                busy: std::sync::Mutex::new(Vec::new()),
            }),
        )
    }

    fn scan(&self) -> Inventory {
        self.media().inner.reconcile().unwrap()
    }
}

#[test]
fn usb_and_sd_volumes_are_listed_mounted_and_the_devices_own_disks_are_not() {
    let host = Host::new();
    let inventory = host.scan();
    let summary: Vec<_> = inventory
        .items
        .iter()
        .map(|v| {
            (
                v.transport,
                v.filesystem,
                v.label.as_deref(),
                v.state,
                v.reason,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("usb", "vfat", Some("UPDATE"), "mounted_readonly", None),
            (
                "usb",
                "ntfs",
                None,
                "unsupported",
                Some("filesystem_unsupported")
            ),
            ("sd", "exfat", Some("CHARTS"), "mounted_readonly", None),
        ]
    );
    let stick = &inventory.items[0];
    assert_eq!(stick.device_id, inventory.items[1].device_id);
    assert_ne!(stick.id, inventory.items[1].id);
    assert_ne!(stick.device_id, inventory.items[2].device_id);
    assert_eq!(stick.size_bytes, 1_024_000 * 512);
    assert_eq!(inventory.items[2].size_bytes, 2_048_000 * 512);
    assert!(inventory.items.iter().all(|v| !v.busy));
    assert_eq!(stick.available_bytes, Some(4096));
    assert_eq!(inventory.items[1].available_bytes, None);
    let info = fs::read_to_string(host.path("proc/self/mountinfo")).unwrap();
    assert!(
        info.contains(&format!(
            "8:1 / /run/mica/media/{} ro,nosuid,nodev,noexec - vfat",
            stick.id
        )),
        "{info}"
    );
    assert_eq!(host.scan().items, inventory.items, "mounted once");
    assert_eq!(
        fs::read_to_string(host.path("proc/self/mountinfo"))
            .unwrap()
            .matches("/run/mica/media/")
            .count(),
        2
    );
    let text = serde_json::to_value(&inventory).unwrap();
    assert_eq!(text["items"][0]["deviceId"], stick.device_id.as_str());
    assert!(text["items"][1]["label"].is_null());
}

#[test]
fn an_sd_card_the_device_booted_from_is_not_media() {
    let host = Host::new();
    host.write(
        "sys/devices/platform/fe2e0000.mmc/mmc_host/mmc0/mmc0:0001/block/mmcblk0/device/type",
        "SD",
    );
    let inventory = host.scan();
    assert_eq!(inventory.items.len(), 3, "{inventory:?}");
    assert!(
        inventory
            .items
            .iter()
            .all(|v| v.transport != "sd" || v.label.as_deref() == Some("CHARTS"))
    );
}

#[test]
fn ids_belong_to_one_attachment() {
    let host = Host::new();
    let media = host.media();
    let scan = || media.inner.reconcile().unwrap();
    let first = scan();
    assert_eq!(scan(), first, "stable while plugged");
    host.usb_stick(9);
    let again = scan();
    // In plug order: the SD card (7) now comes before the stick (9).
    assert_eq!(
        again.items[0], first.items[2],
        "the SD card was not touched"
    );
    assert_ne!(
        again.items[1].id, first.items[0].id,
        "a new id after a replug"
    );
    assert_ne!(again.items[1].device_id, first.items[0].device_id);
    assert_eq!(again.items[1].state, "mounted_readonly");
    assert_ne!(again.generation, first.generation);
    // The old plug's volumes are listed once as removed, their mount gone.
    let removed: Vec<_> = again.items[3..].iter().map(|v| (&v.id, v.state)).collect();
    assert_eq!(
        removed,
        [
            (&first.items[0].id, "removed"),
            (&first.items[1].id, "removed")
        ]
    );
    let info = fs::read_to_string(host.path("proc/self/mountinfo")).unwrap();
    assert!(!info.contains(&first.items[0].id), "{info}");
    assert_eq!(scan().items.len(), 3, "listed once");
}

#[test]
fn a_volume_mounted_outside_mica_is_busy() {
    let host = Host::new();
    host.mounts(
        "30 1 179:3 / /mnt/system ro - ext4 /dev/mmcblk0p3 ro\n\
         31 1 253:0 / /mnt/data rw - ext4 /dev/mapper/mica-data rw\n\
         40 31 8:1 / /mnt/data/srv/stick rw - vfat /dev/sda1 rw\n\
         41 1 179:32 / /run/mica/media/x ro - exfat /dev/mmcblk1 ro\n",
    );
    let inventory = host.scan();
    assert!(inventory.items[0].busy);
    assert_eq!(
        (inventory.items[0].state, inventory.items[0].reason),
        ("detected", Some("mounted_outside_mica")),
        "not mounted by MICA as well"
    );
    // A mount under the media root is MICA's, of a plug that is gone.
    assert!(!inventory.items[2].busy);
    assert_eq!(inventory.items[2].state, "mounted_readonly");
    let info = fs::read_to_string(host.path("proc/self/mountinfo")).unwrap();
    assert!(!info.contains("/run/mica/media/x "), "{info}");
}

#[test]
fn a_filesystem_the_kernel_lacks_is_unsupported() {
    let host = Host::new();
    host.write("proc/filesystems", "nodev\tsysfs\n\text4\n\tvfat\n");
    let sd = &host.scan().items[2];
    assert_eq!(
        (sd.state, sd.reason),
        ("unsupported", Some("filesystem_not_in_kernel"))
    );
}

#[test]
fn an_unreadable_volume_is_failed_and_the_rest_listed() {
    let host = Host::new();
    fs::remove_file(host.path("dev/sda2")).unwrap();
    let inventory = host.scan();
    assert_eq!(
        (inventory.items[1].state, inventory.items[1].reason),
        ("failed", Some("unreadable"))
    );
    assert_eq!(inventory.items[0].state, "mounted_readonly");
}

#[test]
fn a_volume_that_needs_repair_is_not_mounted() {
    let host = Host::new();
    let mut dirty = exfat("CHARTS");
    dirty[106] = 2;
    host.dev("mmcblk1", &dirty);
    let sd = host.scan().items[2].clone();
    assert_eq!((sd.state, sd.reason), ("failed", Some("needs_repair")));
    let info = fs::read_to_string(host.path("proc/self/mountinfo")).unwrap();
    assert!(!info.contains(&sd.id));
}

#[test]
fn a_volume_is_unmounted_alone_and_stays_unmounted() {
    let host = Host::new();
    let media = host.media();
    let first = media.inner.reconcile().unwrap();
    let (stick, sd) = (&first.items[0], &first.items[2]);
    let Unmount::Unmounted { item } = media.inner.unmount(&stick.id).unwrap() else {
        panic!("not unmounted");
    };
    assert_eq!(item.state, "unmounted");
    let again = media.inner.reconcile().unwrap();
    assert_eq!(again.items[0].state, "unmounted", "not mounted again");
    assert_eq!(
        again.items[2].state, "mounted_readonly",
        "the SD card stays"
    );
    let info = fs::read_to_string(host.path("proc/self/mountinfo")).unwrap();
    assert!(!info.contains(&stick.id) && info.contains(&sd.id), "{info}");
    assert!(!host.path(&format!("run/mica/media/{}", stick.id)).exists());
    assert!(matches!(
        media.inner.unmount(&stick.id).unwrap(),
        Unmount::Unmounted { .. }
    ));
    assert_eq!(
        media.inner.unmount("0123456789abcdef0123").unwrap(),
        Unmount::NotFound
    );
}

#[test]
fn a_busy_volume_stays_mounted() {
    let host = Host::new();
    let media = host.media();
    let sd = media.inner.reconcile().unwrap().items[2].clone();
    let fake = FakeMounter {
        root: host.dir.path().to_path_buf(),
        busy: std::sync::Mutex::new(vec![host.path(&format!("run/mica/media/{}", sd.id))]),
    };
    let busy = HostMedia {
        inner: Arc::new(Inner {
            root: host.dir.path().to_path_buf(),
            mounter: Box::new(fake),
            state: Mutex::new(State::default()),
        }),
    };
    let Unmount::Busy { item } = busy.inner.unmount(&sd.id).unwrap() else {
        panic!("not busy");
    };
    assert_eq!(item.state, "mounted_readonly");
    assert_eq!(
        busy.inner.reconcile().unwrap().items[2].state,
        "mounted_readonly"
    );
}

#[test]
fn an_unused_device_is_ejected_whole_and_stays_so() {
    let host = Host::new();
    let media = host.media();
    let first = media.inner.reconcile().unwrap();
    let stick = first.items[0].device_id.clone();
    let Eject::Ejected { items } = media.inner.eject(&stick).unwrap() else {
        panic!("not ejected");
    };
    assert_eq!(items.len(), 2);
    assert!(
        items
            .iter()
            .all(|v| v.state == "ejected" && v.device_id == stick)
    );
    let info = fs::read_to_string(host.path("proc/self/mountinfo")).unwrap();
    assert!(!info.contains(&first.items[0].id), "{info}");
    assert!(
        info.contains(&first.items[2].id),
        "the SD card stays mounted"
    );
    let again = media.inner.reconcile().unwrap();
    assert_eq!(
        (again.items[0].state, again.items[1].state),
        ("ejected", "ejected")
    );
    assert_eq!(again.items[2].state, "mounted_readonly");
    assert!(
        matches!(media.inner.eject(&stick).unwrap(), Eject::Ejected { .. }),
        "again"
    );
    assert_eq!(
        media.inner.eject("feedfacefeedfacefeed").unwrap(),
        Eject::NotFound
    );
}

/// Every way a device can be in use refuses its eject and unmounts nothing.
#[test]
fn a_device_in_use_is_not_ejected() {
    for (case, reason) in [
        ("other namespace", "mounted_in_other_namespace"),
        ("open", "open_by_process"),
        ("mapping", "used_by_mapping"),
        ("this namespace", "mounted_outside_mica"),
    ] {
        let host = Host::new();
        let media = host.media();
        let first = media.inner.reconcile().unwrap();
        let usb = "sys/devices/pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/host0/target0:0:0/0:0:0:0/block/sda";
        match case {
            "other namespace" => {
                host.link("proc/77/ns/mnt", "mnt:[4026532999]");
                host.write(
                    "proc/77/mountinfo",
                    "90 80 8:2 / /media/x rw - ntfs3 /dev/sda2 rw\n",
                );
            }
            "open" => host.link("proc/88/fd/3", "/dev/sda"),
            "mapping" => host.write(&format!("{usb}/sda2/holders/dm-3/.keep"), ""),
            _ => {
                let mut info = fs::read_to_string(host.path("proc/self/mountinfo")).unwrap();
                info.push_str("70 1 8:2 / /mnt/data/srv/x rw - ntfs3 /dev/sda2 rw\n");
                host.write("proc/self/mountinfo", &info);
            }
        }
        let Eject::Busy { items } = media.inner.eject(&first.items[0].device_id).unwrap() else {
            panic!("{case}: ejected");
        };
        let used: Vec<_> = items.iter().filter(|v| v.busy).map(|v| v.reason).collect();
        assert!(used.contains(&Some(reason)), "{case}: {items:?}");
        assert_eq!(
            items[0].state, "mounted_readonly",
            "{case}: nothing unmounted"
        );
        let info = fs::read_to_string(host.path("proc/self/mountinfo")).unwrap();
        assert!(info.contains(&first.items[0].id), "{case}");
    }
}

/// A copy of MICA's mount left in another namespace after MICA's own is
/// gone keeps the device from being safe to unplug.
#[test]
fn a_copy_left_in_another_namespace_keeps_the_device_unsafe() {
    let host = Host::new();
    let media = host.media();
    let first = media.inner.reconcile().unwrap();
    host.link("proc/77/ns/mnt", "mnt:[4026532999]");
    host.write(
        "proc/77/mountinfo",
        &format!(
            "90 80 8:1 / /run/mica/media/{} ro - vfat /dev/sda1 ro\n",
            first.items[0].id
        ),
    );
    let Eject::Busy { items } = media.inner.eject(&first.items[0].device_id).unwrap() else {
        panic!("ejected");
    };
    assert_eq!(items[0].state, "unmounted");
    assert_eq!(items[0].reason, Some("mounted_in_other_namespace"));
    assert_eq!(media.inner.reconcile().unwrap().items[0].state, "unmounted");
}

/// Volumes `mkfs` made, in `MICA_PROBE_DIR`, recognised with their labels.
#[test]
#[ignore = "cross-check against mkfs, run by hand"]
fn probe_cross_check() {
    let dir = std::path::PathBuf::from(std::env::var("MICA_PROBE_DIR").expect("MICA_PROBE_DIR"));
    for (file, filesystem, label) in [
        ("fat16.img", "vfat", Some("FAT16 VOL")),
        ("fat32.img", "vfat", Some("UPDATE")),
        ("fat32-nolabel.img", "vfat", None),
        ("exfat.img", "exfat", Some("Ægean 2026")),
        ("ext4.img", "ext4", Some("logs")),
        ("ext3.img", "ext3", Some("old")),
        ("ext2.img", "ext2", None),
        ("ntfs.img", "ntfs", None),
    ] {
        let image = File::open(dir.join(file)).unwrap();
        let probed = probe(|offset, buf| image.read_exact_at(buf, offset)).unwrap();
        assert_eq!(
            probed,
            Probed {
                filesystem,
                label: label.map(str::to_string),
                dirty: false
            },
            "{file}"
        );
    }
}

/// One boot of the media VM check (a uefi-x64 kernel; `/dev/vda` a virtio
/// disk; two USB sticks: one with a FAT32 `UPDATE` holding `hello.txt`, an
/// ext4 that needs recovery and an exFAT the kernel lacks, the other an
/// ext4 `logs` with no partition table). Run by its VM driver, not in CI.
#[test]
#[ignore = "runs as the init of a VM with USB sticks"]
fn vm_media() {
    use std::io::Write as _;
    let media = HostMedia::production();
    let inventory = media.inner.reconcile().unwrap();
    println!("VM media: {}", serde_json::to_string(&inventory).unwrap());
    let summary: Vec<_> = inventory
        .items
        .iter()
        .map(|v| {
            (
                v.transport,
                v.filesystem,
                v.label.as_deref(),
                v.state,
                v.reason,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("usb", "vfat", Some("UPDATE"), "mounted_readonly", None),
            ("usb", "ext4", Some("dirty"), "failed", Some("needs_repair")),
            (
                "usb",
                "exfat",
                Some("CHARTS"),
                "unsupported",
                Some("filesystem_not_in_kernel")
            ),
            ("usb", "ext4", Some("logs"), "mounted_readonly", None),
        ]
    );
    let update = &inventory.items[0];
    let place = Path::new(MEDIA_ROOT).join(&update.id);
    assert_eq!(
        fs::read_to_string(place.join("hello.txt")).unwrap(),
        "mica\n"
    );
    let info = fs::read_to_string("/proc/self/mountinfo").unwrap();
    let line = info
        .lines()
        .find(|l| l.contains(&update.id))
        .unwrap()
        .to_string();
    for option in ["ro", "nosuid", "nodev", "noexec"] {
        assert!(
            line.split_whitespace()
                .nth(5)
                .unwrap()
                .split(',')
                .any(|o| o == option),
            "{line}"
        );
    }
    println!("VM media: {line}");
    let written = fs::File::create(place.join("new.txt"));
    assert_eq!(
        written.unwrap_err().raw_os_error(),
        Some(rustix::io::Errno::ROFS.raw_os_error())
    );
    // Busy while a file is open, then unmounted alone.
    let open = fs::File::open(place.join("hello.txt")).unwrap();
    assert!(matches!(
        media.inner.unmount(&update.id).unwrap(),
        Unmount::Busy { .. }
    ));
    drop(open);
    assert!(matches!(
        media.inner.unmount(&update.id).unwrap(),
        Unmount::Unmounted { .. }
    ));
    let again = media.inner.reconcile().unwrap();
    assert_eq!(again.items[0].state, "unmounted");
    assert_eq!(
        again.items[3].state, "mounted_readonly",
        "the other stick stays"
    );
    assert!(
        !fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .contains(&update.id)
    );
    // Eject: a file open on the mounted stick refuses at its unmount; the
    // other stick's device node held open refuses before anything moves.
    let logs = &again.items[3];
    let place = Path::new(MEDIA_ROOT).join(&logs.id);
    let file = fs::File::open(place.join("lost+found")).unwrap();
    let Eject::Busy { items } = media.inner.eject(&logs.device_id).unwrap() else {
        panic!("ejected with a file open");
    };
    assert_eq!(items[0].reason, Some("open_files"));
    drop(file);
    let node = fs::File::open("/dev/sda").unwrap();
    let Eject::Busy { items } = media.inner.eject(&update.device_id).unwrap() else {
        panic!("ejected with its node open");
    };
    assert!(
        items.iter().all(|v| v.reason == Some("open_by_process")),
        "{items:?}"
    );
    drop(node);
    for device in [&update.device_id, &logs.device_id] {
        let Eject::Ejected { items } = media.inner.eject(device).unwrap() else {
            panic!("not ejected");
        };
        println!(
            "VM media: ejected {}",
            serde_json::to_string(&items).unwrap()
        );
    }
    let after = media.inner.reconcile().unwrap();
    assert!(
        after.items.iter().all(|v| v.state == "ejected"),
        "{after:?}"
    );
    assert!(
        !fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .contains(MEDIA_ROOT)
    );
    std::io::stdout().flush().unwrap();
    println!("VM_MEDIA PASS");
}

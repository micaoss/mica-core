use std::fs;
use std::os::unix::fs::symlink;

use super::*;

const BOOT: &str = "3f0c7d1e-8f44-4c2a-9a55-0d6c1f2e7b10";

/// An ext4 superblock with `label`.
fn ext4(label: &str) -> Vec<u8> {
    let mut b = vec![0u8; 8192];
    b[1024 + 56..1024 + 58].copy_from_slice(&0xEF53u16.to_le_bytes());
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

    fn scan(&self) -> Inventory {
        HostMedia::at(self.dir.path()).scan().unwrap()
    }
}

#[test]
fn usb_and_sd_volumes_are_listed_and_the_devices_own_disks_are_not() {
    let inventory = Host::new().scan();
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
            ("usb", "vfat", Some("UPDATE"), "detected", None),
            (
                "usb",
                "ntfs",
                None,
                "unsupported",
                Some("filesystem_unsupported")
            ),
            ("sd", "exfat", Some("CHARTS"), "detected", None),
        ]
    );
    let stick = &inventory.items[0];
    assert_eq!(stick.device_id, inventory.items[1].device_id);
    assert_ne!(stick.id, inventory.items[1].id);
    assert_ne!(stick.device_id, inventory.items[2].device_id);
    assert_eq!(stick.size_bytes, 1_024_000 * 512);
    assert_eq!(inventory.items[2].size_bytes, 2_048_000 * 512);
    assert!(
        inventory
            .items
            .iter()
            .all(|v| !v.busy && v.available_bytes.is_none())
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
    let first = host.scan();
    assert_eq!(host.scan(), first, "stable while plugged");
    host.usb_stick(9);
    let again = host.scan();
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
    assert_ne!(again.generation, first.generation);
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
    assert_eq!(inventory.items[0].reason, Some("mounted_outside_mica"));
    assert!(!inventory.items[2].busy, "MICA's own media mount");
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
    assert_eq!(inventory.items[0].state, "detected");
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
        assert_eq!(probed, (filesystem, label.map(str::to_string)), "{file}");
    }
}

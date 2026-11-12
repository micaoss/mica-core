//! A volume's filesystem and label from its superblock, read only.

use std::io;

/// The longest label shown.
const LABEL_CHARS: usize = 64;

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A label for display: control characters dropped, padding trimmed, at
/// most [`LABEL_CHARS`] characters, none when empty.
fn label(text: &str) -> Option<String> {
    let clean: String = text
        .chars()
        .filter(|c| !c.is_control())
        .take(LABEL_CHARS)
        .collect();
    let clean = clean.trim();
    (!clean.is_empty()).then(|| clean.to_string())
}

fn ascii_label(bytes: &[u8]) -> Option<String> {
    label(&String::from_utf8_lossy(bytes).replace('\0', ""))
}

/// The filesystem on a volume and its label, through `read(offset, buf)`,
/// which fills `buf` from `offset`: `vfat`, `exfat`, `ext2`, `ext3`, `ext4`,
/// `ntfs`, or `unknown`.
///
/// # Errors
///
/// Returns the read's error when the volume cannot be read.
pub fn probe(
    read: impl Fn(u64, &mut [u8]) -> io::Result<()>,
) -> io::Result<(&'static str, Option<String>)> {
    let mut head = vec![0u8; 4096];
    read(0, &mut head)?;
    let sb = &head[1024..2048];
    if u16_at(sb, 56) == 0xEF53 {
        let compat = u32_at(sb, 92);
        let incompat = u32_at(sb, 96);
        // extents, 64bit, flex_bg
        let kind = if incompat & (0x40 | 0x80 | 0x200) != 0 {
            "ext4"
        } else if compat & 0x4 != 0 {
            "ext3"
        } else {
            "ext2"
        };
        return Ok((kind, ascii_label(&sb[120..136])));
    }
    if &head[3..11] == b"EXFAT   " {
        return Ok(("exfat", exfat_label(&head, &read)));
    }
    if &head[3..11] == b"NTFS    " {
        return Ok(("ntfs", None));
    }
    let sector = u16_at(&head, 11);
    if head[510..512] == [0x55, 0xAA] && matches!(sector, 512 | 1024 | 2048 | 4096) {
        let at = if &head[82..90] == b"FAT32   " {
            Some(71)
        } else if head[54..58] == *b"FAT1" {
            Some(43)
        } else {
            None
        };
        if let Some(at) = at {
            let name = ascii_label(&head[at..at + 11]).filter(|name| name != "NO NAME");
            return Ok(("vfat", name));
        }
    }
    Ok(("unknown", None))
}

/// The exFAT volume label: the label entry (type 0x83) in the first cluster
/// of the root directory, when the boot sector's geometry is sane.
fn exfat_label(head: &[u8], read: &impl Fn(u64, &mut [u8]) -> io::Result<()>) -> Option<String> {
    let sector_shift = u32::from(head[108]);
    let cluster_shift = u32::from(head[109]);
    if !(9..=12).contains(&sector_shift) || sector_shift + cluster_shift > 25 {
        return None;
    }
    let heap = u64::from(u32_at(head, 88));
    let root = u64::from(u32_at(head, 96)).checked_sub(2)?;
    let offset = (heap + (root << cluster_shift)) << sector_shift;
    let mut cluster = vec![0u8; (1usize << (sector_shift + cluster_shift)).min(65536)];
    read(offset, &mut cluster).ok()?;
    for entry in cluster.as_chunks::<32>().0 {
        match entry[0] {
            0x00 => return None,
            0x83 => {
                let count = usize::from(entry[1]).min(11);
                let units: Vec<u16> = entry[2..2 + 2 * count]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
                    .collect();
                return label(&String::from_utf16_lossy(&units));
            }
            _ => {}
        }
    }
    None
}

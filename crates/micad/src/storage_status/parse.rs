//! Parsers for what the kernel and the storage tools print.

use std::collections::BTreeMap;

use super::*;

/// The JEDEC `DEVICE_LIFE_TIME_EST_TYP_A/B` bucket a raw field names, as the
/// inclusive-exclusive percentage range it actually means.
#[must_use]
pub fn parse_life_time(raw: &str) -> Option<(u8, u8)> {
    let value = u8::from_str_radix(raw.trim().trim_start_matches("0x"), 16).ok()?;
    match value {
        0 => None,
        0x0b => Some((100, 100)),
        1..=0x0a => Some(((value - 1) * 10, value * 10)),
        _ => None,
    }
}

/// The JEDEC `PRE_EOL_INFO` value a raw field names.
#[must_use]
pub fn parse_pre_eol(raw: &str) -> &'static str {
    match u8::from_str_radix(raw.trim().trim_start_matches("0x"), 16) {
        Ok(0x01) => "normal",
        Ok(0x02) => "warning",
        Ok(0x03) => "urgent",
        // Includes 0x00, which JEDEC defines as "not defined": the device
        // declines to answer, which is not the same as "normal".
        _ => "undefined",
    }
}

/// Parse `/proc/self/mountinfo`.
///
/// Field layout: `id parent major:minor root mountpoint options [tags...] -
/// fstype source superoptions`. The optional tag run before the `-` is why
/// the fields after it cannot be indexed from the left.
#[must_use]
pub fn parse_mountinfo(text: &str) -> Vec<MountEvidence> {
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let left: Vec<&str> = left.split_whitespace().collect();
            let right: Vec<&str> = right.split_whitespace().collect();
            Some(MountEvidence {
                device: (*right.get(1)?).to_string(),
                root: unescape_octal(left.get(3)?),
                mount: unescape_octal(left.get(4)?),
                fstype: (*right.first()?).to_string(),
                read_only: left.get(5)?.split(',').any(|option| option == "ro"),
            })
        })
        .collect()
}

/// Decode the octal escapes the kernel writes into mountinfo paths (space,
/// tab, newline and backslash).
pub(super) fn unescape_octal(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let digits: String = chars.clone().take(3).collect();
        match u8::from_str_radix(&digits, 8) {
            Ok(byte) if digits.len() == 3 => {
                out.push(char::from(byte));
                chars.nth(2);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Decode a systemd unit-name escape back to the path it names.
///
/// systemd writes `/` as `-` and every other non-alphanumeric byte as
/// `\xNN`, so `systemd-fsck@dev-disk-by\x2dpartuuid-1234.service` is a check
/// of `/dev/disk/by-partuuid/1234`. The `\x2d` case is why this cannot be a
/// plain `replace('-', "/")`: a partuuid path is full of literal hyphens.
#[must_use]
pub fn unescape_unit_name(escaped: &str) -> String {
    let bytes = escaped.as_bytes();
    let mut out = String::with_capacity(escaped.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' if index + 3 < bytes.len() && bytes[index + 1] == b'x' => {
                let digits = &escaped[index + 2..index + 4];
                if let Ok(byte) = u8::from_str_radix(digits, 16) {
                    out.push(char::from(byte));
                    index += 4;
                } else {
                    out.push('\\');
                    index += 1;
                }
            }
            b'-' => {
                out.push('/');
                index += 1;
            }
            byte => {
                out.push(char::from(byte));
                index += 1;
            }
        }
    }
    out
}

/// The device path a `systemd-fsck@….service` unit checks, or `None` when the
/// unit is not one of those.
#[must_use]
pub fn fsck_unit_device(unit: &str) -> Option<String> {
    let instance = unit
        .strip_prefix("systemd-fsck@")?
        .strip_suffix(".service")?;
    if instance.is_empty() {
        return None;
    }
    Some(format!("/{}", unescape_unit_name(instance)))
}

/// Pair each recorded fsck unit with the canonical device it checked.
#[must_use]
pub fn checks_by_device<R>(units: &[CheckEvidence], resolve: R) -> BTreeMap<String, CheckEvidence>
where
    R: Fn(&str) -> Option<String>,
{
    units
        .iter()
        .filter_map(|check| {
            let named = fsck_unit_device(&check.unit)?;
            let device = resolve(&named).unwrap_or(named);
            Some((device, check.clone()))
        })
        .collect()
}

/// Parse one `df -P -B1 <mount>` answer into a [`FsSpace`].
///
/// `df` rather than `statvfs(3)`: the workspace forbids `unsafe`, so there is
/// no FFI call available, and coreutils is Essential on the image. The
/// reserved pool is `total - used - free`, which is the only place that
/// number is visible at all.
#[must_use]
pub fn parse_df(output: &str) -> Option<FsSpace> {
    let line = output.lines().nth(1)?;
    let fields: Vec<&str> = line.split_whitespace().collect();
    // Filesystem, blocks, used, available, capacity, mountpoint.
    if fields.len() < 6 {
        return None;
    }
    let total: u64 = fields[1].parse().ok()?;
    let used: u64 = fields[2].parse().ok()?;
    let free: u64 = fields[3].parse().ok()?;
    Some(FsSpace {
        total,
        used,
        free,
        reserved: total.saturating_sub(used).saturating_sub(free),
    })
}

//! The user's own mounts and swaps (`docs/plan/user-storage.md` 3 and 5):
//! every mount in the places a user may mount, and every active swap, each
//! marked `user` when one of the user's units or fstab lines declares it.

use std::collections::BTreeSet;

use super::*;

/// Where `mica-unit` lets a user mount, besides `/srv` itself: under
/// `/srv`, `/var/mnt` and a user's home, and exactly at an engine's root.
#[must_use]
pub fn in_user_area(target: &str) -> bool {
    let under = |base: &str| {
        target
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/') && rest.len() > 1)
    };
    target == "/srv"
        || under("/srv")
        || under("/var/mnt")
        || target
            .strip_prefix("/home/")
            .is_some_and(|user| !user.is_empty() && !user.starts_with('/'))
        || matches!(
            target,
            "/mica/containers/storage" | "/mica/containers/docker"
        )
}

/// One mount in a user area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMount {
    pub target: String,
    pub source: String,
    pub filesystem: String,
    /// The mount's own options, as mountinfo records them.
    pub options: String,
    /// Declared by one of the user's units or fstab lines.
    pub user: bool,
    /// Space, for a mount of a block device only: reading it on an autofs
    /// trigger would mount it, and a network filesystem may not answer.
    pub space: Option<FsSpace>,
}

/// One active swap, from `/proc/swaps`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapEvidence {
    pub source: String,
    /// `partition` or `file`.
    pub kind: String,
    pub size_bytes: u64,
    pub used_bytes: u64,
    pub user: bool,
}

/// What the user's units and fstab declare: mount targets (`Where=` of a
/// `.mount` or `.automount`, an fstab line's second field) and swap sources
/// (`What=` of a `.swap`, an fstab `swap` line's first field).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Declared {
    pub targets: BTreeSet<String>,
    pub swaps: BTreeSet<String>,
}

impl Declared {
    /// Add the systemd unit file `name` with `text`.
    pub fn unit(&mut self, name: &str, text: &str) {
        let key = if name.ends_with(".swap") {
            "What="
        } else if name.ends_with(".mount") || name.ends_with(".automount") {
            "Where="
        } else {
            return;
        };
        if let Some(value) = text
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix(key))
        {
            let value = value.trim().trim_end_matches('/');
            let set = if key == "What=" {
                &mut self.swaps
            } else {
                &mut self.targets
            };
            set.insert(if value.is_empty() { "/" } else { value }.to_string());
        }
    }

    /// Add an fstab's `text`.
    pub fn fstab(&mut self, text: &str) {
        for line in text.lines() {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.first().is_none_or(|first| first.starts_with('#')) {
                continue;
            }
            match fields.as_slice() {
                [source, _, "swap", ..] => {
                    self.swaps.insert(unescape_octal(source));
                }
                [_, target, ..] => {
                    self.targets.insert(unescape_octal(target));
                }
                _ => {}
            }
        }
    }
}

/// Parse `/proc/swaps` (`Filename Type Size Used Priority`, sizes in KiB).
#[must_use]
pub fn parse_swaps(text: &str, declared: &Declared) -> Vec<SwapEvidence> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let [source, kind, size, used, ..] = fields.as_slice() else {
                return None;
            };
            let source = unescape_octal(source);
            Some(SwapEvidence {
                user: declared.swaps.contains(&source),
                source,
                kind: (*kind).to_string(),
                size_bytes: size.parse::<u64>().ok()?.saturating_mul(1024),
                used_bytes: used.parse::<u64>().ok()?.saturating_mul(1024),
            })
        })
        .collect()
}

/// The mounts in a user area, but the default `/srv` bind (the DATA
/// directory `srv` on `data_device`).
#[must_use]
pub fn user_mounts(
    mounts: &[MountEvidence],
    data_device: Option<&str>,
    declared: &Declared,
) -> Vec<UserMount> {
    mounts
        .iter()
        .filter(|mount| in_user_area(&mount.mount))
        .filter(|mount| {
            !(mount.mount == "/srv"
                && mount.root == "/srv"
                && Some(mount.device.as_str()) == data_device)
        })
        .map(|mount| UserMount {
            target: mount.mount.clone(),
            source: mount.device.clone(),
            filesystem: mount.fstype.clone(),
            options: mount.options.clone(),
            user: declared.targets.contains(&mount.mount),
            space: None,
        })
        .collect()
}

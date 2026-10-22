//! The devices an app of its own user declares, and what they grant it.
//!
//! Serial adapters, GPIO chips, I2C and SPI buses come and go, so a device is
//! granted by its class rather than by the node present at install: the
//! driver's nodes (`DeviceAllow=char-<driver>` under systemd) and the group
//! that opens them. A device declared by a stable path or a pattern needs
//! nothing resolved at start: the app opens the path itself, and every node
//! of the class is open to it through the group.

use std::path::{Path, PathBuf};

/// The groups a device node may have for an app's own user to be given it,
/// besides 0, which is never given.
pub const ALLOWED_GROUPS: [&str; 4] = ["dialout", "gpio", "i2c", "spi"];

/// How a class of devices is granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    /// Their drivers' names in `/proc/devices`.
    pub drivers: Vec<String>,
    /// The group that opens their nodes.
    pub group: &'static str,
}

/// The class of `device`, if it has one: a serial port (`/dev/tty<name><n>`,
/// `/dev/serial/by-id/...`, `/dev/serial/by-path/...`), a GPIO chip, an I2C
/// or an SPI bus. Any other device is granted as its node.
pub fn class(device: &str) -> Option<Class> {
    let class = |drivers: &[&str], group| {
        Some(Class {
            drivers: drivers.iter().map(|driver| (*driver).to_string()).collect(),
            group,
        })
    };
    if device.starts_with("/dev/serial/by-id/") || device.starts_with("/dev/serial/by-path/") {
        return class(&["ttyUSB", "ttyACM"], "dialout");
    }
    if device.starts_with("/dev/gpiochip") {
        return class(&["gpiochip"], "gpio");
    }
    if device.starts_with("/dev/i2c-") {
        return class(&["i2c"], "i2c");
    }
    if device.starts_with("/dev/spidev") {
        return class(&["spi"], "spi");
    }
    let rest = device.strip_prefix("/dev/tty")?;
    let letters = rest.len()
        - rest
            .trim_start_matches(|c: char| c.is_ascii_alphabetic())
            .len();
    let (name, number) = rest.split_at(letters);
    (!name.is_empty()
        && !number.is_empty()
        && (number == "*" || number.bytes().all(|b| b.is_ascii_digit())))
    .then(|| Class {
        drivers: vec![format!("tty{name}")],
        group: "dialout",
    })
}

/// Whether `device` names every node with its prefix: it ends with `*`.
pub fn is_pattern(device: &str) -> bool {
    device.ends_with('*')
}

/// The nodes under `root` `device` names now: those its pattern matches, or
/// its own path when it is there.
pub fn present(root: &Path, device: &str) -> Vec<PathBuf> {
    let path = root.join(device.trim_start_matches('/'));
    if !is_pattern(device) {
        return if path.exists() {
            vec![path]
        } else {
            Vec::new()
        };
    }
    let (Some(dir), Some(prefix)) = (path.parent(), path.file_name()) else {
        return Vec::new();
    };
    let prefix = prefix.to_string_lossy().trim_end_matches('*').to_string();
    let mut nodes: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
        .map(|entry| entry.path())
        .collect();
    nodes.sort();
    nodes
}

/// The gid `/etc/group` text `groups` gives `name`.
pub fn gid_of(groups: &str, name: &str) -> Option<u32> {
    groups.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next()? == name).then_some(())?;
        fields.nth(1)?.parse().ok()
    })
}

/// Why a node of group `gid` may not be an app's own user's, given the root's
/// `/etc/group`: its group is none of [`ALLOWED_GROUPS`]. Group 0 is never
/// given, so it is never refused.
pub fn group_refusal(gid: u32, groups: Option<&str>) -> Option<String> {
    if gid == 0 {
        return None;
    }
    let id = gid.to_string();
    let name = groups.and_then(|text| {
        text.lines().find_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            (fields.nth(1)? == id).then_some(name)
        })
    });
    match name {
        Some(name) if ALLOWED_GROUPS.contains(&name) => None,
        Some(name) => Some(format!(
            "belongs to group {name}, which an app of its own user may not join"
        )),
        None => Some(format!(
            "belongs to gid {gid}, which has no name in /etc/group"
        )),
    }
}

/// What the devices an app declares grant its services.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grants {
    /// Its supplementary groups.
    pub groups: Vec<u32>,
    /// `DeviceAllow=` of each, before ` rw`: `char-<driver>` or a node.
    pub allow: Vec<String>,
}

/// What `devices` grant on `root`, whose `/etc/group` is `groups`: a class's
/// drivers and group, or a node's path and, when it is there and allowed,
/// its group.
pub fn grants(root: &Path, devices: &[String], groups: Option<&str>) -> Grants {
    use std::os::unix::fs::MetadataExt;
    let mut grants = Grants::default();
    for device in devices {
        match class(device) {
            Some(class) => {
                grants
                    .allow
                    .extend(class.drivers.iter().map(|driver| format!("char-{driver}")));
                grants
                    .groups
                    .extend(groups.and_then(|groups| gid_of(groups, class.group)));
            }
            None => {
                grants.allow.push(device.clone());
                grants.groups.extend(
                    std::fs::metadata(root.join(device.trim_start_matches('/')))
                        .ok()
                        .map(|meta| meta.gid())
                        .filter(|gid| *gid != 0 && group_refusal(*gid, groups).is_none()),
                );
            }
        }
    }
    grants.groups.sort_unstable();
    grants.groups.dedup();
    grants.allow.dedup();
    grants
}

#[cfg(test)]
mod tests {
    use super::*;

    const GROUPS: &str =
        "root:x:0:\ntty:x:5:\ndialout:x:20:\nspi:x:983:\ngpio:x:984:\ni2c:x:987:\n";

    #[test]
    fn a_device_has_the_class_of_its_driver() {
        let drivers = |device| class(device).map(|class| (class.drivers, class.group));
        assert_eq!(
            drivers("/dev/ttyUSB*"),
            Some((vec!["ttyUSB".into()], "dialout"))
        );
        assert_eq!(
            drivers("/dev/ttyACM0"),
            Some((vec!["ttyACM".into()], "dialout"))
        );
        assert_eq!(
            drivers("/dev/ttyS1"),
            Some((vec!["ttyS".into()], "dialout"))
        );
        assert_eq!(
            drivers("/dev/serial/by-id/usb-FTDI_FT232R_USB_UART_A50285BI-if00-port0"),
            Some((vec!["ttyUSB".into(), "ttyACM".into()], "dialout"))
        );
        assert_eq!(
            drivers("/dev/gpiochip0"),
            Some((vec!["gpiochip".into()], "gpio"))
        );
        assert_eq!(drivers("/dev/i2c-1"), Some((vec!["i2c".into()], "i2c")));
        assert_eq!(drivers("/dev/spidev0.0"), Some((vec!["spi".into()], "spi")));
        for device in [
            "/dev/tty",
            "/dev/tty1",
            "/dev/ttyS",
            "/dev/net/tun",
            "/dev/sda",
        ] {
            assert_eq!(class(device), None, "{device}");
        }
    }

    #[test]
    fn a_class_is_granted_whether_its_nodes_are_there_or_not() {
        let root = tempfile::tempdir().unwrap();
        let devices = [
            "/dev/ttyUSB*",
            "/dev/serial/by-id/usb-x",
            "/dev/i2c-1",
            "/dev/net/tun",
        ]
        .map(String::from);
        let grants = grants(root.path(), &devices, Some(GROUPS));
        assert_eq!(grants.groups, [20, 987]);
        assert_eq!(
            grants.allow,
            ["char-ttyUSB", "char-ttyACM", "char-i2c", "/dev/net/tun"]
        );
    }

    #[test]
    fn a_pattern_names_the_nodes_with_its_prefix() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("dev")).unwrap();
        for node in ["ttyUSB0", "ttyUSB1", "ttyS0"] {
            std::fs::write(root.path().join("dev").join(node), "").unwrap();
        }
        let names = |device| -> Vec<String> {
            present(root.path(), device)
                .iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(names("/dev/ttyUSB*"), ["ttyUSB0", "ttyUSB1"]);
        assert_eq!(names("/dev/ttyS0"), ["ttyS0"]);
        assert!(names("/dev/ttyACM*").is_empty());
        assert!(names("/dev/ttyACM0").is_empty());
    }

    #[test]
    fn only_the_device_groups_are_joined() {
        assert_eq!(group_refusal(0, None), None);
        for gid in [20, 983, 984, 987] {
            assert_eq!(group_refusal(gid, Some(GROUPS)), None, "{gid}");
        }
        for (gid, groups) in [(5, Some(GROUPS)), (44, Some(GROUPS)), (20, None)] {
            assert!(group_refusal(gid, groups).is_some(), "{gid}");
        }
    }
}

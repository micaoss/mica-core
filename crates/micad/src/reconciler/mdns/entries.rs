//! What scripts and apps ask the responder to publish: the drop-in files of
//! the image and of the running system, apps' `mdns` entries, and the rules
//! they are held to (docs/mica-core.md section 3.13).

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// The image's drop-ins, read-only in the root.
pub const IMAGE_DIR: &str = "/usr/share/mica/mdns.d";
/// Drop-ins from scripts while the system runs; gone at reboot.
pub const RUNTIME_DIR: &str = "/run/mica/mdns.d";
/// The drop-in format this build reads.
const FILE_VERSION: u32 = 1;
/// The most services one drop-in file or one app declares.
pub const MAX_SERVICES: usize = 8;
/// The most services published at once, the built-in ones included.
pub const MAX_PUBLISHED: usize = 64;
/// The most TXT keys of one service.
const MAX_TXT_KEYS: usize = 16;
/// The longest `key=value` of a TXT record.
const MAX_TXT_ITEM: usize = 255;
/// The most bytes of one service's TXT record.
const MAX_TXT_BYTES: usize = 1300;
/// The longest instance label.
const MAX_INSTANCE: usize = 63;
/// The largest drop-in file read.
const MAX_FILE_BYTES: u64 = 64 * 1024;
/// The longest drop-in name, before `.json`.
const MAX_NAME: usize = 32;

/// One service a drop-in file or an app asks for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The instance label; the hostname when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// `_<name>._tcp` or `_<name>._udp`.
    #[serde(rename = "type")]
    pub kind: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub txt: BTreeMap<String, String>,
    /// An app's entry only: the service or unit of the app whose running it
    /// follows; any of its units when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
}

/// A drop-in file.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    version: u32,
    services: Vec<Entry>,
}

/// A drop-in name: 1 to 32 of a-z, 0-9 and -, a letter or digit first.
pub fn is_name(name: &str) -> bool {
    name.len() <= MAX_NAME
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A DNS-SD service type: `_<name>._tcp` or `_<name>._udp`, `<name>` an
/// RFC 6335 service name of at most 15 characters.
pub fn is_type(kind: &str) -> bool {
    let Some(name) = kind
        .strip_suffix("._tcp")
        .or_else(|| kind.strip_suffix("._udp"))
        .and_then(|rest| rest.strip_prefix('_'))
    else {
        return false;
    };
    (1..=15).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && name.bytes().any(|b| b.is_ascii_lowercase())
        && !name.starts_with('-')
        && !name.ends_with('-')
        && !name.contains("--")
}

/// The rule `entry` breaks, if any; `service` is checked by its owner.
pub fn check(entry: &Entry) -> Result<(), String> {
    if let Some(instance) = &entry.instance
        && (instance.is_empty()
            || instance.len() > MAX_INSTANCE
            || instance.chars().any(|c| c == '.' || c.is_control()))
    {
        return Err(format!(
            "instance {instance:?} is not 1 to {MAX_INSTANCE} bytes without a dot or a control character"
        ));
    }
    if !is_type(&entry.kind) {
        return Err(format!(
            "type {:?} is not _<name>._tcp or _<name>._udp",
            entry.kind
        ));
    }
    if entry.kind == super::MICA_TYPE {
        return Err(format!("type {} is the device's own", super::MICA_TYPE));
    }
    if entry.port == 0 {
        return Err("port is 1 to 65535".to_string());
    }
    if entry.txt.len() > MAX_TXT_KEYS {
        return Err(format!("txt has more than {MAX_TXT_KEYS} keys"));
    }
    let mut bytes = 0;
    for (key, value) in &entry.txt {
        if key.is_empty() || !key.bytes().all(|b| (0x20..0x7f).contains(&b) && b != b'=') {
            return Err(format!("txt key {key:?} is not printable ASCII without ="));
        }
        let item = key.len() + 1 + value.len();
        if item > MAX_TXT_ITEM {
            return Err(format!("txt {key} is above {MAX_TXT_ITEM} bytes"));
        }
        bytes += item + 1;
    }
    if bytes > MAX_TXT_BYTES {
        return Err(format!("txt is above {MAX_TXT_BYTES} bytes"));
    }
    Ok(())
}

/// Every rule `entries`, an app's or a file's, break.
pub fn check_all(entries: &[Entry]) -> Result<(), String> {
    if entries.len() > MAX_SERVICES {
        return Err(format!("more than {MAX_SERVICES} services"));
    }
    entries.iter().try_for_each(check)
}

/// The drop-ins of the image and of the running system, by source
/// (`image:<name>`, `runtime:<name>`): each file's services, or why it is
/// refused. A runtime file replaces the image's of the same name, and must be
/// `owner`'s: root on a device.
pub fn drop_ins(
    image: &Path,
    runtime: &Path,
    owner: u32,
) -> BTreeMap<String, Result<Vec<Entry>, String>> {
    let mut by_name: BTreeMap<String, (&str, Result<Vec<Entry>, String>)> = BTreeMap::new();
    for (dir, origin) in [(image, "image"), (runtime, "runtime")] {
        let Ok(files) = std::fs::read_dir(dir) else {
            continue;
        };
        for file in files.flatten() {
            let file_name = file.file_name();
            let Some(name) = file_name
                .to_str()
                .and_then(|n| n.strip_suffix(".json"))
                .filter(|n| is_name(n))
            else {
                continue;
            };
            let read = read_file(&file.path(), (origin == "runtime").then_some(owner));
            by_name.insert(name.to_string(), (origin, read));
        }
    }
    by_name
        .into_iter()
        .map(|(name, (origin, read))| (format!("{origin}:{name}"), read))
        .collect()
}

/// The services of the drop-in at `path`; a runtime one must be `owner`'s
/// regular file and writable by `owner` alone.
fn read_file(path: &Path, owner: Option<u32>) -> Result<Vec<Entry>, String> {
    let meta = path.symlink_metadata().map_err(|err| err.to_string())?;
    if !meta.is_file() {
        return Err("not a regular file".to_string());
    }
    if let Some(owner) = owner
        && (meta.uid() != owner || meta.mode() & 0o022 != 0)
    {
        return Err("not owned by root, or writable by others than root".to_string());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("above {} KiB", MAX_FILE_BYTES >> 10));
    }
    let text = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    let file: File = serde_json::from_str(&text).map_err(|err| err.to_string())?;
    if file.version != FILE_VERSION {
        return Err(format!("version {} is not {FILE_VERSION}", file.version));
    }
    if file.services.iter().any(|entry| entry.service.is_some()) {
        return Err("service is an app's key".to_string());
    }
    check_all(&file.services)?;
    Ok(file.services)
}

#[cfg(test)]
mod tests;

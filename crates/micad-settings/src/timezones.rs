//! The device's time zones, as its tzdata carries them: whether a name is a
//! zone, and the catalog a console picks from.

use std::path::Path;

use serde::Serialize;

use crate::validate_timezone_name;

/// Where zone files live.
pub const DEFAULT_ZONEINFO_DIR: &str = "/usr/share/zoneinfo";

/// Whether `zone` is a zone the tzdata under `zoneinfo_dir` carries.
///
/// The grammar first ([`validate_timezone_name`] admits no empty, `.` or
/// `..` component), so a name never climbs out of the tree. `UTC` needs no
/// file: it is what every consumer falls back to. Any other name is a zone
/// only when its file is a TZif file, so the tzdata's own metadata
/// (`zone.tab`, `tzdata.zi`, `leapseconds`) is not one.
#[must_use]
pub fn zone_installed(zoneinfo_dir: &Path, zone: &str) -> bool {
    use std::io::Read;
    if validate_timezone_name(zone).is_err() {
        return false;
    }
    if zone == "UTC" {
        return true;
    }
    let mut magic = [0u8; 4];
    std::fs::File::open(zoneinfo_dir.join(zone))
        .and_then(|mut file| file.read_exact(&mut magic))
        .is_ok_and(|()| &magic == b"TZif")
}

/// The zones a console offers: `UTC` first, then the canonical zones of
/// `zone1970.tab` (or `zone.tab` when the tzdata has no `zone1970.tab`), by
/// name. Any other installed name, such as a backward-compatible alias, is
/// still a zone [`zone_installed`] accepts; it is not offered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneCatalog {
    /// The tzdata release, from `tzdata.zi`; `None` when it does not say.
    pub tzdata_version: Option<String>,
    pub zones: Vec<Zone>,
}

/// One zone: its IANA name, split into the region and the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Zone {
    pub id: String,
    /// `Europe` of `Europe/Berlin`; `None` for `UTC`.
    pub region: Option<String>,
    /// `Berlin` of `Europe/Berlin`, `Argentina/Buenos_Aires` of
    /// `America/Argentina/Buenos_Aires`; `None` for `UTC`.
    pub city: Option<String>,
}

/// Read the catalog under `zoneinfo_dir`. An error is a tzdata with neither
/// table.
pub fn zone_catalog(zoneinfo_dir: &Path) -> std::io::Result<ZoneCatalog> {
    let table = std::fs::read_to_string(zoneinfo_dir.join("zone1970.tab"))
        .or_else(|_| std::fs::read_to_string(zoneinfo_dir.join("zone.tab")))?;
    let mut ids: Vec<&str> = table
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split('\t').nth(2))
        .filter(|id| *id != "UTC" && zone_installed(zoneinfo_dir, id))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let zones = std::iter::once("UTC")
        .chain(ids)
        .map(|id| {
            let (region, city) = match id.split_once('/') {
                Some((region, city)) => (Some(region.to_string()), Some(city.to_string())),
                None => (None, None),
            };
            Zone {
                id: id.to_string(),
                region,
                city,
            }
        })
        .collect();
    let tzdata_version = std::fs::read_to_string(zoneinfo_dir.join("tzdata.zi"))
        .ok()
        .and_then(|text| {
            text.lines()
                .next()
                .and_then(|line| line.strip_prefix("# version "))
                .map(|version| version.trim().to_string())
        });
    Ok(ZoneCatalog {
        tzdata_version,
        zones,
    })
}

#[cfg(test)]
mod tests;

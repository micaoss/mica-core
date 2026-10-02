//! What the product carries: the `FEATURES` of `/usr/lib/mica/product.conf`.
//!
//! The file is in the dm-verity root and written when the root is composed, so
//! it is the product's own, authenticated statement. micad runs, and apid
//! serves, only the features it names; everything else is always present.
//!
//! A root with no `product.conf`, or one whose file has no `FEATURES` line, is a
//! development host, and every feature is on.
//!
//! The same file states the product's init (`INIT=systemd|openrc`, systemd when
//! absent). Every feature is served on either init: containers run under
//! mica-podman's own supervisor, mica-containerd, not under the init.

use std::collections::BTreeSet;
use std::path::Path;

/// Where the composed root states the product.
pub const PRODUCT_FILE: &str = "/usr/lib/mica/product.conf";

/// The init system a product runs, from its `INIT=` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Init {
    /// systemd, and the default when the line is absent.
    #[default]
    Systemd,
    /// OpenRC.
    Openrc,
}

impl Init {
    /// The word `INIT` spells it with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Systemd => "systemd",
            Self::Openrc => "openrc",
        }
    }

    /// The init a `product.conf` states; systemd when it states none.
    ///
    /// # Errors
    ///
    /// An `INIT=` line naming an init this build does not drive.
    pub fn try_parse(product_conf: &str) -> Result<Self, String> {
        let Some(value) = line_value(product_conf, "INIT=") else {
            return Ok(Self::Systemd);
        };
        match value {
            "systemd" => Ok(Self::Systemd),
            "openrc" => Ok(Self::Openrc),
            other => Err(format!("INIT={other} names no init this build drives")),
        }
    }

    /// [`Self::try_parse`], with an unknown init read as systemd.
    #[must_use]
    pub fn parse(product_conf: &str) -> Self {
        Self::try_parse(product_conf).unwrap_or_default()
    }

    /// The init of the file at `path`; systemd when there is no file.
    ///
    /// # Errors
    ///
    /// A file that exists and cannot be read, or that names an unknown init.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::try_parse(&text)
                .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidData, message)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::Systemd),
            Err(err) => Err(err),
        }
    }
}

/// The value of the first `<key>` line, unquoted.
fn line_value<'a>(product_conf: &'a str, key: &str) -> Option<&'a str> {
    let value = product_conf
        .lines()
        .find_map(|line| line.trim().strip_prefix(key))?
        .trim();
    Some(
        value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .unwrap_or(value),
    )
}

/// A part of the management surface a product may leave out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    Wifi,
    Bluetooth,
    Ssh,
    Containers,
    Mqtt,
}

impl Feature {
    /// Every feature, in the order they are reported.
    pub const ALL: [Self; 5] = [
        Self::Wifi,
        Self::Bluetooth,
        Self::Ssh,
        Self::Containers,
        Self::Mqtt,
    ];

    /// The word `FEATURES` spells it with.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Bluetooth => "bluetooth",
            Self::Ssh => "ssh",
            Self::Containers => "containers",
            Self::Mqtt => "mqtt",
        }
    }

    fn from_word(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|feature| feature.as_str() == word)
    }

    /// The settings subtree the feature owns.
    #[must_use]
    pub const fn subtree(self) -> &'static str {
        match self {
            Self::Wifi => "wifi",
            Self::Bluetooth => "bluetooth",
            Self::Ssh => "access.ssh",
            Self::Containers => "container",
            Self::Mqtt => "mqtt",
        }
    }

    /// The feature owning the settings dot-path `path`, if any: the path is
    /// the feature's subtree or lies inside it.
    #[must_use]
    pub fn owning(path: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|feature| {
            let root = feature.subtree();
            path == root
                || path
                    .strip_prefix(root)
                    .is_some_and(|rest| rest.starts_with('.'))
        })
    }
}

impl std::fmt::Display for Feature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The features a device serves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Features(BTreeSet<Feature>);

impl Default for Features {
    fn default() -> Self {
        Self::all()
    }
}

impl Features {
    /// Every feature: a development host.
    #[must_use]
    pub fn all() -> Self {
        Self(Feature::ALL.into_iter().collect())
    }

    /// Exactly `features`.
    #[must_use]
    pub fn only(features: &[Feature]) -> Self {
        Self(features.iter().copied().collect())
    }

    /// The features of a `product.conf`: the words of its `FEATURES=` line,
    /// quoted or not, that name a [`Feature`]. Other words are ignored. With no
    /// `FEATURES` line, every feature, on either init.
    #[must_use]
    pub fn parse(product_conf: &str) -> Self {
        match line_value(product_conf, "FEATURES=") {
            Some(value) => Self(
                value
                    .split_whitespace()
                    .filter_map(Feature::from_word)
                    .collect(),
            ),
            None => Self::all(),
        }
    }

    /// The features of the file at `path`; every feature when there is no
    /// file.
    ///
    /// # Errors
    ///
    /// A file that exists and cannot be read: the product's statement is
    /// there and unreadable, which is not the same as a development host.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Ok(Self::parse(&text)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::all()),
            Err(err) => Err(err),
        }
    }

    /// Whether the device serves `feature`.
    #[must_use]
    pub fn has(&self, feature: Feature) -> bool {
        self.0.contains(&feature)
    }

    /// The feature owning `path` when the device does not serve it.
    #[must_use]
    pub fn refuses(&self, path: &str) -> Option<Feature> {
        Feature::owning(path).filter(|feature| !self.has(*feature))
    }

    /// The words of the features served, in report order.
    #[must_use]
    pub fn words(&self) -> Vec<&'static str> {
        Feature::ALL
            .into_iter()
            .filter(|feature| self.has(*feature))
            .map(Feature::as_str)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_features_line_names_what_is_served() {
        let features = Features::parse(
            "PRODUCT=cx3576-dev\nBOARD=cx3576\nFEATURES=\"micad mqtt containers wifi\"\n",
        );
        assert_eq!(features.words(), ["wifi", "containers", "mqtt"]);
        assert!(!features.has(Feature::Bluetooth));
        assert!(!features.has(Feature::Ssh));
        // Unquoted is read the same way; unknown words are ignored.
        assert_eq!(
            Features::parse("FEATURES=ssh display bluetooth\n").words(),
            ["bluetooth", "ssh"]
        );
        // An empty line is a product that carries none of them.
        assert!(Features::parse("FEATURES=\"\"\n").words().is_empty());
    }

    #[test]
    fn no_features_line_or_no_file_is_a_development_host() {
        assert_eq!(Features::parse("PRODUCT=x\n"), Features::all());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Features::load(&dir.path().join("product.conf")).unwrap(),
            Features::all()
        );
        std::fs::write(dir.path().join("product.conf"), "FEATURES=\"mqtt\"\n").unwrap();
        assert_eq!(
            Features::load(&dir.path().join("product.conf"))
                .unwrap()
                .words(),
            ["mqtt"]
        );
    }

    #[test]
    fn the_init_line_names_the_init_and_absent_means_systemd() {
        assert_eq!(Init::parse("PRODUCT=x\n"), Init::Systemd);
        assert_eq!(Init::parse("INIT=systemd\n"), Init::Systemd);
        assert_eq!(Init::parse("INIT=openrc\n"), Init::Openrc);
        assert_eq!(Init::parse("INIT=\"openrc\"\n"), Init::Openrc);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("product.conf");
        assert_eq!(Init::load(&path).unwrap(), Init::Systemd);
        std::fs::write(&path, "INIT=openrc\n").unwrap();
        assert_eq!(Init::load(&path).unwrap(), Init::Openrc);
    }

    #[test]
    fn an_unknown_init_is_refused() {
        assert!(Init::try_parse("INIT=runit\n").is_err());
    }

    /// Containers run under mica-containerd on either init, so an OpenRC
    /// product carries them when its FEATURES line names them.
    #[test]
    fn an_openrc_product_carries_containers_like_any_other() {
        let features =
            Features::parse("INIT=openrc\nFEATURES=\"ssh wifi containers mqtt bluetooth\"\n");
        assert_eq!(
            features.words(),
            ["wifi", "bluetooth", "ssh", "containers", "mqtt"]
        );
        assert_eq!(
            Features::parse("INIT=openrc\n").words(),
            Features::all().words()
        );
    }

    #[test]
    fn a_path_belongs_to_the_feature_whose_subtree_holds_it() {
        assert_eq!(Feature::owning("wifi"), Some(Feature::Wifi));
        assert_eq!(Feature::owning("wifi.client.networks"), Some(Feature::Wifi));
        assert_eq!(
            Feature::owning("access.ssh.authorizedKeys"),
            Some(Feature::Ssh)
        );
        assert_eq!(Feature::owning("access.webAdmin"), None);
        assert_eq!(
            Feature::owning("container.units"),
            Some(Feature::Containers)
        );
        assert_eq!(Feature::owning("mqtt"), Some(Feature::Mqtt));
        // A prefix that is not a segment boundary is not the subtree.
        assert_eq!(Feature::owning("wifiextra"), None);
        assert_eq!(Feature::owning("hostname"), None);
        assert_eq!(Feature::owning(""), None);

        let features = Features::only(&[Feature::Mqtt]);
        assert_eq!(features.refuses("wifi.ap"), Some(Feature::Wifi));
        assert_eq!(features.refuses("mqtt.enabled"), None);
        assert_eq!(features.refuses("hostname"), None);
    }
}

//! What an update follows when the command line does not say: the device's
//! own configuration, resolved as micad resolves it, so `mica-deploy update`
//! run by hand on a device without micad looks where micad would.
//!
//! The operator's `/mica/config/updates.json` over the image's
//! `/usr/share/mica/meta/updates/manifest.json`, per key: the source
//! (`source.url` over `update.source`; absent and `null` both take the
//! image's), the channel of each kind of set (`channels` over
//! `update.channels`, else `general`) and the download budget
//! (`source.maxBytes`, else [`DEFAULT_MAX_BYTES`]).

use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// The operator's update configuration, on DATA.
pub const UPDATES_PATH: &str = "/mica/config/updates.json";
/// The image's update defaults.
pub const MANIFEST_PATH: &str = "/usr/share/mica/meta/updates/manifest.json";
/// The download budget when the operator names none: micad's.
pub const DEFAULT_MAX_BYTES: u64 = 500_000_000;
/// The channel a kind of set follows when nothing names one.
pub const GENERAL_CHANNEL: &str = "general";
/// Bound on either document.
const MAX_DOCUMENT_BYTES: u64 = 65536;

#[derive(Debug, PartialEq, Eq)]
pub struct Defaults {
    /// `None`: no online source anywhere.
    pub source: Option<String>,
    pub max_bytes: u64,
    channels: BTreeMap<String, String>,
}

impl Defaults {
    /// The device's defaults. A missing document contributes nothing; one
    /// that does not read is refused, as micad refuses it, rather than
    /// guessed past.
    pub fn read() -> Result<Self> {
        Self::parse(
            document(Path::new(MANIFEST_PATH))?.as_ref(),
            document(Path::new(UPDATES_PATH))?.as_ref(),
        )
    }

    pub fn parse(manifest: Option<&Value>, updates: Option<&Value>) -> Result<Self> {
        let baked = manifest.and_then(|m| m.get("update"));
        let operator_source = updates.and_then(|u| u.get("source"));
        let source = match operator_source.and_then(|s| s.get("url")) {
            Some(Value::String(url)) => Some(url.clone()),
            Some(Value::Null) | None => match baked.and_then(|b| b.get("source")) {
                Some(Value::String(url)) => Some(url.clone()),
                Some(Value::Null) | None => None,
                Some(_) => anyhow::bail!("{MANIFEST_PATH}: update.source is not a string"),
            },
            Some(_) => anyhow::bail!("{UPDATES_PATH}: source.url is not a string"),
        };
        let max_bytes = match operator_source.and_then(|s| s.get("maxBytes")) {
            Some(value) => value
                .as_u64()
                .with_context(|| format!("{UPDATES_PATH}: source.maxBytes is not a byte count"))?,
            None => DEFAULT_MAX_BYTES,
        };
        let mut channels = BTreeMap::new();
        for (path, named) in [
            (MANIFEST_PATH, baked.and_then(|b| b.get("channels"))),
            (UPDATES_PATH, updates.and_then(|u| u.get("channels"))),
        ] {
            let Some(named) = named else { continue };
            let named = named
                .as_object()
                .with_context(|| format!("{path}: channels is not an object"))?;
            for (kind, channel) in named {
                let channel = channel
                    .as_str()
                    .with_context(|| format!("{path}: the channel of {kind} is not a string"))?;
                channels.insert(kind.clone(), channel.to_owned());
            }
        }
        Ok(Self {
            source,
            max_bytes,
            channels,
        })
    }

    /// The channel followed for `kind`.
    #[must_use]
    pub fn channel(&self, kind: &str) -> &str {
        self.channels
            .get(kind)
            .map_or(GENERAL_CHANNEL, String::as_str)
    }
}

fn document(path: &Path) -> Result<Option<Value>> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        Ok(meta) => ensure!(meta.is_file(), "{} is not a regular file", path.display()),
    }
    let bytes = crate::deployments::read_bounded(path, MAX_DOCUMENT_BYTES)
        .with_context(|| format!("read {}", path.display()))?;
    Ok(Some(
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nothing_configured_is_no_source_the_general_channel_and_micad_s_budget() {
        let defaults = Defaults::parse(None, None).unwrap();
        assert_eq!(defaults.source, None);
        assert_eq!(defaults.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(defaults.channel("core"), "general");
    }

    #[test]
    fn the_operator_s_keys_win_over_the_image_s_one_by_one() {
        let manifest =
            json!({"update": {"source": "https://img/", "channels": {"core": "lts", "x": "a"}}});
        let image = Defaults::parse(Some(&manifest), None).unwrap();
        assert_eq!(image.source.as_deref(), Some("https://img/"));
        assert_eq!(image.channel("core"), "lts");

        let updates =
            json!({"source": {"url": "https://op/", "maxBytes": 7}, "channels": {"core": "beta"}});
        let both = Defaults::parse(Some(&manifest), Some(&updates)).unwrap();
        assert_eq!(both.source.as_deref(), Some("https://op/"));
        assert_eq!(both.max_bytes, 7);
        assert_eq!(both.channel("core"), "beta");
        assert_eq!(both.channel("x"), "a");
    }

    #[test]
    fn a_null_operator_source_takes_the_image_s() {
        let manifest = json!({"update": {"source": "https://img/"}});
        let updates = json!({"source": {"url": null}});
        let defaults = Defaults::parse(Some(&manifest), Some(&updates)).unwrap();
        assert_eq!(defaults.source.as_deref(), Some("https://img/"));
        let none = json!({"update": {"source": null}});
        assert_eq!(
            Defaults::parse(Some(&none), Some(&updates)).unwrap().source,
            None
        );
    }

    #[test]
    fn a_malformed_key_is_refused() {
        for (manifest, updates) in [
            (json!({"update": {"source": 1}}), json!({})),
            (json!({}), json!({"source": {"url": 1}})),
            (json!({}), json!({"source": {"maxBytes": "big"}})),
            (json!({}), json!({"channels": ["core"]})),
            (json!({}), json!({"channels": {"core": 1}})),
        ] {
            assert!(Defaults::parse(Some(&manifest), Some(&updates)).is_err());
        }
    }

    #[test]
    fn a_missing_document_contributes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(document(&dir.path().join("absent.json")).unwrap(), None);
        let path = dir.path().join("updates.json");
        std::fs::write(&path, "{").unwrap();
        assert!(document(&path).is_err());
    }
}

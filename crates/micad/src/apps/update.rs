//! An app's update check: the version file its manifest's `update.url`
//! names, `{"name","version","bundles":{"<arch>":{"url","sha256","size"}}}`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use micad_settings::ServicesSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::bundle::{self, MAX_ARCHIVE_BYTES};
use super::flow::InstallOptions;
use super::{AppEntry, AppError, Apps, now};

/// The largest version file read.
const VERSION_FILE_BYTES: u64 = 64 * 1024;
/// How often every app is checked.
pub const CHECK_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Fetches a URL to a file.
pub trait Fetch: Send + Sync {
    /// Fetch `url` to `path`, at most `limit` bytes.
    ///
    /// # Errors
    ///
    /// The transfer failed or was larger.
    fn fetch(&self, url: &str, path: &Path, limit: u64) -> anyhow::Result<()>;
}

/// Over HTTPS, as the deployment client fetches.
pub struct HttpFetch;

impl Fetch for HttpFetch {
    fn fetch(&self, url: &str, path: &Path, limit: u64) -> anyhow::Result<()> {
        mica_deploy::acquisition::download_to(url, path, limit)
    }
}

/// A newer version the version file offers this device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Deserialize)]
struct VersionFile {
    name: String,
    version: String,
    bundles: BTreeMap<String, BundleRef>,
}

#[derive(Deserialize)]
struct BundleRef {
    url: String,
    sha256: String,
    size: u64,
}

/// Whether `version` is newer than what `entry` runs and has not failed.
pub(super) fn is_available(entry: &AppEntry, version: &str) -> bool {
    mica_deploy::debian_version::compare(version, &entry.current.version) == Some(Ordering::Greater)
        && entry.update.failed.as_deref() != Some(version)
}

impl Apps {
    async fn fetch_to(&self, url: &str, path: &Path, limit: u64) -> anyhow::Result<()> {
        let fetch = Arc::clone(&self.fetch);
        let (url, path) = (url.to_string(), path.to_path_buf());
        tokio::task::spawn_blocking(move || fetch.fetch(&url, &path, limit))
            .await
            .context("fetch")?
    }

    /// Read `name`'s version file now and record what it offers.
    ///
    /// # Errors
    ///
    /// No such app, no update URL, or the version file could not be read.
    pub async fn check_update(&self, name: &str) -> Result<Value, AppError> {
        let entry = self
            .read_record()
            .apps
            .remove(name)
            .ok_or_else(|| AppError::NotFound(format!("no app named {name:?} is installed")))?;
        let manifest = self.manifest_of(name, &entry.current.version)?;
        let spec = manifest
            .update
            .ok_or_else(|| AppError::Refused(format!("{name} names no update URL")))?;
        let checked = self.read_offer(name, &spec.url).await;
        let mut record = self.read_record();
        let Some(entry) = record.apps.get_mut(name) else {
            return Err(AppError::NotFound(format!(
                "no app named {name:?} is installed"
            )));
        };
        entry.update.checked_at = Some(now());
        match checked {
            Ok(offer) => {
                entry.update.latest = Some(offer.clone());
                entry.update.error = None;
                let available = is_available(entry, &offer.version);
                let auto = spec.auto;
                self.write_record(&record)?;
                Ok(
                    json!({ "name": name, "latest": offer.version, "available": available, "auto": auto }),
                )
            }
            Err(err) => {
                entry.update.error = Some(format!("{err:#}"));
                self.write_record(&record)?;
                Err(AppError::Failed(err))
            }
        }
    }

    async fn read_offer(&self, name: &str, url: &str) -> anyhow::Result<Offer> {
        let incoming = self.incoming().map_err(|err| anyhow::anyhow!("{err}"))?;
        std::fs::create_dir_all(&incoming)?;
        let path = incoming.join(format!(".{name}.version.json"));
        let fetched = self.fetch_to(url, &path, VERSION_FILE_BYTES).await;
        let text = fetched.and_then(|()| Ok(std::fs::read(&path)?));
        let _ = std::fs::remove_file(&path);
        let file: VersionFile =
            serde_json::from_slice(&text?).context("the version file does not parse")?;
        if file.name != name {
            bail!("the version file is app {}'s", file.name);
        }
        if !super::manifest::is_version(&file.version) {
            bail!(
                "the version file's version {:?} is not a version",
                file.version
            );
        }
        let bundle = file
            .bundles
            .get(&self.arch)
            .or_else(|| file.bundles.get("all"))
            .with_context(|| format!("the version file has no bundle for {}", self.arch))?;
        let resolved = url::Url::parse(url)
            .and_then(|base| base.join(&bundle.url))
            .context("the bundle URL")?;
        if resolved.scheme() != "https" {
            bail!("the bundle URL is not https");
        }
        if bundle.sha256.len() != 64 || !bundle.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("the bundle's sha256 is not 64 hex digits");
        }
        if bundle.size > MAX_ARCHIVE_BYTES {
            bail!("the bundle is above {} MiB", MAX_ARCHIVE_BYTES >> 20);
        }
        Ok(Offer {
            version: file.version,
            url: resolved.to_string(),
            sha256: bundle.sha256.to_ascii_lowercase(),
            size: bundle.size,
        })
    }

    /// Check `name`, then fetch and install the newer version it offers.
    ///
    /// # Errors
    ///
    /// The check failed, nothing newer is offered, the bundle did not match
    /// its sha256, or its install failed (and was rolled back).
    pub async fn update(&self, name: &str, services: &ServicesSettings) -> Result<Value, AppError> {
        self.fetch_update(name, services, false).await
    }

    /// [`Apps::update`]; when `unattended`, a version that asks for more
    /// privileges is refused, and left to an update by hand.
    pub(super) async fn fetch_update(
        &self,
        name: &str,
        services: &ServicesSettings,
        unattended: bool,
    ) -> Result<Value, AppError> {
        self.check_update(name).await?;
        let record = self.read_record();
        let entry = record
            .apps
            .get(name)
            .ok_or_else(|| AppError::NotFound(format!("no app named {name:?} is installed")))?;
        let offer = entry
            .update
            .latest
            .clone()
            .filter(|offer| is_available(entry, &offer.version))
            .ok_or_else(|| AppError::Refused(format!("no newer version of {name} is offered")))?;
        let path = self
            .incoming()?
            .join(format!(".{name}-{}.tar.gz", offer.version));
        let fetched = self.fetch_to(&offer.url, &path, offer.size).await;
        let checked = fetched.and_then(|()| {
            let size = std::fs::metadata(&path)?.len();
            let sha256 = bundle::sha256_file(&path)?;
            if size != offer.size || sha256 != offer.sha256 {
                bail!("the bundle fetched is not the one the version file names");
            }
            Ok(())
        });
        let installed = match checked {
            Ok(()) => {
                self.install(
                    &path,
                    "url",
                    InstallOptions {
                        upgrade: true,
                        unattended,
                        ..Default::default()
                    },
                    services,
                )
                .await
            }
            Err(err) => Err(AppError::Failed(err)),
        };
        let _ = std::fs::remove_file(&path);
        if installed.is_err() {
            let mut record = self.read_record();
            if let Some(entry) = record.apps.get_mut(name) {
                entry.update.failed = Some(offer.version.clone());
            }
            self.write_record(&record)?;
        }
        installed
    }

    /// Every app with an update URL checked; those that say `auto` updated,
    /// unless the newer version asks for more privileges.
    pub async fn check_all(self: &Arc<Self>, services: &ServicesSettings) {
        for (name, entry) in self.read_record().apps {
            let Ok(manifest) = self.manifest_of(&name, &entry.current.version) else {
                continue;
            };
            let Some(spec) = manifest.update else {
                continue;
            };
            let Ok(_busy) = self.begin() else {
                continue;
            };
            let done = if spec.auto {
                match self.check_update(&name).await {
                    Ok(checked) if checked["available"] == json!(true) => {
                        self.fetch_update(&name, services, true).await
                    }
                    other => other,
                }
            } else {
                self.check_update(&name).await
            };
            if let Err(err) = done {
                tracing::warn!(app = name, %err, "app update check");
            }
        }
    }
}

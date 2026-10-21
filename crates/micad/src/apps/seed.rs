//! The image's seeds: `/usr/share/mica/apps/<name>.tar.gz`, taken every boot
//! by one exact rule, recorded on STATE.
//!
//! Per seed S of app N, with I the installed N:
//! - no record: S is installed, or recorded `skipped` when I is the user's;
//! - a record of S's sha256: nothing, so what the user did with N stays;
//! - a record of another sha256: under `once`, nothing; under `follow-image`,
//!   S is installed only while I is exactly the recorded seed and S is newer;
//! - a seed that fails is recorded `failed` and tried again at one more boot.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use micad_settings::ServicesSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::bundle;
use super::flow::InstallOptions;
use super::manifest::SeedPolicy;
use super::{AppError, Apps};

/// Where the image keeps its seeds.
const SEEDS_DIR: &str = "usr/share/mica/apps";
/// The record of the seeds taken, on STATE.
const SEED_RECORD: &str = "var/lib/mica/seeded-apps.json";
/// The boots a failing seed is tried at.
const SEED_TRIES: u32 = 2;

/// The seed of one app the record holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Seeded {
    pub version: String,
    pub sha256: String,
    /// `installed`, `upgraded`, `skipped` or `failed`.
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A later seed that failed, by sha256, and the boots it was tried at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_sha256: Option<String>,
    #[serde(default)]
    pub tries: u32,
}

impl Apps {
    fn seed_record_path(&self) -> Result<PathBuf, AppError> {
        Ok(self.root()?.join(SEED_RECORD))
    }

    pub(super) fn read_seed_record(&self) -> BTreeMap<String, Seeded> {
        let Ok(path) = self.seed_record_path() else {
            return BTreeMap::new();
        };
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    fn write_seed_record(&self, record: &BTreeMap<String, Seeded>) -> Result<(), AppError> {
        let text = serde_json::to_string_pretty(record).map_err(anyhow::Error::from)? + "\n";
        crate::fswrite::write_config_if_changed(&self.seed_record_path()?, &text, 0o600)?;
        Ok(())
    }

    /// Take every seed of the image by the rule; what each did, by name.
    pub async fn seed(self: &Arc<Self>, services: &ServicesSettings) -> Value {
        let Ok(root) = self.root() else {
            return json!({});
        };
        let mut seeds: Vec<PathBuf> = std::fs::read_dir(root.join(SEEDS_DIR))
            .map(|dir| {
                dir.flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.to_string_lossy().ends_with(".tar.gz"))
                    .collect()
            })
            .unwrap_or_default();
        seeds.sort();
        let mut done = serde_json::Map::new();
        for seed in seeds {
            let busy = loop {
                match self.begin() {
                    Ok(busy) => break busy,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
                }
            };
            let name = seed
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(".tar.gz"))
                .unwrap_or_default()
                .to_string();
            let did = self.seed_one(&name, &seed, services).await;
            drop(busy);
            match &did {
                Ok(what) => tracing::info!(app = name, what, "seed"),
                Err(err) => tracing::warn!(app = name, %err, "seed failed"),
            }
            done.insert(
                name,
                match did {
                    Ok(what) => json!(what),
                    Err(err) => json!({ "error": err.to_string() }),
                },
            );
        }
        Value::Object(done)
    }

    /// Take one seed; what it did.
    async fn seed_one(
        &self,
        name: &str,
        seed: &Path,
        services: &ServicesSettings,
    ) -> Result<&'static str, AppError> {
        let sha256 = bundle::sha256_file(seed).map_err(anyhow::Error::from)?;
        let mut record = self.read_seed_record();
        let installed = self.read_record().apps.get(name).map(|e| e.current.clone());
        let manifest = bundle::peek_manifest(seed);
        let version = manifest
            .as_ref()
            .map(|m| m.version.clone())
            .unwrap_or_default();
        let upgrade = match record.get(name) {
            None => {
                if installed.is_some() {
                    record.insert(name.into(), seeded(&version, &sha256, "skipped"));
                    self.write_seed_record(&record)?;
                    return Ok("skipped");
                }
                false
            }
            Some(had) if had.sha256 == sha256 => {
                if had.result != "failed" || had.tries >= SEED_TRIES {
                    return Ok("unchanged");
                }
                if installed.is_some() {
                    return Ok("unchanged");
                }
                false
            }
            // A seed that never took: the image's new one is tried.
            Some(had) if had.result == "failed" && installed.is_none() => false,
            Some(had) => {
                if had.failed_sha256.as_deref() == Some(sha256.as_str()) && had.tries >= SEED_TRIES
                {
                    return Ok("unchanged");
                }
                let Some(installed) = &installed else {
                    return Ok("not installed");
                };
                let policy = self
                    .manifest_of(name, &installed.version)
                    .map(|m| m.seed_policy)
                    .unwrap_or_else(|_| {
                        manifest.as_ref().map(|m| m.seed_policy).unwrap_or_default()
                    });
                if policy == SeedPolicy::Once {
                    return Ok("once");
                }
                if installed.version != had.version || installed.sha256 != had.sha256 {
                    return Ok("changed by the user");
                }
                let newer = mica_deploy::debian_version::compare(&version, &installed.version)
                    == Some(std::cmp::Ordering::Greater);
                if !newer {
                    return Ok("not newer");
                }
                true
            }
        };
        let outcome = match &manifest {
            Ok(manifest) if manifest.name != name => Err(AppError::Refused(format!(
                "the seed {name}.tar.gz holds app {}",
                manifest.name
            ))),
            Ok(_) => self
                .install(
                    seed,
                    "seed",
                    InstallOptions {
                        upgrade,
                        ..InstallOptions::default()
                    },
                    services,
                )
                .await
                .map(drop),
            Err(err) => Err(AppError::Refused(err.to_string())),
        };
        match outcome {
            Ok(()) => {
                let result = if upgrade { "upgraded" } else { "installed" };
                record.insert(name.into(), seeded(&version, &sha256, result));
                self.write_seed_record(&record)?;
                Ok(result)
            }
            Err(err) => {
                let entry = match record.get(name).cloned() {
                    Some(mut had) if upgrade => {
                        if had.failed_sha256.as_deref() != Some(sha256.as_str()) {
                            had.tries = 0;
                        }
                        had.failed_sha256 = Some(sha256.clone());
                        had.tries += 1;
                        had.error = Some(err.to_string());
                        had
                    }
                    had => {
                        let tries = had
                            .filter(|had| had.sha256 == sha256)
                            .map_or(0, |had| had.tries);
                        Seeded {
                            error: Some(err.to_string()),
                            tries: tries + 1,
                            ..seeded(&version, &sha256, "failed")
                        }
                    }
                };
                record.insert(name.into(), entry);
                self.write_seed_record(&record)?;
                Err(err)
            }
        }
    }
}

fn seeded(version: &str, sha256: &str, result: &str) -> Seeded {
    Seeded {
        version: version.to_string(),
        sha256: sha256.to_string(),
        result: result.to_string(),
        error: None,
        failed_sha256: None,
        tries: 0,
    }
}

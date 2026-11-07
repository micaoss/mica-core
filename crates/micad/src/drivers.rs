//! Owner certificates and driver extensions, micad's half: mica-deploy
//! checks and changes them (`docs/mica-core.md` 6.3 and 6.5) and every
//! change applies from the next boot; micad asks for the administrator
//! password, runs each change as a task of kind `drivers.apply` and reads
//! where they stand.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use serde_json::Value;

/// The longest a read runs: apid's call to micad is bounded at 5 s.
const READ_TIMEOUT: Duration = Duration::from_secs(4);
/// The longest a change runs: a build's image is copied onto SYSTEM.
const CHANGE_TIMEOUT: Duration = Duration::from_secs(300);
/// The longest output read back.
const MAX_OUTPUT: usize = 256 * 1024;

/// mica-deploy, run for the owner certificates and the driver extensions.
#[derive(Debug)]
pub struct Drivers {
    deploy: PathBuf,
}

impl Drivers {
    #[must_use]
    pub fn new(deploy: PathBuf) -> Self {
        Self { deploy }
    }

    /// The device's: `/usr/bin/mica-deploy`.
    #[must_use]
    pub fn production() -> Self {
        Self::new(PathBuf::from("/usr/bin/mica-deploy"))
    }

    /// Run mica-deploy with `args` within `timeout`; it must succeed and
    /// answer JSON.
    async fn run(&self, args: &[&str], timeout: Duration) -> Result<Value> {
        let output = tokio::time::timeout(
            timeout,
            tokio::process::Command::new(&self.deploy)
                .args(args)
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .with_context(|| format!("mica-deploy {} timed out", args[0]))?
        .with_context(|| format!("run {}", self.deploy.display()))?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        ensure!(
            output.status.success(),
            "{}",
            stderr
                .lines()
                .last()
                .unwrap_or("mica-deploy failed")
                .trim()
                .trim_start_matches("Error: ")
        );
        ensure!(
            output.stdout.len() <= MAX_OUTPUT,
            "mica-deploy {} answered too much",
            args[0]
        );
        serde_json::from_slice(&output.stdout)
            .with_context(|| format!("mica-deploy {} answered no JSON", args[0]))
    }

    /// The enrolled certificates, each as it stands against this boot.
    ///
    /// # Errors
    ///
    /// mica-deploy fails.
    pub async fn owner_certs(&self) -> Result<Value> {
        self.run(&["owner-certs"], READ_TIMEOUT).await
    }

    /// The driver extensions: `status.extensions`, and the running kernel.
    ///
    /// # Errors
    ///
    /// mica-deploy fails.
    pub async fn extensions(&self) -> Result<Value> {
        let status = self.run(&["status"], READ_TIMEOUT).await?;
        let mut extensions = status["extensions"].clone();
        if let Some(extensions) = extensions.as_object_mut() {
            extensions.insert("kernel".into(), status["boot"]["kernelId"].clone());
        }
        Ok(extensions)
    }

    /// Enrol the certificate `bytes`, held in `dir` meanwhile, as asked `by`
    /// from `source`; `clock_synced` holds its validity period to the clock.
    ///
    /// # Errors
    ///
    /// The certificate cannot be held, or mica-deploy refuses it.
    pub async fn add_owner_cert(
        &self,
        dir: &Path,
        bytes: &[u8],
        clock_synced: bool,
        by: &str,
        source: &str,
    ) -> Result<Value> {
        let held = dir.join(format!(
            "{:032x}.cert",
            u128::from_ne_bytes(micad_settings::random_bytes())
        ));
        tokio::fs::write(&held, bytes)
            .await
            .with_context(|| format!("write {}", held.display()))?;
        let path = held.to_string_lossy();
        let mut args = vec!["owner-cert-add", &*path, "--by", by, "--source", source];
        if clock_synced {
            args.push("--clock-synced");
        }
        let added = self.run(&args, CHANGE_TIMEOUT).await;
        let _ = tokio::fs::remove_file(&held).await;
        added
    }

    /// Remove the enrolled certificate `sha256`.
    ///
    /// # Errors
    ///
    /// mica-deploy fails.
    pub async fn remove_owner_cert(&self, sha256: &str, by: &str, source: &str) -> Result<Value> {
        self.run(
            &["owner-cert-remove", sha256, "--by", by, "--source", source],
            CHANGE_TIMEOUT,
        )
        .await
    }

    /// Take the build in the archive at `archive`, which goes once read.
    ///
    /// # Errors
    ///
    /// mica-deploy refuses it.
    pub async fn import_extension(&self, archive: &Path) -> Result<Value> {
        let path = archive.to_string_lossy();
        let imported = self.run(&["extension-import", &path], CHANGE_TIMEOUT).await;
        let _ = tokio::fs::remove_file(archive).await;
        imported
    }

    /// Install the build of the three files `descriptor`, `image` and
    /// `signature`, which go once read.
    ///
    /// # Errors
    ///
    /// The files cannot be laid out, or mica-deploy refuses the build.
    pub async fn install_extension(
        &self,
        descriptor: &Path,
        image: &Path,
        signature: &Path,
    ) -> Result<Value> {
        let objects = descriptor.with_extension("objects");
        let laid_out = async {
            tokio::fs::create_dir(&objects).await?;
            tokio::fs::rename(image, objects.join("ext.img")).await?;
            tokio::fs::rename(signature, objects.join("ext.roothash.p7s")).await
        }
        .await
        .context("lay out the build");
        let installed = match laid_out {
            Ok(()) => {
                let (path, dir) = (descriptor.to_string_lossy(), objects.to_string_lossy());
                self.run(
                    &["extension-install", &path, "--objects", &dir],
                    CHANGE_TIMEOUT,
                )
                .await
            }
            Err(error) => Err(error),
        };
        for path in [descriptor, image, signature] {
            let _ = tokio::fs::remove_file(path).await;
        }
        let _ = tokio::fs::remove_dir_all(&objects).await;
        installed
    }

    /// Stop installing the extension `name`.
    ///
    /// # Errors
    ///
    /// mica-deploy fails.
    pub async fn remove_extension(&self, name: &str) -> Result<Value> {
        self.run(&["extension-remove", name], CHANGE_TIMEOUT).await
    }

    /// Let the system with the kernel `kernel` run without the extension
    /// `name`.
    ///
    /// # Errors
    ///
    /// mica-deploy fails.
    pub async fn drop_extension(&self, name: &str, kernel: &str) -> Result<Value> {
        self.run(
            &["extension-drop", name, "--kernel", kernel],
            CHANGE_TIMEOUT,
        )
        .await
    }
}

#[cfg(test)]
mod tests;

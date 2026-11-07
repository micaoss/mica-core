//! The owner certificate and driver extension members: each change asks for
//! the administrator password again, is refused while the device is
//! unclaimed, and runs as a task of kind `drivers.apply`, whose transitions
//! are `TaskChanged` signals.

use std::future::Future;
use std::path::{Path, PathBuf};

use serde_json::Value;
use zbus::fdo;
use zbus::object_server::SignalEmitter;

use super::apply::publish_task_transition;
use super::*;
use crate::drivers::Drivers;
use crate::factory_reset::Refusal;

/// The kind of every owner certificate and driver extension task.
const DRIVERS_TASK_KIND: &str = "drivers.apply";
/// The largest certificate taken, PEM or DER.
const MAX_CERTIFICATE_BYTES: usize = 64 * 1024;

/// Who asked and from where, as mica-deploy records it.
fn requested(by: &str, source: &str) -> Result<(), DriversFault> {
    if crate::factory_reset::requester(by).is_none() {
        return Err(fdo::Error::InvalidArgs(format!(
            "`{by}` is not a requester: console, api or cli"
        ))
        .into());
    }
    if source.len() > 256 || source.chars().any(char::is_control) {
        return Err(fdo::Error::InvalidArgs(
            "the source is at most 256 printable characters".into(),
        )
        .into());
    }
    Ok(())
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn extension_name(name: &str) -> Result<(), DriversFault> {
    if mica_deploy::extensions::valid_name(name) {
        Ok(())
    } else {
        Err(fdo::Error::InvalidArgs(format!("`{name}` is not a driver extension name")).into())
    }
}

impl MicadService {
    fn drivers(&self) -> Result<Arc<Drivers>, DriversFault> {
        self.drivers.clone().ok_or_else(|| {
            fdo::Error::Failed(
                "this daemon changes no owner certificates or driver extensions".into(),
            )
            .into()
        })
    }

    /// The enrolled owner certificates, JSON.
    pub(super) async fn owner_certs_json(&self) -> Result<String, DriversFault> {
        let certs = self
            .drivers()?
            .owner_certs()
            .await
            .map_err(|error| fdo::Error::Failed(format!("{error:#}")))?;
        Ok(certs.to_string())
    }

    /// The driver extensions, JSON.
    pub(super) async fn extensions_json(&self) -> Result<String, DriversFault> {
        let extensions = self
            .drivers()?
            .extensions()
            .await
            .map_err(|error| fdo::Error::Failed(format!("{error:#}")))?;
        Ok(extensions.to_string())
    }

    /// Admit a change `what` that `sender` asks for `by` from `source`: a
    /// claimed device, and the administrator's `password`.
    pub(super) async fn admit_drivers_change(
        &self,
        sender: &str,
        by: &str,
        source: &str,
        what: &str,
        password: String,
    ) -> Result<Arc<Drivers>, DriversFault> {
        requested(by, source)?;
        let drivers = self.drivers()?;
        if self.inner.read().await.settings.access.web_admin.is_none() {
            return Err(Refusal::new(
                "not_claimed",
                "the device is not claimed: nothing on it is changed before it is",
            )
            .into());
        }
        self.admin_password(sender, source, what, password).await?;
        tracing::warn!(sender, by, source, what, "driver change admitted");
        Ok(drivers)
    }

    /// Record a task `operation` on `dot_path`, then run `work` as it in the
    /// background; a `warning` in what it answers is the task's message.
    async fn drivers_task<W>(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        operation: &str,
        dot_path: &str,
        work: W,
    ) -> String
    where
        W: Future<Output = anyhow::Result<Value>> + Send + 'static,
    {
        self.apply_queue.remember_emitter(emitter);
        let record = self
            .apply_queue
            .open(operation, DRIVERS_TASK_KIND, dot_path, sender)
            .await;
        publish_task_transition(&self.apply_queue, &self.inner, &record).await;
        let (queue, inner) = (Arc::clone(&self.apply_queue), Arc::clone(&self.inner));
        let (id, operation) = (record.id.clone(), operation.to_owned());
        tokio::spawn(async move {
            if let Some(started) = queue.start(&id).await {
                publish_task_transition(&queue, &inner, &started).await;
            }
            let (outcome, message) = match work.await {
                Ok(answer) => {
                    tracing::warn!(operation, %answer, "driver change done; it applies from the next boot");
                    ("succeeded", answer["warning"].as_str().map(str::to_owned))
                }
                Err(error) => {
                    tracing::warn!(operation, error = %format!("{error:#}"), "driver change failed");
                    ("failed", Some(format!("{error:#}")))
                }
            };
            if let Some(finished) = queue.finish(&id, outcome, message, Vec::new()).await {
                publish_task_transition(&queue, &inner, &finished).await;
            }
        });
        record.id
    }

    /// An upload apid names: a file directly in the update uploads.
    pub(super) fn upload(&self, path: &str) -> Result<PathBuf, DriversFault> {
        let path = Path::new(path);
        let uploads = self.update.uploads_dir();
        if path.parent() != Some(uploads.as_path())
            || !path.symlink_metadata().is_ok_and(|meta| meta.is_file())
        {
            return Err(fdo::Error::InvalidArgs(format!(
                "an upload is a file directly under {}",
                uploads.display()
            ))
            .into());
        }
        Ok(path.to_path_buf())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn add_owner_cert_now(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        certificate: Vec<u8>,
        password: String,
        by: &str,
        source: &str,
    ) -> Result<String, DriversFault> {
        if certificate.is_empty() || certificate.len() > MAX_CERTIFICATE_BYTES {
            return Err(fdo::Error::InvalidArgs(format!(
                "a certificate is 1 to {MAX_CERTIFICATE_BYTES} bytes"
            ))
            .into());
        }
        let drivers = self
            .admit_drivers_change(sender, by, source, "owner certificate add", password)
            .await?;
        let clock_synced = self.clock_trust().await.believed();
        let (dir, by, source) = (self.update.uploads_dir(), by.to_owned(), source.to_owned());
        let work = async move {
            tokio::fs::create_dir_all(&dir).await?;
            drivers
                .add_owner_cert(&dir, &certificate, clock_synced, &by, &source)
                .await
        };
        Ok(self
            .drivers_task(
                emitter,
                sender,
                "owner-cert.add",
                "drivers.ownerCerts",
                work,
            )
            .await)
    }

    pub(super) async fn remove_owner_cert_now(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        sha256: &str,
        password: String,
        by: &str,
        source: &str,
    ) -> Result<String, DriversFault> {
        if !digest(sha256) {
            return Err(fdo::Error::InvalidArgs(format!("`{sha256}` is not a SHA-256")).into());
        }
        let drivers = self
            .admit_drivers_change(sender, by, source, "owner certificate removal", password)
            .await?;
        let (sha, by, source) = (sha256.to_owned(), by.to_owned(), source.to_owned());
        let work = async move { drivers.remove_owner_cert(&sha, &by, &source).await };
        Ok(self
            .drivers_task(
                emitter,
                sender,
                "owner-cert.remove",
                &format!("drivers.ownerCerts.{sha256}"),
                work,
            )
            .await)
    }

    /// `paths` is one archive, or a descriptor, an image and a signature.
    pub(super) async fn install_extension_now(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        paths: &[&str],
        password: String,
        by: &str,
        source: &str,
    ) -> Result<String, DriversFault> {
        let uploads = paths
            .iter()
            .map(|path| self.upload(path))
            .collect::<Result<Vec<_>, _>>()?;
        let drivers = self
            .admit_drivers_change(sender, by, source, "driver extension install", password)
            .await?;
        let work = async move {
            match uploads.as_slice() {
                [archive] => drivers.import_extension(archive).await,
                [descriptor, image, signature] => {
                    drivers
                        .install_extension(descriptor, image, signature)
                        .await
                }
                _ => anyhow::bail!("an archive, or a descriptor, an image and a signature"),
            }
        };
        Ok(self
            .drivers_task(
                emitter,
                sender,
                "extension.install",
                "drivers.extensions",
                work,
            )
            .await)
    }

    pub(super) async fn remove_extension_now(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        name: &str,
        password: String,
        by: &str,
        source: &str,
    ) -> Result<String, DriversFault> {
        extension_name(name)?;
        let drivers = self
            .admit_drivers_change(sender, by, source, "driver extension removal", password)
            .await?;
        let owned = name.to_owned();
        let work = async move { drivers.remove_extension(&owned).await };
        Ok(self
            .drivers_task(
                emitter,
                sender,
                "extension.remove",
                &format!("drivers.extensions.{name}"),
                work,
            )
            .await)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn drop_extension_now(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        name: &str,
        kernel: &str,
        password: String,
        by: &str,
        source: &str,
    ) -> Result<String, DriversFault> {
        extension_name(name)?;
        if !digest(kernel) {
            return Err(fdo::Error::InvalidArgs(format!("`{kernel}` is not a kernel id")).into());
        }
        let drivers = self
            .admit_drivers_change(sender, by, source, "driver extension drop", password)
            .await?;
        let (owned, kernel) = (name.to_owned(), kernel.to_owned());
        let work = async move { drivers.drop_extension(&owned, &kernel).await };
        Ok(self
            .drivers_task(
                emitter,
                sender,
                "extension.drop",
                &format!("drivers.extensions.{name}"),
                work,
            )
            .await)
    }
}

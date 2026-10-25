//! The app members: each change is a task of kind `app.apply`, run beside the
//! settings queue, one app flow at a time.

use std::future::Future;
use std::path::{Path, PathBuf};

use serde_json::Value;
use zbus::object_server::{InterfaceRef, SignalEmitter};

use super::apply::publish_task_transition;
use super::*;
use crate::apps::{AppError, Apps, Busy, InstallOptions};

/// The kind of every app task.
const APP_TASK_KIND: &str = "app.apply";

impl MicadService {
    /// Record a task for `name`, then run `work` as it in the background.
    async fn app_task<W>(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        operation: &str,
        name: &str,
        busy: Busy,
        work: W,
    ) -> String
    where
        W: Future<Output = Result<Value, AppError>> + Send + 'static,
    {
        self.apply_queue.remember_emitter(emitter);
        let record = self
            .apply_queue
            .open(operation, APP_TASK_KIND, &format!("apps.{name}"), sender)
            .await;
        publish_task_transition(&self.apply_queue, &self.inner, &record).await;
        let (queue, inner) = (Arc::clone(&self.apply_queue), Arc::clone(&self.inner));
        let mdns_kick = Arc::clone(&self.mdns_kick);
        let id = record.id.clone();
        tokio::spawn(async move {
            let _busy = busy;
            if let Some(started) = queue.start(&id).await {
                publish_task_transition(&queue, &inner, &started).await;
            }
            let (outcome, message) = match work.await {
                Ok(_) => ("succeeded", None),
                Err(err) => ("failed", Some(err.to_string())),
            };
            mdns_kick.notify_one();
            if let Some(finished) = queue.finish(&id, outcome, message, Vec::new()).await {
                publish_task_transition(&queue, &inner, &finished).await;
            }
        });
        record.id
    }

    async fn services(&self) -> micad_settings::ServicesSettings {
        self.inner.read().await.settings.services.clone()
    }

    /// Take the bundle at `path` and install it as a task: a file directly
    /// in the uploads directory is moved, any other is copied there first.
    /// The manifest is checked before the task is made.
    pub(super) async fn install_app_now(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        path: &str,
        options: InstallOptions,
    ) -> Result<String, AppError> {
        let busy = self.apps.begin()?;
        let incoming = self.apps.incoming()?;
        let given = Path::new(path);
        let (archive, source) = if given.parent() == Some(incoming.as_path()) {
            if !given.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
                return Err(AppError::Refused(format!("{path} is not a file")));
            }
            (given.to_path_buf(), "upload")
        } else {
            (copy_in(given, &incoming).await?, "cli")
        };
        let services = self.services().await;
        let manifest = match self.apps.admit(&archive, options, &services).await {
            Ok(manifest) => manifest,
            Err(err) => {
                let _ = std::fs::remove_file(&archive);
                return Err(err);
            }
        };
        let apps = Arc::clone(&self.apps);
        let work = async move {
            let installed = apps.install(&archive, source, options, &services).await;
            let _ = std::fs::remove_file(&archive);
            installed
        };
        Ok(self
            .app_task(emitter, sender, "app.install", &manifest.name, busy, work)
            .await)
    }

    /// Run `flow` on the app `name` as a task, once it is known installed.
    pub(super) async fn app_flow_now<F, W>(
        &self,
        emitter: &SignalEmitter<'_>,
        sender: &str,
        operation: &str,
        name: &str,
        flow: F,
    ) -> Result<String, AppError>
    where
        F: FnOnce(Arc<Apps>, micad_settings::ServicesSettings) -> W,
        W: Future<Output = Result<Value, AppError>> + Send + 'static,
    {
        let busy = self.apps.begin()?;
        self.apps.view(Some(name)).await?;
        let work = flow(Arc::clone(&self.apps), self.services().await);
        Ok(self
            .app_task(emitter, sender, operation, name, busy, work)
            .await)
    }
}

/// Copy `given` into `incoming` under a name of its own.
async fn copy_in(given: &Path, incoming: &Path) -> Result<PathBuf, AppError> {
    let size = std::fs::metadata(given)
        .map_err(|err| AppError::Refused(format!("read {}: {err}", given.display())))?
        .len();
    if size > crate::apps::MAX_ARCHIVE_BYTES {
        return Err(AppError::Invalid(vec![crate::apps::Problem::new(
            "too_large",
            format!(
                "the bundle is above {} MiB",
                crate::apps::MAX_ARCHIVE_BYTES >> 20
            ),
        )]));
    }
    std::fs::create_dir_all(incoming).map_err(|err| {
        AppError::Failed(anyhow::Error::new(err).context("create the uploads directory"))
    })?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let copy = incoming.join(format!("cli-{}-{nanos:x}.tar.gz", std::process::id()));
    let (from, to) = (given.to_path_buf(), copy.clone());
    tokio::task::spawn_blocking(move || std::fs::copy(from, to))
        .await
        .map_err(|err| AppError::Failed(err.into()))?
        .map_err(|err| AppError::Failed(anyhow::Error::new(err).context("copy the bundle")))?;
    Ok(copy)
}

/// The image's seeds at boot, then every app's update check each day.
pub async fn app_upkeep(service: InterfaceRef<MicadService>) {
    let (apps, inner) = {
        let service = service.get().await;
        (Arc::clone(&service.apps), Arc::clone(&service.inner))
    };
    let services = inner.read().await.settings.services.clone();
    let seeded = apps.seed(&services).await;
    if seeded.as_object().is_some_and(|seeded| !seeded.is_empty()) {
        tracing::info!(%seeded, "app seeds");
    }
    loop {
        tokio::time::sleep(crate::apps::CHECK_EVERY).await;
        let services = inner.read().await.settings.services.clone();
        apps.check_all(&services).await;
    }
}

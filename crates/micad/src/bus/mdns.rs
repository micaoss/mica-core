//! The mDNS responder's upkeep: it follows the runtime drop-ins and the apps
//! between settings writes.

use std::sync::Arc;
use std::time::Duration;

use zbus::object_server::InterfaceRef;

use super::MicadService;
use super::apply::reconcile_subtree;
use crate::reconciler::mdns::entries::RUNTIME_DIR;

/// How often the apps' units are looked at, for one that stopped on its own.
const CHECK_EVERY: Duration = Duration::from_secs(10);
/// The least time between two re-applies: changes in a burst are one.
const COALESCE: Duration = Duration::from_secs(1);

/// Re-apply `access.mdns` when a runtime drop-in changes, when an app task
/// ends, and every [`CHECK_EVERY`].
pub async fn mdns_upkeep(service: InterfaceRef<MicadService>) {
    let (reconcilers, inner, refusals, apply_lock, kick) = {
        let service = service.get().await;
        (
            Arc::clone(&service.reconcilers),
            Arc::clone(&service.inner),
            Arc::clone(&service.refusals),
            Arc::clone(&service.apply_lock),
            Arc::clone(&service.mdns_kick),
        )
    };
    if let Err(err) = watch(RUNTIME_DIR, Arc::clone(&kick)) {
        tracing::warn!(
            dir = RUNTIME_DIR,
            "mdns: drop-ins are read every {}s only: {err:#}",
            CHECK_EVERY.as_secs()
        );
    }
    loop {
        tokio::select! {
            () = kick.notified() => {}
            () = tokio::time::sleep(CHECK_EVERY) => {}
        }
        {
            let _apply = apply_lock.lock().await;
            reconcile_subtree(&reconcilers, &inner, &refusals, "access.mdns").await;
        }
        tokio::time::sleep(COALESCE).await;
    }
}

/// Make `dir`, root's and readable by all, and tell `kick` on each change in
/// it, from a thread of its own.
fn watch(dir: &str, kick: Arc<tokio::sync::Notify>) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    use rustix::fs::inotify;

    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))?;
    let fd = inotify::init(inotify::CreateFlags::CLOEXEC)?;
    inotify::add_watch(
        &fd,
        dir,
        inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::MOVED_FROM
            | inotify::WatchFlags::DELETE
            | inotify::WatchFlags::ATTRIB,
    )?;
    std::thread::Builder::new()
        .name("mdns-watch".to_string())
        .spawn(move || {
            let mut buf = [0u8; 4096];
            // Which file changed does not matter: every change re-reads all.
            loop {
                match rustix::io::read(&fd, &mut buf) {
                    Ok(_) => kick.notify_one(),
                    Err(rustix::io::Errno::INTR) => {}
                    Err(err) => {
                        tracing::warn!(dir = RUNTIME_DIR, "mdns: the watch ended: {err}");
                        break;
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file written, renamed in or removed is told; the directory is made.
    #[tokio::test]
    async fn a_change_in_the_directory_is_told() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("mdns.d");
        let kick = Arc::new(tokio::sync::Notify::new());
        watch(dir.to_str().unwrap(), Arc::clone(&kick)).unwrap();

        let told = || tokio::time::timeout(Duration::from_secs(5), kick.notified());
        std::fs::write(dir.join(".a.json.new"), "{}").unwrap();
        told().await.unwrap();
        std::fs::rename(dir.join(".a.json.new"), dir.join("a.json")).unwrap();
        told().await.unwrap();
        std::fs::remove_file(dir.join("a.json")).unwrap();
        told().await.unwrap();
    }
}

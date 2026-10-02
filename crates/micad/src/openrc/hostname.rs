//! The hostname on an OpenRC root: `/etc/hostname` (Base's `mica-mounts` binds
//! it from STATE) and the running name set with `sethostname(2)`, where a
//! systemd root asks hostnamed.

use std::path::PathBuf;

use anyhow::Context;

use crate::reconciler::HostnameExecutor;

/// Mode of the hostname file, as Debian ships it.
const HOSTNAME_MODE: u32 = 0o644;

pub struct EtcHostname {
    path: PathBuf,
    set_running: bool,
}

impl EtcHostname {
    pub fn production() -> Self {
        Self {
            path: PathBuf::from("/etc/hostname"),
            set_running: true,
        }
    }

    /// The file at `path`, leaving the running hostname alone.
    #[cfg(test)]
    pub fn file_only(path: PathBuf) -> Self {
        Self {
            path,
            set_running: false,
        }
    }
}

#[async_trait::async_trait]
impl HostnameExecutor for EtcHostname {
    async fn set_static_hostname(&self, name: &str) -> anyhow::Result<()> {
        crate::fswrite::write_config(&self.path, &format!("{name}\n"), HOSTNAME_MODE)
            .with_context(|| format!("write {}", self.path.display()))?;
        if self.set_running {
            rustix::system::sethostname(name.as_bytes()).context("sethostname")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_name_is_written_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hostname");
        EtcHostname::file_only(path.clone())
            .set_static_hostname("mica-01")
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "mica-01\n");
    }
}

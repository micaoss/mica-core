//! Small durable-write helpers shared by the login-guard state and the audit
//! ring.

use std::path::Path;

use anyhow::Context;

/// Seconds since the UNIX epoch, saturating at zero for a clock before 1970.
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Write `contents` to `path` atomically at `mode`.
///
/// The directory is synced after the rename: a power cut is exactly the event
/// the guard state exists to survive.
pub fn write_atomically(path: &Path, contents: impl AsRef<[u8]>, mode: u32) -> anyhow::Result<()> {
    mica_fs::Replace::new(path, "apid-tmp")?
        .mode(mode)
        .write(contents)
        .with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn writes_with_the_requested_mode_and_replaces_existing_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        write_atomically(&path, "first", 0o600).unwrap();
        write_atomically(&path, "second", 0o600).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // No temporary residue is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

//! An owner's driver extension build in an archive: `MICAUPD1` with the
//! unsigned descriptor `ext.json` where a signed envelope would be, and the
//! image and its root hash signature as the objects. What is left ready is
//! the descriptor in the workspace (`verified/ext-<extId>.json`) and the
//! objects by digest, for `mica-deploy extension-install` or a system
//! update's `--extension`.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use anyhow::{Result, ensure};
use serde::Serialize;

use super::*;
use crate::driver_ext::{self, DESCRIPTOR_SCHEMA};

/// A build whose descriptor and objects are in the workspace.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyExtension {
    pub id: String,
    pub name: String,
    pub version: String,
    /// The kernel component it is built for.
    pub kernel: String,
    pub path: PathBuf,
    pub objects: PathBuf,
}

/// Whether an archive's descriptor slot holds a driver extension descriptor,
/// read without checking it: enough to say which reader the bytes go to.
pub(super) fn is_descriptor(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| {
            value
                .get("schema")?
                .as_str()
                .map(|s| s == DESCRIPTOR_SCHEMA)
        })
        .unwrap_or(false)
}

/// The descriptors of the builds staged in the workspace at `root` for the kernel
/// `kernel`, for a system update that brings that kernel.
///
/// # Errors
///
/// The workspace cannot be read.
pub fn staged_extensions(root: &Path, kernel: &str) -> Result<Vec<Vec<u8>>> {
    let verified = root.join("verified");
    let mut staged = Vec::new();
    if verified.symlink_metadata().is_err() {
        return Ok(staged);
    }
    for entry in fs::read_dir(&verified)? {
        let path = entry?.path();
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("ext-") && name.ends_with(".json"));
        if !named {
            continue;
        }
        let bytes = read_bounded(&path, driver_ext::MAX_DESCRIPTOR_BYTES as u64)?;
        if driver_ext::parse_descriptor(&bytes)
            .is_ok_and(|extension| extension.descriptor.kernel == kernel)
        {
            staged.push(bytes);
        }
    }
    staged.sort();
    Ok(staged)
}

impl Acquisition<'_> {
    pub(super) fn import_extension(
        &self,
        descriptor: &[u8],
        input: &mut impl Read,
    ) -> Result<ReadyExtension> {
        let extension = driver_ext::parse_descriptor(descriptor)?;
        let content = &extension.descriptor.content;
        let mut required = BTreeMap::new();
        for artifact in [&content.image, &content.signature] {
            if let Some(previous) = required.insert(artifact.sha256.clone(), artifact.bytes) {
                ensure!(previous == artifact.bytes, "conflicting object lengths");
            }
        }
        self.prepare()?;
        let mut missing = BTreeMap::new();
        for artifact in [&content.image, &content.signature] {
            let stored = self.objects().join(&artifact.sha256);
            if stored.symlink_metadata().is_ok() {
                verify_file(&stored, artifact)?;
            } else {
                missing.insert(artifact.sha256.clone(), artifact.bytes);
            }
        }
        let volumes = BTreeMap::new();
        self.room(&missing, &volumes, None)?;
        self.import_objects(input, required, &missing, false, &volumes)?;
        for artifact in [&content.image, &content.signature] {
            verify_file(&self.objects().join(&artifact.sha256), artifact)?;
        }
        let path = self
            .root
            .join(format!("verified/ext-{}.json", extension.id));
        atomic_write(&path, descriptor)?;
        Ok(ReadyExtension {
            id: extension.id,
            name: extension.descriptor.name,
            version: extension.descriptor.version,
            kernel: extension.descriptor.kernel,
            path,
            objects: self.objects(),
        })
    }
}

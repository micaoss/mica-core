//! The driver extension contract (`docs/mica-core.md` 6.2): out-of-tree kernel
//! modules and firmware for exactly one kernel, in a signed dm-verity image.
//!
//! Trust is the kernel's: the image's root hash carries a PKCS#7 signature the
//! kernel checks when the image is mapped, by a key that chains to its builtin
//! keyring or to an owner certificate enrolled on the device, the chain carried
//! in the PKCS#7; the modules inside are signed the same way. The descriptor
//! (`ext.json`, `mica/driver-ext-descriptor/v1`) is not signed. It locates the
//! image and its geometry, and a reader checks it against the manifest the
//! signed image carries (`manifest.json`, `mica/driver-ext/v1`) before the
//! extension counts. The extension id is the SHA-256 of the descriptor's bytes.
//!
//! Inside the image, modules are under
//! `modules/<kernelRelease>/updates/mica-ext/<name>/` and firmware under
//! `firmware/`, so the image's trees overlay `/usr/lib/modules` and
//! `/usr/lib/firmware` as they are.

use crate::components::{KernelComponent, VerityImage, name, sha256, version_key};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const DESCRIPTOR_SCHEMA: &str = "mica/driver-ext-descriptor/v1";
pub const MANIFEST_SCHEMA: &str = "mica/driver-ext/v1";
/// The most bytes a descriptor holds.
pub const MAX_DESCRIPTOR_BYTES: usize = 4096;
/// The most bytes the in-image manifest holds.
pub const MAX_MANIFEST_BYTES: usize = 65536;
/// The most modules, and the most firmware files, one extension carries.
pub const MAX_MODULES: usize = 64;
pub const MAX_FIRMWARE: usize = 256;

/// `ext.json`: where the signed image is and how to map it.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Descriptor {
    pub schema: String,
    pub name: String,
    pub version: String,
    pub arch: String,
    /// The id of the kernel component the extension is built for.
    pub kernel: String,
    pub content: VerityImage,
}

/// A descriptor and the extension id its bytes give.
#[derive(Debug)]
pub struct Extension {
    pub id: String,
    pub descriptor: Descriptor,
}

/// `manifest.json` in the signed image: what the extension carries.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Manifest {
    pub schema: String,
    pub name: String,
    pub version: String,
    pub arch: String,
    pub kernel: String,
    pub kernel_release: String,
    /// `.ko` paths below `modules/<kernelRelease>/updates/mica-ext/<name>/`.
    pub modules: Vec<String>,
    /// Modules to load at boot that no device alias triggers.
    pub load: Vec<String>,
    /// modprobe options per module of this extension.
    pub options: BTreeMap<String, String>,
    /// Paths below `firmware/`.
    pub firmware: Vec<String>,
}

/// The JSON with no whitespace and every object's keys sorted that `bytes`
/// parse to, refusing any other serialization.
fn canonical<T: serde::de::DeserializeOwned>(bytes: &[u8], limit: usize) -> Result<T> {
    ensure!(bytes.len() <= limit, "driver extension document too large");
    let raw: Value = serde_json::from_slice(bytes)
        .map_err(|_| anyhow::anyhow!("invalid driver extension JSON"))?;
    ensure!(
        serde_json::to_vec(&raw)? == bytes,
        "noncanonical driver extension JSON"
    );
    serde_json::from_value(raw)
        .map_err(|_| anyhow::anyhow!("unknown, missing or invalid driver extension fields"))
}

fn extension_name(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A relative path of plain components, at most four deep.
fn relative_path(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    path.len() <= 128
        && parts.len() <= 4
        && parts.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        })
}

/// The module name kmod knows a `.ko` path by.
fn module_name(path: &str) -> String {
    let file = path.rsplit('/').next().unwrap_or(path);
    file.strip_suffix(".ko").unwrap_or(file).replace('-', "_")
}

fn identity(name: &str, version: &str, arch: &str, kernel: &str) -> Result<()> {
    ensure!(extension_name(name), "invalid driver extension name");
    ensure!(
        version_key(version).is_ok(),
        "invalid driver extension version"
    );
    ensure!(
        matches!(arch, "amd64" | "arm64" | "armhf"),
        "invalid driver extension architecture"
    );
    ensure!(digest(kernel), "invalid driver extension kernel id");
    Ok(())
}

/// Parse `ext.json`.
pub fn parse_descriptor(bytes: &[u8]) -> Result<Extension> {
    let descriptor: Descriptor = canonical(bytes, MAX_DESCRIPTOR_BYTES)?;
    ensure!(
        descriptor.schema == DESCRIPTOR_SCHEMA,
        "unsupported driver extension descriptor schema"
    );
    identity(
        &descriptor.name,
        &descriptor.version,
        &descriptor.arch,
        &descriptor.kernel,
    )?;
    descriptor.content.validate()?;
    Ok(Extension {
        id: sha256(bytes),
        descriptor,
    })
}

/// Parse the manifest the signed image of `descriptor` carries.
pub fn parse_manifest(bytes: &[u8], descriptor: &Descriptor) -> Result<Manifest> {
    let manifest: Manifest = canonical(bytes, MAX_MANIFEST_BYTES)?;
    ensure!(
        manifest.schema == MANIFEST_SCHEMA,
        "unsupported driver extension manifest schema"
    );
    identity(
        &manifest.name,
        &manifest.version,
        &manifest.arch,
        &manifest.kernel,
    )?;
    ensure!(
        name(&manifest.kernel_release).is_ok(),
        "invalid driver extension kernel release"
    );
    ensure!(
        manifest.name == descriptor.name
            && manifest.version == descriptor.version
            && manifest.arch == descriptor.arch
            && manifest.kernel == descriptor.kernel,
        "driver extension manifest does not match its descriptor"
    );
    ensure!(
        !manifest.modules.is_empty() || !manifest.firmware.is_empty(),
        "empty driver extension"
    );
    ensure!(
        manifest.modules.len() <= MAX_MODULES
            && manifest.load.len() <= MAX_MODULES
            && manifest.options.len() <= MAX_MODULES
            && manifest.firmware.len() <= MAX_FIRMWARE,
        "too many driver extension entries"
    );
    let mut modules = BTreeSet::new();
    for path in &manifest.modules {
        ensure!(
            relative_path(path) && path.ends_with(".ko") && path.len() > 3,
            "invalid driver extension module path"
        );
        ensure!(
            modules.insert(module_name(path)),
            "duplicate driver extension module"
        );
    }
    let mut load = BTreeSet::new();
    for module in &manifest.load {
        ensure!(
            modules.contains(module) && load.insert(module),
            "driver extension loads a module it does not carry"
        );
    }
    for (module, line) in &manifest.options {
        ensure!(
            modules.contains(module),
            "driver extension sets options of a module it does not carry"
        );
        ensure!(
            (1..=256).contains(&line.len()) && line.bytes().all(|b| (b' '..=b'~').contains(&b)),
            "invalid driver extension module options"
        );
    }
    let mut firmware = BTreeSet::new();
    for path in &manifest.firmware {
        ensure!(
            relative_path(path) && firmware.insert(path),
            "invalid or duplicate driver extension firmware path"
        );
    }
    Ok(manifest)
}

impl Manifest {
    /// Refuse the extension on any kernel but the one it is built for.
    pub fn check_kernel(&self, kernel: &KernelComponent) -> Result<()> {
        ensure!(
            self.kernel == kernel.id
                && self.kernel_release == kernel.release
                && self.arch == kernel.arch,
            "driver extension built for another kernel"
        );
        Ok(())
    }

    /// The directory in the image, and in the composed `/usr/lib/modules`,
    /// that holds the extension's modules.
    pub fn module_directory(&self) -> String {
        format!(
            "modules/{}/updates/mica-ext/{}",
            self.kernel_release, self.name
        )
    }
}

/// The whole reader contract: the descriptor's bytes, the manifest read from
/// its mapped image, and the kernel it is to run on.
pub fn check(
    descriptor: &[u8],
    manifest: &[u8],
    kernel: &KernelComponent,
) -> Result<(Extension, Manifest)> {
    let extension = parse_descriptor(descriptor)?;
    let manifest = parse_manifest(manifest, &extension.descriptor)?;
    manifest.check_kernel(kernel)?;
    Ok((extension, manifest))
}

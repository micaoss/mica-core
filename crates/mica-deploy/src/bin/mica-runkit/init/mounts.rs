//! Mounting the running system's storage, verified.

use anyhow::{Context, Result, ensure};
use mica_deploy::{
    boot::{
        shutdown::Device,
        startup::{
            self,
            native::{self, Operation as Startup},
        },
    },
    components::VerityImage,
};
use std::{fs, os::unix::fs::MetadataExt};

use super::*;

pub(super) fn bounded_file(path: &str, limit: u64) -> Result<Vec<u8>> {
    Ok(mica_fs::read_bounded(Path::new(path), limit)?)
}

pub(super) fn run(control: &mut BootControl, operation: Startup) -> Result<String> {
    control.supervisor.run_startup(native::command(operation)?)
}

pub(super) fn mount(
    control: &mut BootControl,
    source: &str,
    target: &str,
    kind: &str,
    options: &str,
) -> Result<()> {
    fs::create_dir_all(target)?;
    if kind == "ext4" {
        control.backing(source)?;
    }
    let mounted = run(
        control,
        Startup::Mount {
            source: source.into(),
            target: target.into(),
            kind: kind.into(),
            options: options.into(),
        },
    );
    if target == "/run/initramfs" {
        let live = control.observe()?;
        if let Some(mount) = live
            .mounts
            .into_iter()
            .find(|m| m.path == target && m.kind == "tmpfs" && m.root == "/")
        {
            control.storage.mounts.push(mount);
        }
    }
    mounted.with_context(|| format!("mount {source} at {target}"))?;
    Ok(())
}

/// Mount each core component of `deployment`, verified, at
/// `/run/mica-core/<package>`, refuse a composition that would shadow a file,
/// and compose the components' `usr` and `etc` over the root's with read-only
/// overlays. Runs before anything binds into either tree: an overlay does not
/// show the submounts of its layers.
pub(super) fn compose_core(
    control: &mut BootControl,
    deployment: &mica_deploy::components::Deployment,
    paths: &mica_deploy::components::DeploymentPaths,
) -> Result<()> {
    if deployment.core.is_empty() {
        return Ok(());
    }
    let mut layers = Vec::new();
    for (core, path) in deployment.core.iter().zip(&paths.core) {
        ensure!(
            core.package == path.package,
            "core component order mismatch"
        );
        let target = format!("/run/mica-core/{}", core.package);
        verified_mount(
            control,
            &format!("/system/{}", path.image),
            &format!("/system/{}", path.signature),
            &format!("{}{}", mica_deploy::boot::CORE_MAPPING_PREFIX, core.package),
            &target,
            &core.content,
        )?;
        layers.push(std::path::PathBuf::from(target));
    }
    mica_deploy::boot::check_composition(Path::new("/newroot"), &layers)?;
    for tree in ["usr", "etc"] {
        let mut lower: Vec<String> = layers
            .iter()
            .map(|layer| layer.join(tree))
            .filter(|layer| layer.is_dir())
            .map(|layer| layer.display().to_string())
            .collect();
        if lower.is_empty() {
            continue;
        }
        let target = format!("/newroot/{tree}");
        lower.push(target.clone());
        run(control, Startup::Overlay { target, lower })?;
    }
    eprintln!(
        "mica-init: composed {} core component(s) over the root",
        layers.len()
    );
    Ok(())
}

pub(super) fn verified_mount(
    control: &mut BootControl,
    path: &str,
    signature: &str,
    name: &str,
    target: &str,
    image: &VerityImage,
) -> Result<()> {
    let meta = fs::symlink_metadata(path).with_context(|| format!("stat {path}"))?;
    ensure!(
        meta.is_file() && meta.len() == image.image.bytes,
        "image type or length mismatch: {path}"
    );
    image.signature.verify(&bounded_file(signature, 65536)?)?;
    let loop_device = run(control, Startup::Loop { image: path.into() })?;
    let live = control.observe()?;
    let loop_id = Device::from_raw(fs::metadata(&loop_device)?.rdev());
    let association = live
        .blocks
        .iter()
        .find(|b| b.device == loop_id)
        .and_then(|b| b.association.as_ref())
        .context("new loop association disappeared")?
        .clone();
    ensure!(
        association.backing == Device::from_raw(meta.dev())
            && association.inode == meta.ino()
            && association.flags & 1 == 1
            && association.offset == 0
            && association.size_limit == 0,
        "native loop identity disagrees with verified startup association"
    );
    control.storage.loops.push(association);
    ensure!(
        !live
            .blocks
            .iter()
            .any(|b| b.mapping.as_ref().is_some_and(|m| m.name == name)),
        "MICA mapping already exists before creation"
    );
    let creation = run(
        control,
        Startup::Verity {
            device: loop_device,
            name: name.into(),
            signature: signature.into(),
            image: serde_json::from_value(serde_json::to_value(image)?)?,
        },
    );
    // A worker can create the mapping and then fail. Adopt only the live
    // expected verity table and the already verified owned loop, never a name.
    let live = control.observe()?;
    if let Some(block) = live
        .blocks
        .iter()
        .find(|b| b.mapping.as_ref().is_some_and(|m| m.name == name))
    {
        let mapping = block.mapping.as_ref().context("mapping disappeared")?;
        ensure!(
            block.slaves == std::collections::BTreeSet::from([loop_id])
                && mapping.table.split_whitespace().nth(2) == Some("verity")
                && mapping
                    .table
                    .split_whitespace()
                    .any(|s| s == image.root_hash)
                && !mapping.uuid.is_empty(),
            "created mapping ownership cannot be established"
        );
        control.storage.mappings.push(mapping.clone());
    }
    creation?;
    let mapping = control
        .storage
        .mappings
        .last()
        .context("created mapping is absent")?;
    let expected = startup::verity::table(
        loop_id.major,
        loop_id.minor,
        &format!("cryptsetup:{name}"),
        image,
    )?;
    ensure!(
        mapping.table
            == format!(
                "{} {} {} {}",
                expected.sector, expected.length, expected.kind, expected.parameters
            ),
        "kernel mapping differs from signed verity table"
    );
    mount(
        control,
        &format!("/dev/mapper/{name}"),
        target,
        "squashfs",
        "ro,nodev",
    )
}

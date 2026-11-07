//! Driver extensions at boot (`docs/mica-core.md` 6.5): mapping the builds
//! SYSTEM names for the booted kernel once the owner keyring is sealed, and
//! composing their modules and firmware over the kernel support image's.

use anyhow::{Result, ensure};
use mica_deploy::{
    boot::{EXTENSION_MAPPING_PREFIX, startup::native::Operation as Startup, trust::Sealed},
    components::KernelComponent,
    driver_ext,
    extensions::{self as ext, BootExtension, BootExtensions, BootState, Claims},
    set_state::{self, TrialKind},
    ubi::SysUbi,
};
use std::{fs, path::Path};

use super::*;

/// The size of the tmpfs the composed modules' index is made in.
const MODULE_INDEX_BYTES: u64 = 16 << 20;

/// Map the extensions SYSTEM names for `kernel` at a boot of the deployment
/// `deployment` (`ext::boot_choice`, spending a boot of a change alone on
/// trial in DATA's `meta`), none at a factory reset boot (`reset`), and
/// compose what they carry into the root's
/// `/usr/lib/modules` and `/usr/lib/firmware`; the kernel support image's
/// own trees are bound there when nothing is composed. An extension that
/// does not map, or that maps and does not hold to its descriptor, kernel
/// and manifest, is left out and the boot goes on.
///
/// # Errors
///
/// A tree cannot be bound or an overlay of checked layers cannot be mounted.
pub(super) fn compose(
    control: &mut BootControl,
    sealed: &Sealed,
    ubi: Option<&SysUbi>,
    deployment: &str,
    kernel: &KernelComponent,
    reset: bool,
) -> Result<BootExtensions> {
    for target in ["/newroot/usr/lib/modules", "/newroot/usr/lib/firmware"] {
        ensure!(
            fs::symlink_metadata(target)?.is_dir() && fs::read_dir(target)?.next().is_none(),
            "invalid support mountpoint {target}"
        );
    }
    let mut record = BootExtensions::default();
    let builds = if reset {
        record.skipped = Some("a factory reset boot maps no extension".into());
        Default::default()
    } else {
        match ext::read_state(Path::new("/system")) {
            Ok(state) => {
                let (builds, trial) = ext::boot_choice(&state, deployment, &kernel.id, |id| {
                    set_state::spend_attempt(
                        Path::new("/newroot/mnt/data/meta"),
                        TrialKind::Extensions,
                        id,
                    )
                });
                record.trial = trial;
                builds
            }
            Err(error) => {
                record.skipped = Some(format!("{error:#}"));
                Default::default()
            }
        }
    };
    let mut claims = Claims::default();
    let mut composed = Vec::new();
    for (name, id) in builds {
        let mut entry = BootExtension {
            name: name.clone(),
            id: id.clone(),
            version: None,
            state: BootState::Refused,
            reason: None,
        };
        let mount = format!("{}/{name}", ext::MOUNTS);
        let mapped = (|| -> Result<driver_ext::Extension> {
            let dir = ext::object_dir(Path::new("/system"), &id);
            let extension = driver_ext::parse_descriptor(&mica_fs::read_bounded(
                &dir.join("ext.json"),
                driver_ext::MAX_DESCRIPTOR_BYTES as u64,
            )?)?;
            ensure!(
                extension.id == id
                    && extension.descriptor.name == name
                    && extension.descriptor.kernel == kernel.id,
                "the descriptor is not the build the state names"
            );
            extension_mount(
                control,
                sealed,
                ubi,
                &match ubi {
                    Some(_) => format!("{}{id}", ext::VOLUME_PREFIX),
                    None => dir.join("ext.img").display().to_string(),
                },
                &dir.join("ext.roothash.p7s").display().to_string(),
                &format!("{EXTENSION_MAPPING_PREFIX}{name}"),
                &mount,
                &extension.descriptor.content,
            )?;
            Ok(extension)
        })();
        let extension = match mapped {
            Ok(extension) => extension,
            Err(error) => {
                eprintln!("mica-init: driver extension {name} not mapped: {error:#}");
                entry.reason = Some(format!("{error:#}"));
                record.extensions.push(entry);
                continue;
            }
        };
        entry.version = Some(extension.descriptor.version.clone());
        let checked = (|| -> Result<driver_ext::Manifest> {
            let manifest = driver_ext::parse_manifest(
                &mica_fs::read_bounded(
                    &Path::new(&mount).join("manifest.json"),
                    driver_ext::MAX_MANIFEST_BYTES as u64,
                )?,
                &extension.descriptor,
            )?;
            manifest.check_kernel(kernel)?;
            ext::check_tree(Path::new(&mount), &manifest)?;
            claims.claim(&manifest)?;
            Ok(manifest)
        })();
        match checked {
            Ok(manifest) => {
                entry.state = BootState::Mapped;
                composed.push(manifest);
            }
            Err(error) => {
                eprintln!("mica-init: driver extension {name} left out: {error:#}");
                entry.state = BootState::Unused;
                entry.reason = Some(format!("{error:#}"));
            }
        }
        record.extensions.push(entry);
    }
    let layers = |tree: &str| -> Vec<String> {
        composed
            .iter()
            .map(|manifest| format!("{}/{}/{tree}", ext::MOUNTS, manifest.name))
            .filter(|layer| Path::new(layer).is_dir())
            .collect()
    };
    let (modules, firmware) = (layers("modules"), layers("firmware"));
    // The index is the part that can fail on the owner's content; without
    // it, no extension is composed.
    if !modules.is_empty()
        && let Err(error) = module_index(control, kernel, &modules)
    {
        eprintln!("mica-init: driver extensions not composed: {error:#}");
        for entry in &mut record.extensions {
            if entry.state == BootState::Mapped {
                entry.state = BootState::Unused;
                entry.reason = Some(format!("the modules could not be indexed: {error:#}"));
            }
        }
        composed.clear();
    }
    let (modules, firmware) = if composed.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        (modules, firmware)
    };
    for (tree, mut lower) in [("modules", modules), ("firmware", firmware)] {
        let target = format!("/newroot/usr/lib/{tree}");
        let support = format!("/support/{tree}");
        if lower.is_empty() {
            run(
                control,
                Startup::Bind {
                    source: support,
                    target: target.clone(),
                },
            )?;
            run(
                control,
                Startup::Remount {
                    target,
                    options: "remount,bind,ro,nodev,nosuid".into(),
                },
            )?;
            continue;
        }
        if tree == "modules" {
            lower.insert(0, ext::MODULE_INDEX_LAYER.to_owned());
        }
        lower.push(support);
        run(control, Startup::Overlay { target, lower })?;
    }
    for manifest in &composed {
        for (directory, contents) in [
            ("/run/modprobe.d", ext::modprobe_conf(manifest)),
            ("/run/modules-load.d", ext::modules_load(manifest)),
        ] {
            if !contents.is_empty() {
                fs::create_dir_all(directory)?;
                fs::write(
                    format!(
                        "{directory}/{EXTENSION_MAPPING_PREFIX}{}.conf",
                        manifest.name
                    ),
                    contents,
                )?;
            }
        }
    }
    if !composed.is_empty() {
        eprintln!(
            "mica-init: composed {} driver extension(s) over the kernel support image",
            composed.len()
        );
    }
    Ok(record)
}

/// Compose the extensions' module trees `modules` over the support image's
/// in a tmpfs and index them with the root's `depmod`.
fn module_index(
    control: &mut BootControl,
    kernel: &KernelComponent,
    modules: &[String],
) -> Result<()> {
    mount(
        control,
        "tmpfs",
        ext::MODULE_INDEX,
        "tmpfs",
        &format!("nosuid,nodev,noexec,mode=0755,size={MODULE_INDEX_BYTES}"),
    )?;
    fs::create_dir_all(ext::MODULE_STAGING)?;
    let mut lower = modules.to_vec();
    lower.push("/support/modules".to_owned());
    run(
        control,
        Startup::Overlay {
            target: ext::MODULE_STAGING.to_owned(),
            lower,
        },
    )?;
    run(
        control,
        Startup::ModuleIndex {
            arch: kernel.arch.clone(),
            release: kernel.release.clone(),
        },
    )?;
    Ok(())
}

//! DATA as the startup worker opened it: its mapping owned, its state on the
//! console, and what a first format leaves to write into it once mounted.

use anyhow::{Context, Result, ensure};
use mica_deploy::{boot::shutdown::Device, data_crypt::runkit, factory_reset};
use std::{collections::BTreeSet, fs, os::unix::fs::MetadataExt};

use super::*;

pub(super) use runkit::{Report as DataReport, State as DataState};

/// The recovery passphrase of a first format, in DATA's `meta/`, until the
/// system shows it.
const RECOVERY_KEY: &str = "data-recovery-key";

/// Own the live `mica-data` mapping on the DATA partition at `partition`, if
/// the worker made one, whether or not it then succeeded: the exitrd
/// releases only what is owned.
pub(super) fn adopt_data_mapping(control: &mut BootControl, partition: &str) -> Result<()> {
    let partition = Device::from_raw(fs::metadata(partition)?.rdev());
    let live = control.observe()?;
    let Some(block) = live.blocks.iter().find(|b| {
        b.mapping
            .as_ref()
            .is_some_and(|m| m.name == runkit::MAPPING)
    }) else {
        return Ok(());
    };
    let mapping = block.mapping.as_ref().context("mapping disappeared")?;
    ensure!(
        block.slaves == BTreeSet::from([partition])
            && mapping.table.split_whitespace().nth(2) == Some("crypt")
            && !mapping.uuid.is_empty(),
        "DATA mapping ownership cannot be established"
    );
    if !control.storage.mappings.contains(mapping) {
        control.storage.mappings.push(mapping.clone());
    }
    Ok(())
}

/// The console line of `opened`, and nothing secret: no recovery passphrase.
pub(super) fn data_line(opened: &DataReport) -> String {
    let mut line = format!(
        "MICA_DATA stage={} formatted={}",
        match opened.state {
            DataState::On => "on",
            DataState::Off => "off",
            DataState::Locked => "locked",
        },
        opened.formatted
    );
    if let Some(reason) = &opened.reason {
        line.push_str(&format!(" reason={reason}"));
    }
    if let Some(cipher) = &opened.cipher {
        line.push_str(&format!(" cipher={cipher}"));
    }
    for provider in &opened.providers {
        let state = if provider.unlocked_this_boot {
            "unlocked"
        } else if provider.enrolled {
            "enrolled"
        } else if provider.failure.is_some() {
            "failed"
        } else {
            "unused"
        };
        line.push_str(&format!(" {}={state}", provider.kind));
    }
    for provider in &opened.providers {
        if let Some(failure) = &provider.failure {
            line.push_str(&format!(
                "\nmica-init: DATA provider {}: {failure}",
                provider.kind
            ));
        }
    }
    for failure in &opened.seal_off_failures {
        line.push_str(&format!("\nmica-init: DATA seal-off failed: {failure}"));
    }
    line
}

/// Make a formatted DATA's layout, give it SYSTEM's firmware receipt when it
/// has none, and keep a first format's recovery passphrase in `meta/`.
pub(super) fn seed_data(system: &Path, data: &Path, opened: &DataReport) -> Result<()> {
    if opened.formatted {
        factory_reset::skeleton(data)?;
    }
    let meta = data.join("meta");
    let firmware = system.join("firmware.json");
    if firmware.is_file() && meta.is_dir() && !meta.join("firmware.json").exists() {
        mica_fs::Replace::new(&meta.join("firmware.json"), "seed")?
            .mode(0o600)
            .write(mica_fs::read_bounded(&firmware, 4096)?)?;
        eprintln!("mica-init: DATA firmware receipt seeded from SYSTEM");
    }
    if let Some(recovery) = &opened.recovery {
        mica_fs::Replace::new(&meta.join(RECOVERY_KEY), "new")?
            .mode(0o600)
            .write(format!("{recovery}\n"))?;
    }
    Ok(())
}

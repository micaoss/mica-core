//! Reconciler for the `container` settings subtree.
//!
//! What this switch operates is not a service. The engine is daemonless:
//! `podman run` forks `conmon`, which execs `crun`, and nothing stays resident;
//! mica-podman does not run upstream's `make install.systemd`, so the image
//! contains no podman unit at all -- no `podman.socket` to leave masked and no
//! service to leave stopped.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use micad_settings::{ContainerUnit, Settings};

use super::Reconciler;
use super::systemd::{Systemd, UnitControl, is_active, is_enabled};

/// Mount unit binding `/etc/containers/systemd` from STATE.
pub const QUADLET_MOUNT_UNIT: &str = "etc-containers-systemd.mount";

/// Directory Quadlet reads. Measured from `quadlet --dryrun`, which prints its
/// own search path as `[/run/containers/systemd /etc/containers/systemd
/// /usr/share/containers/systemd]`. `/usr/local/lib/systemd/system` is not on
/// that path and Quadlet never looks at it.
const DEFAULT_QUADLET_DIR: &str = "/etc/containers/systemd";
/// Override for tests.
pub const QUADLET_DIR_ENV: &str = "MICA_QUADLET_DIR";

/// Where systemd leaves what its generators produced.
const DEFAULT_GENERATOR_DIR: &str = "/run/systemd/generator";
/// Override for tests.
pub const GENERATOR_DIR_ENV: &str = "MICA_SYSTEMD_GENERATOR_DIR";

/// Marker identifying a unit as one Quadlet generated for this image.
const GENERATED_UNIT_MARKER: &str = "/usr/bin/podman";

/// The prefix every file this reconciler writes carries.
///
/// The same shape the network reconciler's units take, and for the same
/// reason: the sweep has to be able to tell a file micad wrote from one an
/// integrator dropped into the directory by hand, and delete only the first.
pub const RENDERED_PREFIX: &str = "50-mica-";

/// The unit name Quadlet generates for a declared container.
#[must_use]
pub fn unit_name(container: &str) -> String {
    format!("{RENDERED_PREFIX}{container}.service")
}

/// The `.container` file a declared unit renders to.
///
/// A pure function of the entry: the reconciler re-renders on every pass,
/// compares against what is on disk and writes only on a difference, so a
/// converged device performs no flash write.
#[must_use]
pub fn render_container(name: &str, unit: &ContainerUnit) -> String {
    let mut out = format!(
        "[Unit]\nDescription=mica container {name}\n\n[Container]\nImage={}\n",
        unit.image
    );
    out.push_str(&format!("ContainerName={name}\n"));
    for (key, value) in &unit.environment {
        out.push_str(&format!("Environment={key}={value}\n"));
    }
    for port in &unit.publish {
        out.push_str(&format!(
            "PublishPort={}:{}/{}\n",
            port.host,
            port.container,
            port.protocol.as_str()
        ));
    }
    for volume in &unit.volumes {
        out.push_str(&format!(
            "Volume={}:{}{}\n",
            volume.host,
            volume.container,
            if volume.read_only { ":ro" } else { "" }
        ));
    }
    if !unit.command.is_empty() {
        out.push_str(&format!("Exec={}\n", unit.command.join(" ")));
    }
    out.push_str(&format!("\n[Service]\nRestart={}\n", unit.restart.as_str()));
    if unit.auto_start {
        out.push_str("\n[Install]\nWantedBy=multi-user.target\n");
    }
    out
}

/// Reconciler for the `container` subtree.
pub struct ContainerReconciler<C: UnitControl> {
    quadlet_dir: PathBuf,
    generator_dir: PathBuf,
    control: C,
}

impl<C: UnitControl> ContainerReconciler<C> {
    /// Reconciler reading `quadlet_dir`, scanning `generator_dir` for units
    /// Quadlet produced, and driving the mount through `control`.
    ///
    /// Both paths are parameters so tests run inside a temporary directory and
    /// never read the host's real generator output.
    pub fn new(quadlet_dir: PathBuf, generator_dir: PathBuf, control: C) -> Self {
        Self {
            quadlet_dir,
            generator_dir,
            control,
        }
    }

    /// Names of the units Quadlet generated, newest listing each time.
    ///
    /// An unreadable generator directory yields an empty list rather than an
    /// error: it does not exist before the first daemon-reload of a boot, and
    /// that is the normal state, not a fault.
    fn generated_units(&self) -> Vec<String> {
        let mut units = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.generator_dir) else {
            return units;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "service") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&path) else {
                continue;
            };
            if !body.contains(GENERATED_UNIT_MARKER) {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                units.push(name.to_string());
            }
        }
        units.sort();
        units
    }

    /// `.container`, `.kube` and `.pod` files the integrator left on STATE.
    ///
    /// Reported so that "containers are off" and "there was nothing to run
    /// anyway" are distinguishable in live state. They look identical from
    /// every unit-level observation.
    fn quadlet_files(&self) -> Vec<String> {
        let mut files = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.quadlet_dir) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_quadlet = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| matches!(e, "container" | "kube" | "pod" | "volume" | "network"));
            if !is_quadlet {
                continue;
            }
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                files.push(name.to_string());
            }
        }
        files.sort();
        files
    }

    /// Write the declared units and sweep the ones this reconciler wrote and
    /// no longer declares.
    ///
    /// Returns the files written and the files swept, both sorted. Called
    /// after the bind is up -- the directory is the mount -- and before the
    /// daemon-reload, so Quadlet reads the files this pass produced.
    fn render_declared(
        &self,
        units: &BTreeMap<String, ContainerUnit>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let mut written = Vec::new();
        let mut wanted = Vec::new();
        for (name, unit) in units {
            let file = format!("{RENDERED_PREFIX}{name}.container");
            let path = self.quadlet_dir.join(&file);
            let body = render_container(name, unit);
            wanted.push(file.clone());
            // Compare before writing: these files live on STATE, and an
            // unconditional rewrite costs a flash write on every reconcile.
            if std::fs::read_to_string(&path).is_ok_and(|current| current == body) {
                continue;
            }
            std::fs::write(&path, &body)
                .with_context(|| format!("write the container unit {}", path.display()))?;
            written.push(file);
        }
        let mut swept = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.quadlet_dir) {
            for entry in entries.flatten() {
                let Some(file) = entry.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                // Only this reconciler's own files: a `.container` an
                // integrator dropped in by hand is theirs and is left alone.
                if !file.starts_with(RENDERED_PREFIX) || !file.ends_with(".container") {
                    continue;
                }
                if wanted.contains(&file) {
                    continue;
                }
                std::fs::remove_file(entry.path())
                    .with_context(|| format!("remove the container unit {file}"))?;
                swept.push(file);
            }
        }
        written.sort();
        swept.sort();
        Ok((written, swept))
    }

    /// Bring the bind up and re-run generators so Quadlet sees STATE.
    // Each step logs before it runs, not after. A reconciler that blocks leaves
    // no evidence otherwise: micad is Type=dbus and runs apply_all BEFORE it
    // acquires its bus name, so a hang here shows up as a unit stuck in
    // "activating" with nothing in the journal naming the step it stopped at.
    async fn turn_on(
        &self,
        units: &BTreeMap<String, ContainerUnit>,
    ) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
        tracing::info!("container: turn_on begin");
        tracing::info!("container: reading unit_file_state");
        if !is_enabled(&self.control.unit_file_state(QUADLET_MOUNT_UNIT).await?) {
            self.control.enable(QUADLET_MOUNT_UNIT).await?;
        }
        tracing::info!("container: reading active_state");
        if !is_active(&self.control.active_state(QUADLET_MOUNT_UNIT).await?) {
            tracing::info!("container: starting the bind");
            self.control.start(QUADLET_MOUNT_UNIT).await?;
        }
        // After the mount and before the reload, for the same reason the reload
        // is after the mount: a file written into the image's empty directory
        // is a file Quadlet never reads.
        let (written, swept) = self.render_declared(units)?;
        tracing::info!(written = ?written, swept = ?swept, "container: declared units rendered");
        // After the mount, never before: a generator run with the directory
        // still unmounted parses the image's empty one and produces nothing,
        // and every step would have succeeded.
        tracing::info!("container: requesting daemon-reload");
        self.control.daemon_reload().await?;
        tracing::info!("container: daemon-reload returned");

        // And then start them. Without this step the switch does nothing at
        // all: a daemon-reload re-runs Quadlet, which writes the unit AND --
        // when the .container file has an [Install] section -- the .wants
        // symlink saying it should be running, but systemd does not act on a
        // symlink that appeared during a reload. It starts wanted units when
        // the target is started, and multi-user.target is reached long before
        // micad runs. The units then exist, are marked as wanted, and sit
        // inactive: the bind mounted, the reconciler reporting success, and no
        // container ever running.
        let mut started = Vec::new();
        let generated = self.generated_units();
        tracing::info!(count = generated.len(), units = ?generated, "container: units Quadlet generated");
        for unit in generated {
            // A declared container starts when it says it starts. A unit from
            // a file this reconciler did not write is an integrator's, and
            // keeps the behaviour it had before containers could be declared:
            // it is started.
            if let Some(declared) = units
                .iter()
                .find(|(name, _)| unit_name(name) == unit)
                .map(|(_, declared)| declared)
                && !declared.auto_start
            {
                continue;
            }
            if !is_active(&self.control.active_state(&unit).await?) {
                self.control.start(&unit).await?;
                started.push(unit);
            }
        }
        Ok((started, written, swept))
    }

    /// Stop what is running, then take the bind down.
    ///
    /// Order matters and the reverse is a real failure: unmounting first makes
    /// the `.container` files invisible, the next daemon-reload removes the
    /// generated units from systemd's view, and the containers they started go
    /// on running as orphans that no unit name can now stop.
    async fn turn_off(&self) -> Result<Vec<String>> {
        let mut stopped = Vec::new();
        for unit in self.generated_units() {
            if is_active(&self.control.active_state(&unit).await?) {
                self.control.stop(&unit).await?;
                stopped.push(unit);
            }
        }
        if is_active(&self.control.active_state(QUADLET_MOUNT_UNIT).await?) {
            self.control.stop(QUADLET_MOUNT_UNIT).await?;
        }
        if is_enabled(&self.control.unit_file_state(QUADLET_MOUNT_UNIT).await?) {
            self.control.disable(QUADLET_MOUNT_UNIT).await?;
        }
        // Now the generated units may go: their source is gone, so this reload
        // is what removes them rather than what creates them.
        self.control.daemon_reload().await?;
        Ok(stopped)
    }
}

impl ContainerReconciler<Systemd> {
    /// Production reconciler: paths from [`QUADLET_DIR_ENV`] and
    /// [`GENERATOR_DIR_ENV`] if set, else the system locations.
    pub fn production() -> Self {
        let quadlet_dir = std::env::var(QUADLET_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_QUADLET_DIR));
        let generator_dir = std::env::var(GENERATOR_DIR_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_GENERATOR_DIR));
        Self::new(quadlet_dir, generator_dir, Systemd::new())
    }
}

#[async_trait::async_trait]
impl<C: UnitControl> Reconciler for ContainerReconciler<C> {
    fn name(&self) -> &'static str {
        "container"
    }

    fn subtree(&self) -> &'static str {
        "container"
    }

    async fn apply(&self, settings: &Settings) -> Result<serde_json::Value> {
        let enabled = settings.container.enabled;
        // Named `declared` and not `units`: the local below is the list of
        // units Quadlet generated, which is a different set.
        let declared = &settings.container.units;
        let (started, written, swept, stopped) = if enabled {
            let (started, written, swept) = self.turn_on(declared).await?;
            (started, written, swept, Vec::new())
        } else {
            // Nothing is rendered with the switch off: the directory is not
            // mounted, so a write would land in the image's own read-only
            // copy of it -- and on a device that is a failure, not a file.
            (Vec::new(), Vec::new(), Vec::new(), self.turn_off().await?)
        };

        // Read AFTER the transition, so what is published is what the system
        // now is rather than what it was asked to become.
        let mount_state = self.control.active_state(QUADLET_MOUNT_UNIT).await?;
        let units = if enabled {
            self.generated_units()
        } else {
            Vec::new()
        };
        let files = self.quadlet_files();

        Ok(serde_json::json!({
            "enabled": enabled,
            "quadletMountUnit": QUADLET_MOUNT_UNIT,
            "quadletMountState": mount_state,
            // Distinguishes "off" from "nothing to run": with the bind down the
            // list is empty because the directory is the image's, so it is
            // reported only when the bind is up.
            "quadletFiles": if enabled { files } else { Vec::new() },
            "generatedUnits": units,
            // Distinguishes "the switch is on and units exist" from "the switch
            // is on and something is actually running". They were the same
            // value while nothing was ever started.
            "startedUnits": started,
            "stoppedUnits": stopped,
            // What this pass did with the declared map, named separately from
            // what was already there: a device with a `.container` an
            // integrator wrote and one micad declares has both, and only one
            // of them is this reconciler's to sweep.
            "declaredCount": declared.len(),
            "renderedUnits": written,
            "sweptUnits": swept,
        }))
    }
}

#[cfg(test)]
mod tests;

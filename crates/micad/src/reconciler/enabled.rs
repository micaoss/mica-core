//! The services micad turns on, saved on DATA so the init starts them at boot
//! with or without micad.
//!
//! Enablement under systemd is runtime-scoped and under OpenRC a no-op (the
//! root is read-only), so on its own it lasts until the next boot. [`Saved`]
//! keeps the list in [`ENABLED_PATH`] as well, one unit a line, and answers
//! whether a unit is enabled from it; Base's `mica-units` starts each listed
//! unit at boot. A unit the init reports `masked` stays masked, and one it
//! reports `static` (no `[Install]` section, or any OpenRC service) is listed
//! without asking the init to enable it.

use std::collections::BTreeSet;
use std::path::PathBuf;

use anyhow::{Context, Result};

use super::systemd::UnitControl;
use crate::fswrite::write_config_if_changed;

/// The units micad has enabled, which Base's `mica-units` starts at boot.
pub const ENABLED_PATH: &str = "/var/lib/mica/units/enabled";

/// A [`UnitControl`] whose enablement is also saved in a file.
pub struct Saved<C> {
    inner: C,
    path: PathBuf,
}

impl<C: UnitControl> Saved<C> {
    /// `inner`, saving its enablement in [`ENABLED_PATH`].
    pub fn new(inner: C) -> Self {
        Self::at(inner, PathBuf::from(ENABLED_PATH))
    }

    /// `inner`, saving its enablement in `path`.
    pub fn at(inner: C, path: PathBuf) -> Self {
        Self { inner, path }
    }

    fn listed(&self) -> Result<BTreeSet<String>> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => Ok(text.lines().map(str::to_owned).collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(error) => Err(error).with_context(|| format!("read {}", self.path.display())),
        }
    }

    fn save(&self, units: &BTreeSet<String>) -> Result<()> {
        let text: String = units.iter().map(|unit| format!("{unit}\n")).collect();
        write_config_if_changed(&self.path, &text, 0o644)?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl<C: UnitControl> UnitControl for Saved<C> {
    async fn active_state(&self, unit: &str) -> Result<String> {
        self.inner.active_state(unit).await
    }

    async fn unit_file_state(&self, unit: &str) -> Result<String> {
        if self.inner.unit_file_state(unit).await? == "masked" {
            return Ok("masked".to_string());
        }
        let state = if self.listed()?.contains(unit) {
            "enabled"
        } else {
            "disabled"
        };
        Ok(state.to_string())
    }

    async fn start(&self, unit: &str) -> Result<()> {
        self.inner.start(unit).await
    }

    async fn stop(&self, unit: &str) -> Result<()> {
        self.inner.stop(unit).await
    }

    async fn restart(&self, unit: &str) -> Result<()> {
        self.inner.restart(unit).await
    }

    async fn reset_failed(&self, unit: &str) -> Result<()> {
        self.inner.reset_failed(unit).await
    }

    async fn enable(&self, unit: &str) -> Result<()> {
        if self.inner.unit_file_state(unit).await? != "static" {
            self.inner.enable(unit).await?;
        }
        let mut units = self.listed()?;
        if units.insert(unit.to_owned()) {
            self.save(&units)?;
        }
        Ok(())
    }

    async fn disable(&self, unit: &str) -> Result<()> {
        if self.inner.unit_file_state(unit).await? != "static" {
            self.inner.disable(unit).await?;
        }
        let mut units = self.listed()?;
        if units.remove(unit) {
            self.save(&units)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reconciler::systemd::mock::MockUnitControl;

    fn saved(file: &str) -> (tempfile::TempDir, Saved<MockUnitControl>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("units/enabled");
        (dir, Saved::at(MockUnitControl::new("inactive", file), path))
    }

    #[tokio::test]
    async fn an_enabled_unit_is_saved_and_answered_from_the_file() {
        let (dir, units) = saved("disabled");
        assert_eq!(
            units.unit_file_state("b.service").await.unwrap(),
            "disabled"
        );

        units.enable("b.service").await.unwrap();
        units.enable("a@wlan0.service").await.unwrap();
        units.enable("b.service").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("units/enabled")).unwrap(),
            "a@wlan0.service\nb.service\n"
        );
        assert_eq!(units.unit_file_state("b.service").await.unwrap(), "enabled");

        units.disable("b.service").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("units/enabled")).unwrap(),
            "a@wlan0.service\n"
        );
        assert_eq!(
            units.unit_file_state("b.service").await.unwrap(),
            "disabled"
        );
    }

    // After a reboot the init's runtime enablement is gone, and the file is
    // what still says the unit is on.
    #[tokio::test]
    async fn the_file_outlives_the_init_s_runtime_enablement() {
        let (dir, units) = saved("disabled");
        std::fs::create_dir_all(dir.path().join("units")).unwrap();
        std::fs::write(dir.path().join("units/enabled"), "b.service\n").unwrap();
        assert_eq!(units.unit_file_state("b.service").await.unwrap(), "enabled");
    }

    #[tokio::test]
    async fn a_static_unit_is_listed_without_asking_the_init() {
        let (dir, units) = saved("static");
        units.enable("x.service").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("units/enabled")).unwrap(),
            "x.service\n"
        );
        assert_eq!(units.unit_file_state("x.service").await.unwrap(), "enabled");
        units.disable("x.service").await.unwrap();
        assert_eq!(
            units.unit_file_state("x.service").await.unwrap(),
            "disabled"
        );
    }

    #[tokio::test]
    async fn a_masked_unit_stays_masked() {
        let (_dir, units) = saved("masked");
        assert_eq!(units.unit_file_state("m.service").await.unwrap(), "masked");
    }
}

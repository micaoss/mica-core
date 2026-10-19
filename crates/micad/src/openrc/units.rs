//! [`UnitControl`] over OpenRC's `rc-service`.
//!
//! A systemd unit name maps onto a service script: `x.service` is `x`, and a
//! template instance `a@b.service` is OpenRC's multiplexed `a.b`. Any other
//! unit type has no OpenRC counterpart and is refused.
//!
//! Enablement is a no-op. `/etc/runlevels` is on the read-only root, and micad
//! reconciles every service on every start, so a service it wants running is
//! started each boot -- what runtime enablement gives it under systemd.

use std::sync::Arc;

use anyhow::{Result, bail};

use super::{Commands, Host};
use crate::reconciler::systemd::UnitControl;

/// `rc-service <name> status` exit codes (openrc-run).
const STATUS_STARTED: i32 = 0;
const STATUS_CRASHED: i32 = 32;

/// The service script a systemd unit name maps to.
///
/// # Errors
///
/// A unit that is not a service, or a name that is not one.
pub fn service_name(unit: &str) -> Result<String> {
    let Some(stem) = unit.strip_suffix(".service") else {
        bail!("{unit} is not a service, and OpenRC runs services only");
    };
    let name = stem.replacen('@', ".", 1);
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
    {
        bail!("{unit} names no OpenRC service");
    }
    Ok(name)
}

/// Production [`UnitControl`] for an OpenRC root.
#[derive(Clone)]
pub struct OpenrcUnits {
    commands: Arc<dyn Commands>,
}

impl OpenrcUnits {
    /// Control through `commands`.
    pub fn new(commands: Arc<dyn Commands>) -> Self {
        Self { commands }
    }

    /// Control through the host's `rc-service`.
    pub fn production() -> Self {
        Self::new(Arc::new(Host))
    }

    async fn rc_service(&self, unit: &str, verb: &str) -> Result<i32> {
        let name = service_name(unit)?;
        let output = self.commands.run("rc-service", &[&name, verb]).await?;
        Ok(output.code)
    }

    async fn verb(&self, unit: &str, verb: &str) -> Result<()> {
        let code = self.rc_service(unit, verb).await?;
        anyhow::ensure!(code == 0, "rc-service {unit} {verb} exited {code}");
        Ok(())
    }
}

#[async_trait::async_trait]
impl UnitControl for OpenrcUnits {
    /// systemd's words for OpenRC's states: started is `active`, crashed is
    /// `failed`, anything else `inactive`.
    async fn active_state(&self, unit: &str) -> Result<String> {
        let state = match self.rc_service(unit, "status").await? {
            STATUS_STARTED => "active",
            STATUS_CRASHED => "failed",
            _ => "inactive",
        };
        Ok(state.to_string())
    }

    async fn unit_file_state(&self, unit: &str) -> Result<String> {
        service_name(unit)?;
        Ok("static".to_string())
    }

    async fn start(&self, unit: &str) -> Result<()> {
        self.verb(unit, "start").await
    }

    async fn stop(&self, unit: &str) -> Result<()> {
        self.verb(unit, "stop").await
    }

    async fn restart(&self, unit: &str) -> Result<()> {
        self.verb(unit, "restart").await
    }

    /// A crashed service is marked stopped (`zap`); any other is untouched,
    /// as `ResetFailedUnit` leaves a unit that has not failed.
    async fn reset_failed(&self, unit: &str) -> Result<()> {
        if self.rc_service(unit, "status").await? == STATUS_CRASHED {
            self.verb(unit, "zap").await?;
        }
        Ok(())
    }

    async fn enable(&self, unit: &str) -> Result<()> {
        service_name(unit).map(drop)
    }

    async fn disable(&self, unit: &str) -> Result<()> {
        service_name(unit).map(drop)
    }
}

#[cfg(test)]
mod tests;

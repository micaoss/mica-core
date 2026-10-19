//! [`PowerControl`] through `openrc-shutdown`, which asks `openrc-init` to run
//! the shutdown runlevel and then reboot or power off -- what Base's
//! `mica-init reboot|poweroff` does on this init.

use std::sync::Arc;

use anyhow::Result;

use super::{Commands, Host};
use crate::power::PowerControl;

pub struct OpenrcPower {
    commands: Arc<dyn Commands>,
}

impl OpenrcPower {
    pub fn new(commands: Arc<dyn Commands>) -> Self {
        Self { commands }
    }

    pub fn production() -> Self {
        Self::new(Arc::new(Host))
    }

    async fn shutdown(&self, how: &str) -> Result<()> {
        self.commands
            .run("openrc-shutdown", &[how, "now"])
            .await?
            .success("openrc-shutdown")
            .map(drop)
    }
}

#[async_trait::async_trait]
impl PowerControl for OpenrcPower {
    async fn reboot(&self) -> Result<()> {
        self.shutdown("--reboot").await
    }

    async fn power_off(&self) -> Result<()> {
        self.shutdown("--poweroff").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openrc::FakeCommands;

    #[tokio::test]
    async fn reboot_and_power_off_ask_openrc_shutdown() {
        let fake = Arc::new(FakeCommands::default());
        let power = OpenrcPower::new(Arc::clone(&fake) as Arc<dyn Commands>);
        power.reboot().await.unwrap();
        power.power_off().await.unwrap();
        assert_eq!(
            fake.calls(),
            [
                "openrc-shutdown --reboot now",
                "openrc-shutdown --poweroff now"
            ]
        );
        fake.answer("openrc-shutdown --reboot now", 1, "");
        assert!(power.reboot().await.is_err());
    }
}

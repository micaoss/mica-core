//! [`TimeStatusSource`] on an OpenRC root: whether Base's `mica-ntpd` runs, and
//! the kernel's synchronized bit busybox ntpd keeps -- the bit timedated
//! reports as `NTPSynchronized`. busybox ntpd publishes no server or sample.

use super::OpenrcUnits;
use crate::reconciler::systemd::{UnitControl, is_active};
use crate::time_status::{TimeStatusSource, TimesyncEvidence};

pub struct NtpdTimeStatus {
    units: OpenrcUnits,
}

impl NtpdTimeStatus {
    pub fn new(units: OpenrcUnits) -> Self {
        Self { units }
    }

    pub fn production() -> Self {
        Self::new(OpenrcUnits::production())
    }
}

#[async_trait::async_trait]
impl TimeStatusSource for NtpdTimeStatus {
    async fn observe(&self) -> anyhow::Result<TimesyncEvidence> {
        let running = self
            .units
            .active_state("mica-ntpd.service")
            .await
            .is_ok_and(|state| is_active(&state));
        Ok(TimesyncEvidence {
            service_reachable: running,
            ntp_synchronized: lifecycle_sys::clock_synchronized().ok(),
            ..TimesyncEvidence::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::openrc::{Commands, FakeCommands};

    #[tokio::test]
    async fn a_stopped_ntpd_is_an_unreachable_service() {
        let fake = Arc::new(FakeCommands::default());
        fake.answer("rc-service mica-ntpd status", 3, "");
        let status = NtpdTimeStatus::new(OpenrcUnits::new(Arc::clone(&fake) as Arc<dyn Commands>));
        let evidence = status.observe().await.unwrap();
        assert!(!evidence.service_reachable);
        fake.answer("rc-service mica-ntpd status", 0, "");
        assert!(status.observe().await.unwrap().service_reachable);
    }
}

//! The access point paused for a Wi-Fi scan.
//!
//! One radio is a station or an access point, and nothing scans while
//! hostapd holds it. A scan asked for then stops the access point, scans with
//! a supplicant of its own ([`crate::network_state::scan_alone`]) and starts
//! the access point again (`docs/ble-provisioning.md`, `scanWifi`).

use std::path::Path;
use std::sync::Arc;

use crate::network_state::ScannedNetwork;

use super::super::systemd::{Systemd, UnitControl, is_active};
use super::{OPENRC_UNIT, unit_name};

/// A scan on the access point's interface.
#[async_trait::async_trait]
pub trait ApPause: Send + Sync {
    /// Scan on `interface`. An access point running there is stopped for the
    /// scan and started again after it; with `pause` false, one running there
    /// refuses the scan instead.
    async fn scan(&self, interface: &str, pause: bool) -> Result<Vec<ScannedNetwork>, String>;
}

/// The scan that runs once nothing holds the radio.
type Scanner = Arc<dyn Fn(&str) -> Result<Vec<ScannedNetwork>, String> + Send + Sync>;

/// The pause through the access point's unit.
pub struct UnitPause<C: UnitControl> {
    control: C,
    openrc: bool,
    scanner: Scanner,
}

impl<C: UnitControl> UnitPause<C> {
    /// Pause through `control`, scanning with `scanner`.
    pub fn new(control: C, openrc: bool, scanner: Scanner) -> Self {
        Self {
            control,
            openrc,
            scanner,
        }
    }
}

impl UnitPause<Systemd> {
    /// `hostapd@<interface>`, scanning the host's radio.
    pub fn systemd() -> Self {
        Self::new(Systemd::new(), false, host_scanner())
    }
}

impl UnitPause<crate::openrc::OpenrcUnits> {
    /// mica-wifi-ap, scanning the host's radio.
    pub fn openrc() -> Self {
        Self::new(
            crate::openrc::OpenrcUnits::production(),
            true,
            host_scanner(),
        )
    }
}

fn host_scanner() -> Scanner {
    Arc::new(|interface| crate::network_state::scan_alone(Path::new("/"), interface))
}

#[async_trait::async_trait]
impl<C: UnitControl> ApPause for UnitPause<C> {
    async fn scan(&self, interface: &str, pause: bool) -> Result<Vec<ScannedNetwork>, String> {
        let unit = if self.openrc {
            OPENRC_UNIT.to_string()
        } else {
            unit_name(interface)
        };
        let state = self
            .control
            .active_state(&unit)
            .await
            .map_err(|err| format!("{err:#}"))?;
        let running = is_active(&state);
        if running && !pause {
            return Err(format!("{unit} is running"));
        }
        if running {
            self.control
                .stop(&unit)
                .await
                .map_err(|err| format!("{err:#}"))?;
        }
        let scanner = Arc::clone(&self.scanner);
        let name = interface.to_string();
        let scanned = tokio::task::spawn_blocking(move || scanner(&name))
            .await
            .unwrap_or_else(|err| Err(format!("the scan task failed: {err}")));
        if running && let Err(err) = self.control.start(&unit).await {
            tracing::warn!(unit, error = %format!("{err:#}"), "the access point paused for a scan did not start again");
        }
        scanned
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::systemd::mock::MockUnitControl;
    use super::*;

    fn network(ssid: &str) -> ScannedNetwork {
        ScannedNetwork {
            ssid: ssid.to_string(),
            ..ScannedNetwork::default()
        }
    }

    fn pause(
        active: &str,
        scanned: Result<Vec<ScannedNetwork>, String>,
    ) -> UnitPause<MockUnitControl> {
        UnitPause::new(
            MockUnitControl::new(active, "enabled"),
            false,
            Arc::new(move |interface| {
                assert_eq!(interface, "wlan0");
                scanned.clone()
            }),
        )
    }

    #[tokio::test]
    async fn a_running_access_point_is_stopped_for_the_scan_and_started_again() {
        let pause = pause("active", Ok(vec![network("Harbour")]));
        let scanned = pause.scan("wlan0", true).await.unwrap();
        assert_eq!(scanned, vec![network("Harbour")]);
        assert_eq!(
            pause.control.calls(),
            vec!["stop hostapd@wlan0.service", "start hostapd@wlan0.service"]
        );
    }

    #[tokio::test]
    async fn the_access_point_starts_again_when_the_scan_fails() {
        let pause = pause("active", Err("no results".to_string()));
        assert_eq!(
            pause.scan("wlan0", true).await,
            Err("no results".to_string())
        );
        assert_eq!(
            pause.control.calls(),
            vec!["stop hostapd@wlan0.service", "start hostapd@wlan0.service"]
        );
    }

    #[tokio::test]
    async fn a_running_access_point_refuses_a_scan_that_may_not_pause_it() {
        let pause = pause("active", Ok(vec![network("Harbour")]));
        assert!(pause.scan("wlan0", false).await.is_err());
        assert!(pause.control.calls().is_empty());
    }

    #[tokio::test]
    async fn with_no_access_point_running_the_scan_touches_no_unit() {
        let pause = pause("inactive", Ok(vec![network("Harbour")]));
        assert_eq!(
            pause.scan("wlan0", false).await.unwrap(),
            vec![network("Harbour")]
        );
        assert!(pause.control.calls().is_empty());
    }
}

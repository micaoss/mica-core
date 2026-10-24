//! The last scan kept for `scanWifi`.

use std::time::Duration;

use serde_json::{Value, json};

use super::super::engine::{AP_IN_USE, Scanned};
use super::super::runtime::Provisioning;

fn harbour() -> Vec<Value> {
    vec![json!({ "ssid": "Harbour" })]
}

#[tokio::test]
async fn a_fresh_scan_is_kept_and_answered_again_unless_a_rescan_is_asked() {
    let provisioning = Provisioning::default();
    let fresh = Duration::from_secs(30);
    let first = provisioning
        .scanned(fresh, false, async { Ok(harbour()) })
        .await
        .unwrap();
    assert_eq!(
        first,
        Scanned {
            networks: harbour(),
            age: None
        }
    );
    let again = provisioning
        .scanned(fresh, false, async { Err("scan_failed") })
        .await
        .unwrap();
    assert_eq!(again.networks, harbour(), "within 30 s nothing scans");
    let rescan = provisioning
        .scanned(fresh, true, async { Err("scan_failed") })
        .await;
    assert_eq!(rescan, Err("scan_failed"));
}

#[tokio::test]
async fn an_access_point_in_use_answers_the_last_scan_with_its_age() {
    let provisioning = Provisioning::default();
    assert_eq!(
        provisioning
            .scanned(Duration::ZERO, false, async { Err(AP_IN_USE) })
            .await,
        Err("ap_active"),
        "nothing scanned before"
    );
    provisioning.remember_scan(harbour()).await;
    let stale = provisioning
        .scanned(Duration::ZERO, false, async { Err(AP_IN_USE) })
        .await
        .unwrap();
    assert_eq!(stale.networks, harbour());
    assert!(stale.age.is_some());
}

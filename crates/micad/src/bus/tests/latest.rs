//! `InstallLatestUpdate`: check, fetch and install in one operator request.

use super::super::install_latest;
use super::auto::{Device, TestServed, auto_document};
use crate::update_lifecycle::Refusal;
use std::sync::Arc;

use super::*;

const MANUAL: &str = r#"{"policy": "check", "source": {"url": "http://mirror/tuf"}}"#;

async fn install_latest_on(device: &Device) -> Result<(), Refusal> {
    install_latest(
        Arc::clone(&device.lifecycle),
        TestServed(Arc::clone(&device.service)),
        ":1.6".to_string(),
    )
    .await
}

#[tokio::test]
pub(super) async fn the_newest_release_is_checked_fetched_and_installed() {
    let device = Device::new(MANUAL);
    install_latest_on(&device).await.expect("admitted");
    wait_for_install_status(&device.service, "done").await;
    assert_eq!(
        *device.native.lock().expect("native"),
        vec![format!("install {}", device.descriptor)],
        "exactly the fetched release reached the installer"
    );
    let install = device.update_entry("update.install").await;
    assert_eq!(install["requested_by"], ":1.6");
}

#[tokio::test]
pub(super) async fn a_closed_window_refuses_before_anything_is_fetched() {
    let device = Device::new(&auto_document(false, "window"));
    let refusal = install_latest_on(&device)
        .await
        .expect_err("the window is shut");
    assert!(
        matches!(&refusal, Refusal::Policy(message) if message.contains("maintenance window")),
        "{}",
        refusal.message()
    );
    assert_eq!(
        device.update_entry("update.lifecycle.deploymentId").await,
        serde_json::Value::Null,
        "nothing was fetched"
    );
    assert!(device.native.lock().expect("native").is_empty());
}

#[tokio::test]
pub(super) async fn a_metered_link_refuses_before_anything_is_checked() {
    let device = Device::new(
        r#"{"policy": "check", "network": {"mode": "metered"},
            "source": {"url": "http://mirror/tuf"}}"#,
    );
    let refusal = install_latest_on(&device)
        .await
        .expect_err("a metered link refuses the download");
    assert!(
        matches!(&refusal, Refusal::Policy(message) if message.contains("metered")),
        "{}",
        refusal.message()
    );
    assert_ne!(
        device.update_entry("update.lifecycle.state").await,
        serde_json::Value::String("checking".to_string())
    );
}

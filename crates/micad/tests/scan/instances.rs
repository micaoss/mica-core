//! Services without a device instance, and classes.

use serde_json::Value as Json;
use std::time::Duration;

use super::*;

/// 4. No `/DeviceInstance` — the Venus fallback, named as the gap it is.
#[tokio::test(flavor = "multi_thread")]
pub(super) async fn a_service_without_a_device_instance_falls_back_to_zero() {
    let harness = Harness::start().await;
    let name = "com.mica.meter.noinstance";
    let _fake = harness.fake(name, conforming_without_instance()).await;

    let services = harness.wait_for(name, |services| has(services, name)).await;
    let entry = entry(&services, name);

    assert_eq!(
        entry["instance"], 0,
        "a service with no /DeviceInstance falls back to instance 0; got {entry:#}"
    );
    assert_eq!(
        entry["conformance"]["device_instance"], false,
        "and the fallback is recorded as the gap it is, or an operator reads instance 0 as a \
         number the service chose; got {entry:#}"
    );
    assert_eq!(
        entry["conformance"]["missing_paths"],
        serde_json::json!(["/DeviceInstance"]),
        "{entry:#}"
    );
    assert_eq!(entry["class"], "meter", "{entry:#}");
}

/// 5. Two services of one class both falling back to instance 0: BOTH are
///    marked, and neither is dropped or shadowed by the other.
#[tokio::test(flavor = "multi_thread")]
pub(super) async fn two_instanceless_services_of_a_class_both_carry_the_collision() {
    let harness = Harness::start().await;
    let one = "com.mica.sensor.one";
    let two = "com.mica.sensor.two";
    let _first = harness.fake(one, conforming_without_instance()).await;
    let _second = harness.fake(two, conforming_without_instance()).await;

    let services = harness
        .wait_for("both services", |services| {
            has(services, one) && has(services, two)
        })
        .await;

    for name in [one, two] {
        let entry = entry(&services, name);
        assert_eq!(entry["instance"], 0, "{entry:#}");
        assert_eq!(entry["class"], "sensor", "{entry:#}");
        assert_eq!(
            entry["instance_collision"], true,
            "{name} shares class `sensor` and instance 0 with another connected service, so it \
             must be marked as colliding — both sides, since neither is more at fault than the \
             other; the registry was:\n{services:#}"
        );
        assert_eq!(
            entry["connected"], true,
            "neither service may be dropped or shadowed by the collision; got {entry:#}"
        );
    }
}

/// 6. `ext` is an ordinary direct class, not a privileged namespace.
#[tokio::test(flavor = "multi_thread")]
pub(super) async fn ext_is_an_ordinary_direct_class() {
    let harness = Harness::start().await;
    let name = "com.mica.ext";
    let _fake = harness.fake(name, conforming(9)).await;

    let services = harness.wait_for(name, |services| has(services, name)).await;
    let entry = entry(&services, name);

    assert_eq!(entry["class"], "ext", "{entry:#}");
    assert_eq!(
        entry["conformance"],
        serde_json::json!({}),
        "the fake publishes all seven mandatory paths; got {entry:#}"
    );
    assert_eq!(entry["instance"], 9, "{entry:#}");
}

/// The dry-run gate: with `MICAD_SCAN` unset, a dry-run daemon constructs no
/// scan at all — no subscription, no probe, and no registry to forget from.
#[tokio::test(flavor = "multi_thread")]
pub(super) async fn dry_run_constructs_no_scan() {
    let harness = Harness::start_without_scan().await;
    let name = "com.mica.sensor.unwatched";
    let _fake = harness.fake(name, conforming(2)).await;

    let refused = harness
        .proxy()
        .await
        .forget_service(name)
        .await
        .expect_err("a daemon with no scan has no registry to forget from");
    assert!(
        refused.to_string().contains("no service registry"),
        "the refusal must say the registry is absent, not that the name is: {refused}"
    );

    // And nothing was ever published: a service came up on this bus and the
    // live-state tree never grew a `services` key for it.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let services = harness.services().await;
    assert_eq!(
        services,
        Json::Null,
        "a dry-run daemon must publish no service registry at all; got {services:#}"
    );
}

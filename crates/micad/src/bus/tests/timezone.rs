//! A timezone write names a zone this device's tzdata carries.

use super::*;

/// A root whose tzdata carries `UTC` and `Europe/Berlin`, plus a metadata
/// file that is not a zone.
fn root_with_tzdata(dir: &std::path::Path) -> std::path::PathBuf {
    let root = dir.join("root");
    let zoneinfo = root.join("usr/share/zoneinfo");
    std::fs::create_dir_all(zoneinfo.join("Europe")).unwrap();
    std::fs::write(zoneinfo.join("UTC"), "TZif2").unwrap();
    std::fs::write(zoneinfo.join("Europe/Berlin"), "TZif2").unwrap();
    std::fs::write(zoneinfo.join("zone.tab"), "# tzdb zone descriptions\n").unwrap();
    root
}

async fn timezone(service: &MicadService) -> String {
    service.inner.read().await.settings.time.timezone.clone()
}

#[tokio::test]
pub(super) async fn a_zone_the_tzdata_does_not_carry_is_refused_before_it_is_stored() {
    let (service, _calls, dir) = service_with_mock();
    let service = service.with_device_root(root_with_tzdata(dir.path()));

    for zone in ["Atlantis/Made_Up", "zone.tab"] {
        let err = service
            .persist_setting("time.timezone", serde_json::json!(zone))
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("timezone_invalid"),
            "{zone}: {err}"
        );
        assert_eq!(timezone(&service).await, "UTC", "{zone} was stored");
    }

    service
        .persist_setting("time.timezone", serde_json::json!("Europe/Berlin"))
        .await
        .unwrap();
    assert_eq!(timezone(&service).await, "Europe/Berlin");
}

/// A whole-subtree write is checked the same way as the leaf.
#[tokio::test]
pub(super) async fn a_subtree_write_naming_an_unknown_zone_is_refused() {
    let (service, _calls, dir) = service_with_mock();
    let service = service.with_device_root(root_with_tzdata(dir.path()));

    let err = service
        .persist_setting(
            "time",
            serde_json::json!({ "ntp": { "servers": [] }, "timezone": "Atlantis/Made_Up" }),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("timezone_invalid"), "{err}");
}

/// Without tzdata only `UTC` is a zone: it is the one that needs no file.
#[tokio::test]
pub(super) async fn without_tzdata_only_utc_is_accepted() {
    let (service, _calls, dir) = service_with_mock();
    let root = dir.path().join("bare");
    std::fs::create_dir_all(&root).unwrap();
    let service = service.with_device_root(root);

    assert!(
        service
            .persist_setting("time.timezone", serde_json::json!("Europe/Berlin"))
            .await
            .is_err()
    );
    service
        .persist_setting("time.timezone", serde_json::json!("UTC"))
        .await
        .unwrap();
}

//! Collecting from sources that answer, stall or fail.

use crate::settings_api::{FakeSettings, SettingsApi};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

use super::*;

/// The collection over a fake that answers: every section ok, the
/// snapshot versioned, redacted and carrying the collection record.
#[tokio::test]
pub(super) async fn a_collection_over_answering_sources_is_complete() {
    let fake = FakeSettings::new(json!({ "hostname": "mica", "network": {}, "access": {} }));
    fake.set_state_entry("update", json!({ "boot": {"deploymentId":"a".repeat(64)} }));
    fake.set_state_entry("health", json!({ "var": { "status": "ok", "detail": "" } }));
    let collected = Collector::new(&fake).collect().await;
    let snapshot = &collected.snapshot;
    assert_eq!(snapshot["schemaVersion"], SCHEMA_VERSION);
    assert!(
        snapshot["collectedAt"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z'))
    );
    assert_eq!(
        snapshot["system"]["machineId"]["id"],
        "0123456789abcdef0123456789abcdef"
    );
    assert_eq!(
        snapshot["boot"]["update"]["boot"]["deploymentId"],
        "a".repeat(64)
    );
    assert_eq!(snapshot["boot"]["uptime"]["seconds"], 7);
    assert_eq!(snapshot["failures"]["health"]["var"]["status"], "ok");
    assert_eq!(snapshot["failures"]["tasks"], json!([]));
    assert_eq!(snapshot["time"]["status"], "synchronized");
    assert_eq!(
        snapshot["collection"]["redaction"]["schemaVersion"],
        REDACTION_SCHEMA_VERSION
    );
    for (name, status) in &collected.report.sections {
        assert_eq!(*status, SectionStatus::Ok, "{name}");
        assert_eq!(snapshot["collection"]["sections"][*name], "ok");
    }
    assert_eq!(collected.report.sections.len(), 7);
}

#[tokio::test]
pub(super) async fn the_snapshot_keeps_native_deployment_evidence_and_drops_source_credentials() {
    let fake = FakeSettings::new(json!({ "hostname": "mica", "network": {}, "access": {} }));
    let id = "a".repeat(64);
    fake.set_system_info(json!({
        "deployment": {"available":true,"id":id,"generation":7,"kernelId":"b".repeat(64),
            "rootfsId":"c".repeat(64),"confirmed":true,"contentVerified":true,"secureBoot":false,"bootVerified":true,"backend":"uboot-fit"}
    }));
    fake.set_state_entry("update", json!({
        "boot":{"deploymentId":id,"entry":format!("mica-{id}.conf"),"kernelId":"b".repeat(64),
            "rootfsId":"c".repeat(64),"contentVerified":true,"secureBoot":false,"bootVerified":true,"backend":"uboot-fit"},
        "state":{"highestGeneration":8,"current":id,"fallback":"d".repeat(64),
            "candidate":null,"failed":["e".repeat(64)]},
        "deployments":[{"id":id,"file":format!("mica-{id}.conf"),"generation":7,
            "triesLeft":null,"version":"1.7.0","kernelId":"b".repeat(64),
            "kernelRelease":"6.12.107","rootfsId":"c".repeat(64)}],
        "rollback":{"permitted":true,"target":"d".repeat(64)},
        "install":{"status":"failed","deploymentId":"e".repeat(64),
            "error":"signature verification failed","error_code":"signature-invalid"},
        "last_action":{"action":"confirm","deploymentId":id,"requested_by":":1.7"},
        "lifecycle":{"state":"succeeded","deploymentId":null,
            "policy":{"sourceUrl":"https://user:credential-marker@example.test/catalog"}},
        "slots":{"rootfs.0":{"boot_status":"good"}}
    }));
    let snapshot = Collector::new(&fake).collect().await.snapshot;
    assert_eq!(snapshot["schemaVersion"], SCHEMA_VERSION);
    assert_eq!(snapshot["system"]["deployment"]["id"], id);
    assert_eq!(snapshot["boot"]["deployment"]["confirmed"], true);
    let update = &snapshot["boot"]["update"];
    assert_eq!(update["boot"]["deploymentId"], id);
    assert_eq!(update["state"]["highestGeneration"], 8);
    assert_eq!(update["state"]["failed"], json!(["e".repeat(64)]));
    assert_eq!(update["deployments"][0]["rootfsId"], "c".repeat(64));
    assert_eq!(update["rollback"]["target"], "d".repeat(64));
    assert_eq!(update["install"]["error_code"], "signature-invalid");
    assert_eq!(update["last_action"]["action"], "confirm");
    assert!(update.get("slots").is_none());
    assert!(!snapshot.to_string().contains("credential-marker"));
    assert!(!snapshot.to_string().contains("sourceUrl"));
}

/// The time bound, enforced: sources that answer too slowly are
/// abandoned per section, the deadline stops the rest before they are
/// asked, and the snapshot is still produced — within the bound.
#[tokio::test]
pub(super) async fn slow_sources_are_abandoned_within_the_deadline() {
    let fake = FakeSettings::new(json!({ "hostname": "mica", "network": {}, "access": {} }));
    fake.set_diagnostic_delay(Duration::from_millis(300));
    let collector =
        Collector::new(&fake).with_bounds(Duration::from_millis(500), Duration::from_millis(200));
    let started = Instant::now();
    let collected = collector.collect().await;
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the collection waited on slow sources: {:?}",
        started.elapsed()
    );
    assert!(collected.report.elapsed < Duration::from_secs(3));
    let snapshot = &collected.snapshot;
    assert_eq!(snapshot["schemaVersion"], SCHEMA_VERSION);
    // Every source timed out one way or the other, and the members say
    // so rather than reading as healthy.
    for (name, status) in &collected.report.sections {
        assert_eq!(*status, SectionStatus::Timeout, "{name}");
    }
    assert_eq!(snapshot["system"]["available"], false);
    assert!(
        snapshot["system"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("no answer within"))
    );
    assert_eq!(snapshot["boot"]["deployment"]["available"], false);
    assert!(
        snapshot["boot"]["deployment"]["detail"]
            .as_str()
            .is_some_and(|d| d.starts_with("not collected"))
    );
    // The deadline, not only the per-section bound, did the stopping:
    // the last source was never asked.
    assert!(
        snapshot["network"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("deadline")),
        "{}",
        snapshot["network"]
    );
}

/// A source that errors is `unavailable`, with the error, and the rest
/// of the snapshot is unaffected.
#[tokio::test]
pub(super) async fn an_erroring_source_is_unavailable_not_fatal() {
    struct HalfFailing(FakeSettings);

    #[async_trait::async_trait]
    impl SettingsApi for HalfFailing {
        async fn get_settings(&self, path: &str) -> anyhow::Result<Value> {
            self.0.get_settings(path).await
        }
        async fn set_settings(&self, path: &str, value: &Value) -> anyhow::Result<String> {
            self.0.set_settings(path, value).await
        }
        async fn get_state(&self, path: &str) -> anyhow::Result<Value> {
            self.0.get_state(path).await
        }
        async fn get_time_status(&self) -> anyhow::Result<Value> {
            anyhow::bail!("timesyncd exploded")
        }
        async fn get_storage_status(&self) -> anyhow::Result<Value> {
            self.0.get_storage_status().await
        }
        async fn get_system_info(&self) -> anyhow::Result<Value> {
            self.0.get_system_info().await
        }
        async fn get_telemetry(&self) -> anyhow::Result<Value> {
            self.0.get_telemetry().await
        }
        async fn get_observed_network(&self) -> anyhow::Result<Value> {
            self.0.get_observed_network().await
        }
        async fn get_failure_evidence(&self) -> anyhow::Result<Value> {
            self.0.get_failure_evidence().await
        }
        async fn get_log(&self, source: &str) -> anyhow::Result<Value> {
            self.0.get_log(source).await
        }
        async fn reboot(&self) -> anyhow::Result<()> {
            self.0.reboot().await
        }
        async fn power_off(&self) -> anyhow::Result<()> {
            self.0.power_off().await
        }
        async fn set_transient_root_password(&self, password: &str) -> anyhow::Result<String> {
            self.0.set_transient_root_password(password).await
        }
        async fn rotate_wireguard_key(&self, iface: &str) -> anyhow::Result<String> {
            self.0.rotate_wireguard_key(iface).await
        }
        async fn get_update_state(&self) -> anyhow::Result<Value> {
            self.0.get_update_state().await
        }
        async fn check_update(&self) -> anyhow::Result<()> {
            self.0.check_update().await
        }
        async fn fetch_update(&self) -> anyhow::Result<()> {
            self.0.fetch_update().await
        }
        async fn install_update(&self, bundle: &str) -> anyhow::Result<()> {
            self.0.install_update(bundle).await
        }
        async fn confirm_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
            self.0.confirm_deployment(deployment_id).await
        }

        async fn reject_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
            self.0.reject_deployment(deployment_id).await
        }

        async fn rollback_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
            self.0.rollback_deployment(deployment_id).await
        }
        async fn set_reboot_override(&self, seconds: u32) -> anyhow::Result<Value> {
            self.0.set_reboot_override(seconds).await
        }

        async fn set_update_config(&self, patch: &Value) -> anyhow::Result<Value> {
            self.0.set_update_config(patch).await
        }
    }

    let api = HalfFailing(FakeSettings::new(
        json!({ "hostname": "mica", "network": {}, "access": {} }),
    ));
    let collected = Collector::new(&api).collect().await;
    assert_eq!(
        collected.report.sections["time"],
        SectionStatus::Unavailable
    );
    assert_eq!(collected.report.sections["system"], SectionStatus::Ok);
    assert_eq!(collected.snapshot["time"]["available"], false);
    assert!(
        collected.snapshot["time"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("exploded"))
    );
    assert_eq!(collected.snapshot["system"]["uptime"]["seconds"], 7);
}

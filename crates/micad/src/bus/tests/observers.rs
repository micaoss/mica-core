//! The observation members: time, storage, system information and diagnostics.

use std::sync::Arc;

use super::*;

/// A time observer the test controls, standing in for the two bus reads.
pub(super) struct FixedTimesync(pub(super) crate::time_status::TimesyncEvidence);

#[async_trait::async_trait]
impl crate::time_status::TimeStatusSource for FixedTimesync {
    async fn observe(&self) -> anyhow::Result<crate::time_status::TimesyncEvidence> {
        Ok(self.0.clone())
    }
}

/// Through the method that serves it: an observation whose
/// timedate1 read did not answer reaches the wire as `unknown` with the
/// `synchronized` member absent, not as a device that was asked and found
/// out of sync.
#[tokio::test]
pub(super) async fn the_time_status_reports_an_unread_kernel_bit_as_unknown() {
    use crate::time_status::TimesyncEvidence;
    let (service, _calls, _deployment_calls, _dir) =
        service_with_deployments(MockDeployments::default());

    let observed = TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: None,
        server_name: Some("0.pool.ntp.org".to_string()),
        ..TimesyncEvidence::default()
    };
    let service = service.with_time_status(Arc::new(FixedTimesync(observed.clone())));
    let status = service.get_time_status().await.expect("observed");
    let status: serde_json::Value = serde_json::from_str(&status).expect("json");
    assert_eq!(status["status"], "unknown");
    assert!(status.get("synchronized").is_none(), "{status}");
    assert_eq!(status["server"]["name"], "0.pool.ntp.org");

    // The same observation with the bit actually read is the state the
    // unread one must not be confused with.
    let service = service.with_time_status(Arc::new(FixedTimesync(TimesyncEvidence {
        ntp_synchronized: Some(false),
        ..observed
    })));
    let status = service.get_time_status().await.expect("observed");
    let status: serde_json::Value = serde_json::from_str(&status).expect("json");
    assert_eq!(status["status"], "polling");
    assert_eq!(status["synchronized"], serde_json::json!(false));
}

/// A storage observer the test controls, standing in for sysfs and the
/// mount table.
pub(super) struct FixedStorage(pub(super) crate::storage_status::StorageEvidence);

#[async_trait::async_trait]
impl crate::storage_status::StorageStatusSource for FixedStorage {
    async fn observe(&self) -> anyhow::Result<crate::storage_status::StorageEvidence> {
        Ok(self.0.clone())
    }
}

/// Evidence for a DATA tier at `/srv` with `free` bytes left.
pub(super) fn data_evidence(free: u64) -> crate::storage_status::StorageEvidence {
    use crate::storage_status::{FsSpace, MountEvidence, StorageEvidence, TierEvidence};
    let mut tiers = std::collections::BTreeMap::new();
    tiers.insert(
        "data".to_string(),
        TierEvidence {
            device: Some("/dev/mmcblk0p11".to_string()),
            mount: Some(MountEvidence {
                root: "/".to_string(),
                device: "/dev/mmcblk0p11".to_string(),
                mount: "/srv".to_string(),
                fstype: "ext4".to_string(),
                read_only: false,
            }),
            space: Some(FsSpace {
                total: 1_000_000_000,
                used: 1_000_000_000 - free,
                free,
                reserved: 0,
            }),
            ..TierEvidence::default()
        },
    );
    StorageEvidence {
        directory_bytes: std::collections::BTreeMap::new(),
        project_quotas: None,
        tiers,
        media: Vec::new(),
        binds: std::collections::BTreeMap::new(),
    }
}

/// The storage surface is served from the observer, and a daemon without
/// one says so instead of answering with an empty layout.
#[tokio::test]
pub(super) async fn the_storage_status_is_observed_and_absent_without_an_observer() {
    let (service, _calls, _deployment_calls, _dir) =
        service_with_deployments(MockDeployments::default());
    let unobserved = service
        .get_storage_status()
        .await
        .expect_err("a daemon with no observer cannot answer");
    assert!(
        format!("{unobserved:?}").contains("storage"),
        "{unobserved:?}"
    );

    let service = service.with_storage_status(Arc::new(FixedStorage(data_evidence(25_000_000))));
    let status = service.get_storage_status().await.expect("observed");
    let status: serde_json::Value = serde_json::from_str(&status).expect("json");
    let data = status["tiers"]
        .as_array()
        .expect("tiers")
        .iter()
        .find(|tier| tier["name"] == "data")
        .expect("a data tier")
        .clone();
    assert_eq!(data["mount"], "/srv");
    assert_eq!(data["pressure"], "critical");
}

pub(super) struct FixedSystemInfo(pub(super) crate::system_info::SystemInfoEvidence);

#[async_trait::async_trait]
impl crate::system_info::SystemInfoSource for FixedSystemInfo {
    async fn observe(&self) -> anyhow::Result<crate::system_info::SystemInfoEvidence> {
        Ok(self.0.clone())
    }
}

/// The system-information surface: absent without an observer, and with
/// one it carries the running deployment read from the native backend client the service
/// already holds — one client, not a second reader of the installer.
#[tokio::test]
pub(super) async fn the_system_info_is_observed_with_the_running_deployment_and_absent_without_an_observer()
 {
    let (service, _calls, _deployment_calls, _dir) = service_with_deployments(MockDeployments {
        ..MockDeployments::default()
    });
    let err = service
        .get_system_info()
        .await
        .expect_err("no observer means no answer");
    assert!(format!("{err:?}").contains("observes no system information"));

    let evidence = crate::system_info::SystemInfoEvidence {
        machine_id: Ok("0123456789abcdef0123456789abcdef".to_string()),
        uptime_seconds: Some(42),
        ..crate::system_info::SystemInfoEvidence::default()
    };
    let service = service.with_system_info(Arc::new(FixedSystemInfo(evidence)));
    let info: serde_json::Value =
        serde_json::from_str(&service.get_system_info().await.expect("observed")).unwrap();
    assert_eq!(info["machineId"]["id"], "0123456789abcdef0123456789abcdef");
    assert_eq!(info["uptime"]["seconds"], 42);
    assert_eq!(info["deployment"]["available"], true);
    assert_eq!(info["deployment"]["id"], "a".repeat(64));
    assert_eq!(info["deployment"]["kernelId"], "c".repeat(64));
    assert_eq!(info["daemon"]["name"], "micad");
    // A member the fixture did not supply is absent with a reason, not
    // manufactured.
    assert_eq!(info["board"]["available"], false);
}

/// The other three diagnostic reads refuse without an observer, so a
/// dry-run daemon can never inspect its host through them.
#[tokio::test]
pub(super) async fn the_diagnostic_reads_are_absent_without_observers() {
    let (service, _calls, _dir) = service_with_mock();
    assert!(service.get_telemetry().await.is_err());
    assert!(service.get_observed_network().await.is_err());
    assert!(service.get_failure_evidence().await.is_err());

    let service = service
        .with_telemetry(Arc::new(crate::telemetry::SysfsTelemetry::at(_dir.path())))
        .with_failure_evidence(Arc::new(crate::diagnostics::HostFailureEvidence::new(
            Box::new(EmptyJournal),
            Box::new(NoUnits),
        )));
    let telemetry: serde_json::Value =
        serde_json::from_str(&service.get_telemetry().await.expect("telemetry")).unwrap();
    assert_eq!(telemetry["reset"]["reason"], "unknown");
    assert_eq!(telemetry["reset"]["available"], false);
    let failures: serde_json::Value =
        serde_json::from_str(&service.get_failure_evidence().await.expect("failures")).unwrap();
    assert_eq!(failures["journal"]["lineCount"], 0);
    assert_eq!(failures["units"]["count"], 0);
}

/// The device code is read under the attached root, and its absence is an
/// answer, not an error.
#[tokio::test]
pub(super) async fn the_device_code_names_its_source_or_why_there_is_none() {
    let (service, _calls, dir) = service_with_mock();
    let none: serde_json::Value = serde_json::from_str(&service.get_device_code().await).unwrap();
    assert_eq!(none["code"], serde_json::Value::Null);
    assert_eq!(none["gate"], "off", "a device without a code asks for none");

    let root = dir.path().join("root");
    let serial = root.join("sys/firmware/devicetree/base/serial-number");
    std::fs::create_dir_all(serial.parent().unwrap()).unwrap();
    std::fs::write(&serial, "a5f2c3d4e5f60718\0").unwrap();
    let conf = root.join(crate::device_code::CONFIG_PATH);
    std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
    std::fs::write(&conf, "source=dt-serial\n").unwrap();
    let service = service.with_device_root(root.clone());
    let code: serde_json::Value = serde_json::from_str(&service.get_device_code().await).unwrap();
    assert_eq!(
        code,
        serde_json::json!({"code": "LSZJ-2L3T-WFG", "source": "dt-serial", "gate": "required"})
    );

    // A product that turns the gate off still has, and shows, its code.
    let product = root.join(micad_settings::PRODUCT_FILE.trim_start_matches('/'));
    std::fs::create_dir_all(product.parent().unwrap()).unwrap();
    std::fs::write(&product, "INIT=systemd\nCLAIM_DEVICE_CODE=off\n").unwrap();
    let code: serde_json::Value = serde_json::from_str(&service.get_device_code().await).unwrap();
    assert_eq!(
        code,
        serde_json::json!({"code": "LSZJ-2L3T-WFG", "source": "dt-serial", "gate": "off"})
    );
}

/// An observation that answers what it was given.
struct FixedNetwork(serde_json::Value);

#[async_trait::async_trait]
impl crate::network_state::NetworkState for FixedNetwork {
    async fn describe(&self) -> anyhow::Result<serde_json::Value> {
        Ok(self.0.clone())
    }

    async fn observe(&self) -> anyhow::Result<serde_json::Value> {
        Ok(self.0.clone())
    }
}

/// The observed network carries what the board declares: the label beside a
/// listed port, and a declared port the device lacks.
#[tokio::test]
pub(super) async fn the_observed_network_carries_the_boards_declaration() {
    let (service, _calls, _dir) = service_with_mock();
    let board = crate::board::Board::parse(
        r#"{"version":1,"interfaces":[
            {"name":"eth0","kind":"ethernet","label":"LAN1"},
            {"name":"eth1","kind":"ethernet","label":"LAN2","role":"internal"}]}"#,
    )
    .unwrap();
    let service = service
        .with_network_state(Arc::new(FixedNetwork(serde_json::json!({
            "interfaces": { "available": true, "entries": [{ "name": "eth0", "present": true }] },
        }))))
        .with_board(Arc::new(board));
    let observed: serde_json::Value =
        serde_json::from_str(&service.get_observed_network().await.expect("observed")).unwrap();
    assert_eq!(observed["board"], "declared");
    assert_eq!(
        observed["interfaces"]["entries"][0]["declared"]["label"],
        "LAN1"
    );
    assert_eq!(observed["interfaces"]["entries"][1]["name"], "eth1");
    assert_eq!(observed["interfaces"]["entries"][1]["present"], false);
    assert_eq!(
        observed["interfaces"]["entries"][1]["declared"]["role"],
        "internal"
    );
}

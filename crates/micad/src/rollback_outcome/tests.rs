use serde_json::json;

use super::*;
use crate::deployment::tests::fixture;

const RUNNING: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RETAINED: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

struct Device {
    _dir: tempfile::TempDir,
    record: RollbackRecord,
    path: PathBuf,
    boot: PathBuf,
}

impl Device {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("update/rollback.json");
        let boot = dir.path().join("boot_id");
        std::fs::write(&boot, "boot-1\n").expect("boot id");
        Self {
            record: RollbackRecord::new(path.clone(), boot.clone()),
            _dir: dir,
            path,
            boot,
        }
    }

    fn reboot(&self, boot: &str) {
        std::fs::write(&self.boot, boot).expect("boot id");
    }
}

/// The fixture after a reboot that ran `booted`, with `edit` applied.
fn status_after(booted: &str, edit: impl FnOnce(&mut Value)) -> Status {
    let mut value = fixture();
    value["boot"]["deploymentId"] = json!(booted);
    value["boot"]["entry"] = json!(format!("mica-{booted}.conf"));
    edit(&mut value);
    Status::parse(&value.to_string()).expect("status")
}

/// The outcome is decided once, in the first boot after the rollback, and is
/// reported throughout that boot and in no later one.
#[test]
fn a_rollback_that_took_is_reported_for_the_boot_after_it() {
    let device = Device::new();
    let before = Status::parse(&fixture().to_string()).expect("status");
    device.record.stage(&before, RETAINED).expect("stage");
    assert_eq!(device.record.outcome(&before), None, "not rebooted yet");

    device.reboot("boot-2");
    let after = status_after(RETAINED, |_| {});
    let expected = json!({"outcome": "rolled_back", "target": RETAINED,
        "targetGeneration": 1, "targetVersion": "test-1"});
    assert_eq!(device.record.outcome(&after), Some(expected.clone()));
    // A micad restart in the same boot reports the same outcome.
    assert_eq!(device.record.outcome(&after), Some(expected));

    device.reboot("boot-3");
    assert_eq!(device.record.outcome(&after), None);
    assert!(!device.path.exists(), "the record is dropped once reported");
}

/// A case: its name, the edit to the booted status and the reason expected.
type Case = (&'static str, fn(&mut Value), Option<&'static str>);

#[test]
fn a_rollback_that_did_not_take_names_what_booted_and_why() {
    let cases: [Case; 4] = [
        (
            "failed",
            |value| value["state"]["failed"] = json!([RETAINED]),
            Some("target_failed"),
        ),
        (
            "exhausted",
            |value| value["deployments"][1]["triesLeft"] = json!(0),
            Some("target_exhausted"),
        ),
        (
            "missing",
            |value| {
                value["deployments"].as_array_mut().expect("list").remove(1);
                value["state"]["fallback"] = Value::Null;
            },
            Some("target_missing"),
        ),
        ("no reason recorded", |_| {}, None),
    ];
    for (case, edit, reason) in cases {
        let device = Device::new();
        let before = Status::parse(&fixture().to_string()).expect("status");
        device.record.stage(&before, RETAINED).expect("stage");
        device.reboot("boot-2");
        let outcome = device
            .record
            .outcome(&status_after(RUNNING, edit))
            .expect("an outcome");
        assert_eq!(outcome["outcome"], "did_not_take", "{case}");
        assert_eq!(outcome["booted"], RUNNING, "{case}");
        assert_eq!(outcome["bootedGeneration"], 2, "{case}");
        assert_eq!(outcome["bootedVersion"], "test-2", "{case}");
        assert_eq!(outcome["targetGeneration"], 1, "{case}");
        assert_eq!(outcome["reason"], json!(reason), "{case}");
    }
}

#[test]
fn no_record_and_an_unreadable_record_report_nothing() {
    let device = Device::new();
    let status = Status::parse(&fixture().to_string()).expect("status");
    assert_eq!(device.record.outcome(&status), None);
    std::fs::create_dir_all(device.path.parent().expect("dir")).expect("dir");
    std::fs::write(&device.path, "not json").expect("seed");
    assert_eq!(device.record.outcome(&status), None);
    assert!(!device.path.exists());
}

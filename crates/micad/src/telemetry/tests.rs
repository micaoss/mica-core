use super::*;

fn write(root: &Path, relative: &str, contents: &str) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, contents).expect("write fixture");
}

#[test]
fn bootstatus_bits_decode_by_name() {
    assert_eq!(decode_bootstatus(0), Vec::<&str>::new());
    assert_eq!(decode_bootstatus(0x20), vec!["cardReset"]);
    assert_eq!(decode_bootstatus(0x21), vec!["overheat", "cardReset"]);
    assert_eq!(decode_bootstatus(0x8000), vec!["keepalivePing"]);
    // A bit the header does not name decodes to nothing rather than to a
    // guess.
    assert_eq!(decode_bootstatus(0x100), Vec::<&str>::new());
}

/// The full sysfs shape: two thermal zones, a hwmon chip with a labelled
/// input, a watchdog that fired, and a crash record.
#[tokio::test]
async fn a_populated_sysfs_yields_every_member() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write(
        root,
        "sys/class/thermal/thermal_zone0/type",
        "soc-thermal\n",
    );
    write(root, "sys/class/thermal/thermal_zone0/temp", "48250\n");
    write(
        root,
        "sys/class/thermal/thermal_zone1/type",
        "gpu-thermal\n",
    );
    write(root, "sys/class/thermal/thermal_zone1/temp", "-1500\n");
    write(root, "sys/class/thermal/cooling_device0/type", "fan\n");
    write(root, "sys/class/hwmon/hwmon0/name", "pmic\n");
    write(root, "sys/class/hwmon/hwmon0/temp1_input", "39000\n");
    write(root, "sys/class/hwmon/hwmon0/temp1_label", "die\n");
    write(root, "sys/class/hwmon/hwmon0/temp2_input", "41000\n");
    write(root, "sys/class/watchdog/watchdog0/identity", "dw_wdt\n");
    write(root, "sys/class/watchdog/watchdog0/state", "active\n");
    write(root, "sys/class/watchdog/watchdog0/timeout", "30\n");
    write(root, "sys/class/watchdog/watchdog0/bootstatus", "32\n");
    write(root, "sys/class/watchdog/watchdog0/nowayout", "1\n");
    write(root, "sys/fs/pstore/dmesg-ramoops-0", "Panic#1 Part1\n");
    write(root, "sys/fs/pstore/console-ramoops-0", "...\n");

    let evidence = SysfsTelemetry::at(root).observe().await.expect("observe");
    let telemetry = telemetry_json(&evidence);

    assert_eq!(telemetry["thermal"]["available"], true);
    assert_eq!(telemetry["thermal"]["zones"][0]["label"], "soc-thermal");
    assert_eq!(telemetry["thermal"]["zones"][0]["milliCelsius"], 48250);
    assert_eq!(telemetry["thermal"]["zones"][1]["milliCelsius"], -1500);
    assert_eq!(telemetry["thermal"]["zones"].as_array().unwrap().len(), 2);
    assert_eq!(telemetry["thermal"]["hwmon"][0]["label"], "pmic die");
    assert_eq!(telemetry["thermal"]["hwmon"][0]["sensor"], "hwmon0/temp1");
    assert_eq!(telemetry["thermal"]["hwmon"][1]["label"], "pmic temp2");
    assert_eq!(telemetry["watchdog"]["devices"][0]["identity"], "dw_wdt");
    assert_eq!(telemetry["watchdog"]["devices"][0]["timeoutSeconds"], 30);
    assert_eq!(telemetry["watchdog"]["devices"][0]["nowayout"], true);
    assert_eq!(
        telemetry["watchdog"]["devices"][0]["bootstatus"]["flags"],
        json!(["cardReset"])
    );
    assert!(
        telemetry["watchdog"]["devices"][0]
            .get("timeLeftSeconds")
            .is_none()
    );
    // The watchdog flag outranks the crash record.
    assert_eq!(telemetry["reset"]["available"], true);
    assert_eq!(telemetry["reset"]["reason"], "watchdog");
    assert_eq!(
        telemetry["reset"]["evidence"]["pstore"]["records"],
        json!(["console-ramoops-0", "dmesg-ramoops-0"])
    );
}

/// A crash record with a clean watchdog is a kernel crash; a console
/// record alone is not — it is the previous boot's console, which every
/// ramoops reboot leaves behind.
#[test]
fn a_dmesg_record_is_a_crash_and_a_console_record_is_not() {
    let crashed = TelemetryEvidence {
        watchdogs: vec![WatchdogEvidence {
            bootstatus: Some(0),
            ..WatchdogEvidence::default()
        }],
        pstore_mounted: true,
        pstore_records: vec!["dmesg-ramoops-0".to_string()],
        ..TelemetryEvidence::default()
    };
    assert_eq!(classify_reset(&crashed), (ResetReason::KernelCrash, true));

    let console_only = TelemetryEvidence {
        pstore_records: vec!["console-ramoops-0".to_string()],
        ..crashed
    };
    assert_eq!(classify_reset(&console_only), (ResetReason::Unknown, true));
    let rendered = telemetry_json(&console_only);
    assert_eq!(rendered["reset"]["reason"], "unknown");
    assert_eq!(rendered["reset"]["available"], true);
    assert!(
        rendered["reset"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("indistinguishable"))
    );
}

/// The empty root: every member absent with a reason, the reset reason
/// `unknown` AND `available: false`, because no source spoke.
#[tokio::test]
async fn an_empty_sysfs_reports_absence_not_health() {
    let dir = tempfile::tempdir().expect("tempdir");
    let evidence = SysfsTelemetry::at(dir.path())
        .observe()
        .await
        .expect("observe");
    let telemetry = telemetry_json(&evidence);
    assert_eq!(telemetry["thermal"]["available"], false);
    assert!(telemetry["thermal"]["detail"].is_string());
    assert_eq!(telemetry["watchdog"]["available"], false);
    assert_eq!(telemetry["reset"]["available"], false);
    assert_eq!(telemetry["reset"]["reason"], "unknown");
    assert!(
        telemetry["reset"]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("no reset-reason source"))
    );
    assert_eq!(telemetry["reset"]["evidence"]["pstore"]["available"], false);
}

/// A watchdog driver without `bootstatus` is reported as such on the
/// device, and does not count as a source for the reset reason.
#[tokio::test]
async fn a_watchdog_without_bootstatus_is_absent_evidence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    write(root, "sys/class/watchdog/watchdog0/identity", "sp805\n");
    write(root, "sys/class/watchdog/watchdog0/state", "inactive\n");
    let evidence = SysfsTelemetry::at(root).observe().await.expect("observe");
    let telemetry = telemetry_json(&evidence);
    assert_eq!(telemetry["watchdog"]["available"], true);
    assert_eq!(
        telemetry["watchdog"]["devices"][0]["bootstatus"]["available"],
        false
    );
    assert_eq!(telemetry["reset"]["available"], false);
}

/// A negative bootstatus (a driver error) is not a bitmask.
#[test]
fn a_negative_bootstatus_is_not_a_bitmask() {
    assert_eq!(parse_bootstatus("-1\n"), None);
    assert_eq!(parse_bootstatus("32\n"), Some(32));
    assert_eq!(parse_bootstatus("x"), None);
}

#[tokio::test]
async fn the_unavailable_adapter_is_an_error() {
    assert!(UnavailableTelemetry.observe().await.is_err());
}

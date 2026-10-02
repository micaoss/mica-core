//! The automatic update path against the same gates.

use super::super::{BusRoutes, MicadService, ServedDaemon, fdo};
use crate::power::MockPower;
use serde_json::Value;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// The served-object hop, and nothing else. See [`ServedDaemon`].
pub(super) struct TestServed(pub(super) Arc<MicadService>);

#[async_trait::async_trait]
impl ServedDaemon for TestServed {
    async fn update_state(&self) -> fdo::Result<String> {
        self.0.refresh_update_state().await
    }

    async fn install(&self, sender: &str, descriptor: &str) -> fdo::Result<()> {
        self.0.request_install(sender, descriptor).await
    }

    async fn reboot(&self, sender: &str) -> fdo::Result<()> {
        self.0.request_reboot(sender).await
    }

    async fn clock_trust(&self) -> crate::time_status::ClockTrust {
        self.0.clock_trust().await
    }
}

/// An update client scripted by subcommand, answering in `mica-deploy`'s
/// printed contract.
pub(super) struct ScriptedClient {
    pub(super) staged: String,
    pub(super) selected: String,
    pub(super) calls: CallLog,
}

#[async_trait::async_trait]
impl crate::update_lifecycle::UpdateClient for ScriptedClient {
    fn unavailable(&self) -> Option<String> {
        None
    }

    async fn run(
        &self,
        args: &[String],
        _timeout: Duration,
    ) -> anyhow::Result<crate::update_lifecycle::ClientOutput> {
        let verb = args
            .get(if args.first().is_some_and(|arg| arg == "--max-bytes") {
                2
            } else {
                0
            })
            .cloned()
            .unwrap_or_default();
        self.calls.lock().expect("client calls").push(verb.clone());
        let id = self.selected.strip_suffix(".json").unwrap();
        let stdout = match verb.as_str() {
            "probe" => serde_json::json!({"status":"ready","freeBytes":1_000_000_000u64}).to_string(),
            "check" => serde_json::json!({"revision":1,"selected":{"deploymentId":id,"deployment":{"version":"1.5.0"}}}).to_string(),
            "fetch" => serde_json::json!({"id":id,"path":self.staged,"objects":std::path::Path::new(&self.staged).parent().unwrap().join("objects"),"version":"1.5.0","generation":3}).to_string(),
            "status" => crate::deployment::tests::fixture().to_string(),
            other => anyhow::bail!("unexpected native command: {other}"),
        };
        Ok(crate::update_lifecycle::ClientOutput {
            code: Some(0),
            stdout,
            stderr: String::new(),
        })
    }
}

/// One device, wired the way `main.rs` wires it.
pub(super) struct Device {
    pub(super) dir: tempfile::TempDir,
    pub(super) service: Arc<MicadService>,
    pub(super) lifecycle: Arc<crate::update_lifecycle::UpdateLifecycle>,
    pub(super) routes: Arc<BusRoutes<TestServed>>,
    pub(super) power: CallLog,
    pub(super) native: CallLog,
    pub(super) descriptor: String,
}

pub(super) const STAGED_BUNDLE: &str =
    "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee.json";

impl Device {
    /// A device carrying `document`, a believed clock and the native backend mock
    /// given.
    pub(super) fn with_deployments(document: &str, native: MockDeployments) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let shadow_path = dir.path().join("shadow");
        std::fs::write(&shadow_path, SHADOW).expect("seed shadow");
        let policy_path = dir.path().join("updates.json");
        std::fs::write(&policy_path, document).expect("seed the policy document");
        let descriptor = verified_descriptor(&dir, STAGED_BUNDLE);
        let power = Arc::new(Mutex::new(Vec::new()));
        let deployment_calls = Arc::clone(&native.calls);
        let service = MicadService::new(
            store_in(&dir),
            micad_settings::Settings::default(),
            Vec::new(),
            Box::new(MockPower {
                calls: Arc::clone(&power),
            }),
            shadow_path,
            serde_json::json!({}),
        )
        .with_update_workspace(dir.path().join("updates"))
        .with_update(
            Arc::new(ScriptedClient {
                staged: descriptor.clone(),
                selected: STAGED_BUNDLE.to_string(),
                calls: Arc::new(Mutex::new(Vec::new())),
            }),
            crate::update_policy::PolicyStore::at(policy_path),
        )
        .with_deployments(Arc::new(native))
        // The predicate is asserted on its own in
        // `update_auto`; here it must simply hold, or every install
        // below would defer on the clock before reaching its gate.
        .with_time_status(Arc::new(FixedTimesync(synchronized_evidence())));
        let service = Arc::new(service);
        let lifecycle = service.update_handle();
        let routes = Arc::new(BusRoutes::new(
            Arc::clone(&lifecycle),
            TestServed(Arc::clone(&service)),
        ));
        Self {
            dir,
            service,
            lifecycle,
            routes,
            power,
            native: deployment_calls,
            descriptor,
        }
    }

    pub(super) fn new(document: &str) -> Self {
        Self::with_deployments(document, MockDeployments::default())
    }

    pub(super) fn rewrite(&self, document: &str) {
        std::fs::write(self.dir.path().join("updates.json"), document)
            .expect("rewrite the policy document");
    }

    /// A driver over this device's real routes, with a cadence a test
    /// moves by hand.
    pub(super) fn driver(
        &self,
    ) -> (
        crate::update_auto::AutoDriver,
        Arc<crate::update_auto::TestCadence>,
    ) {
        let cadence = crate::update_auto::TestCadence::new();
        let routes: Arc<dyn crate::update_auto::AutoRoutes> = self.routes.clone();
        let clock: Arc<dyn crate::update_auto::Cadence> = cadence.clone();
        (crate::update_auto::AutoDriver::new(routes, clock), cadence)
    }

    pub(super) async fn update_entry(&self, path: &str) -> Value {
        let raw = self.service.get_state(path).await.unwrap_or_default();
        serde_json::from_str(&raw).unwrap_or(Value::Null)
    }

    /// The deferral the automatic path last recorded, as an operator
    /// reads it back out of `update.lifecycle`.
    pub(super) async fn deferral(&self) -> Value {
        self.update_entry("update.lifecycle.deferred").await
    }

    pub(super) async fn reboot_gate(&self) -> Value {
        self.update_entry("update.lifecycle.reboot_gate").await
    }

    /// Wait out a check the manual route spawned, so the next assertion
    /// is not answered by the busy guard instead of the gate it means to
    /// ask about.
    pub(super) async fn settled(&self) {
        for _ in 0..200 {
            let state = self.update_entry("update.lifecycle.state").await;
            if state != Value::String("checking".to_string())
                && state != Value::String("downloading".to_string())
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the lifecycle never settled");
    }
}

/// Timesync evidence a healthy networked device settles into, so
/// `clock_trust` believes the clock by the first limb.
pub(super) fn synchronized_evidence() -> crate::time_status::TimesyncEvidence {
    crate::time_status::TimesyncEvidence {
        service_reachable: true,
        ntp_synchronized: Some(true),
        server_name: Some("time.example".to_string()),
        server_address: Some("192.0.2.10".to_string()),
        sample: Some(crate::time_status::NtpSample {
            leap: 0,
            stratum: 2,
            spike: false,
            offset_seconds: 0.001,
            packet_count: 8,
        }),
    }
}

/// An `auto` document with a maintenance window `hours` from now, open
/// when `open`. Two hours either side, so the verdict does not depend on
/// the minute the suite runs in.
pub(super) fn auto_document(open: bool, reboot_policy: &str) -> String {
    let face = |offset: chrono::Duration| (chrono::Utc::now() + offset).format("%H:%M").to_string();
    let (start, end) = if open {
        (
            face(-chrono::Duration::hours(2)),
            face(chrono::Duration::hours(2)),
        )
    } else {
        (
            face(chrono::Duration::hours(2)),
            face(chrono::Duration::hours(4)),
        )
    };
    format!(
        r#"{{"policy": "auto", "checkIntervalMinutes": 60,
             "rebootPolicy": "{reboot_policy}",
             "source": {{"url": "http://mirror/tuf"}},
             "maintenance": {{"windows": [{{"start": "{start}", "end": "{end}"}}]}}}}"#
    )
}

/// **A test asserting the automatic and manual paths meet
/// the same gate set.**
#[tokio::test]
pub(super) async fn the_automatic_and_manual_paths_meet_the_same_gate_set() {
    use crate::update_auto::{AutoRoutes, SENDER};

    // The check, refused by network mode. Both routes call `admit_check`.
    let device = Device::new(
        r#"{"policy": "auto", "network": {"mode": "offline"},
            "maintenance": {"windows": [{"start": "00:00", "end": "23:59"}]}}"#,
    );
    let manual = device
        .lifecycle
        .request_check(":1.7")
        .await
        .expect_err("offline refuses an operator's check");
    let automatic = device
        .routes
        .check(SENDER)
        .await
        .expect_err("and refuses the automatic one");
    assert_eq!(manual.message(), automatic.message());
    assert!(manual.message().contains("offline"), "{}", manual.message());

    // The check, refused because the document does not load. There is no
    // selection to read a source out of, so neither path may guess one.
    let device = Device::new("{not json");
    let manual = device
        .lifecycle
        .request_check(":1.7")
        .await
        .expect_err("an unreadable document refuses an operator's check");
    let automatic = device
        .routes
        .check(SENDER)
        .await
        .expect_err("and the automatic one");
    assert_eq!(manual.message(), automatic.message());
    assert!(manual.message().contains("invalid"), "{}", manual.message());

    // The fetch, refused by metered mode. `fetch_refusal` is a superset of
    // `check_refusal`, and both routes call `admit_fetch`.
    let device = Device::new(
        r#"{"policy": "auto", "source": {"url": "http://mirror/tuf"},
            "network": {"mode": "metered", "meteredAllowsFetch": false},
            "maintenance": {"windows": [{"start": "00:00", "end": "23:59"}]}}"#,
    );
    let manual = device
        .lifecycle
        .request_fetch(":1.7")
        .await
        .expect_err("metered refuses an operator's download");
    let automatic = device
        .routes
        .fetch(SENDER)
        .await
        .expect_err("and the automatic one");
    assert_eq!(manual.message(), automatic.message());
    assert!(manual.message().contains("metered"), "{}", manual.message());
    // The same document admits the CHECK on both routes: metered gates
    // downloads, not discovery, and a gate set copied wholesale from the
    // fetch would be stricter than the one designed.
    device.routes.check(SENDER).await.expect("admitted");
    device
        .lifecycle
        .request_check(":1.7")
        .await
        .expect("admitted");
    device.settled().await;

    // The install, refused by the maintenance window.
    let device = Device::new(&auto_document(false, "manual"));
    let manual = device
        .service
        .request_install(":1.7", &device.descriptor)
        .await
        .expect_err("a shut window refuses an operator's install");
    let automatic = device
        .routes
        .install(SENDER, &device.descriptor)
        .await
        .expect_err("and the automatic one");
    assert!(
        manual.to_string().ends_with(&automatic),
        "manual: {manual}, automatic: {automatic}"
    );
    assert!(automatic.contains("outside every configured maintenance window"));

    // The install, refused because the descriptor is not a verified one. The
    // automatic path reaches this route with a staged path, so the rule
    // that only `verified/` is installable binds it too.
    let stray = device.dir.path().join("stray.json");
    std::fs::write(&stray, b"not staged").expect("seed a stray descriptor");
    let stray = stray.to_str().expect("utf-8");
    let device = Device::new(&auto_document(true, "manual"));
    let manual = device
        .service
        .request_install(":1.7", stray)
        .await
        .expect_err("a path outside verified/ refuses an operator's install");
    let automatic = device
        .routes
        .install(SENDER, stray)
        .await
        .expect_err("and the automatic one");
    assert!(
        manual.to_string().ends_with(&automatic),
        "manual: {manual}, automatic: {automatic}"
    );

    // The reboot, refused by the safe-to-reboot gate.
    let device = Device::new(&auto_document(true, "window"));
    device
        .service
        .report_health("exporter", "blocking", "mid-transaction")
        .await
        .expect("report");
    let manual = device
        .service
        .request_reboot(":1.7")
        .await
        .expect_err("a blocking report refuses an operator's reboot");
    let automatic = device
        .routes
        .reboot(SENDER)
        .await
        .expect_err("and the automatic one");
    assert!(
        manual.to_string().ends_with(&automatic),
        "manual: {manual}, automatic: {automatic}"
    );
    assert!(automatic.contains("exporter"), "{automatic}");
    assert!(
        device.power.lock().expect("power").is_empty(),
        "neither refused reboot may reach the power control"
    );

    // The reboot, refused by an install in flight — the block no override
    // lifts. Reached by an install this driver started, which is the only
    // way the automatic path can be holding one.
    let gate = Arc::new(tokio::sync::Notify::new());
    let device = Device::with_deployments(
        &auto_document(true, "window"),
        MockDeployments {
            install_gate: Some(Arc::clone(&gate)),
            ..MockDeployments::default()
        },
    );
    device
        .routes
        .install(SENDER, &device.descriptor)
        .await
        .expect("the window is open");
    wait_for_install_status(&device.service, "running").await;
    let manual = device
        .service
        .request_reboot(":1.7")
        .await
        .expect_err("an install in flight refuses an operator's reboot");
    let automatic = device
        .routes
        .reboot(SENDER)
        .await
        .expect_err("and the automatic one");
    assert!(
        manual.to_string().ends_with(&automatic),
        "manual: {manual}, automatic: {automatic}"
    );
    assert!(automatic.contains("install"), "{automatic}");
    gate.notify_one();
    wait_for_install_status(&device.service, "done").await;
}

/// End to end: an automatic pass driven against a closed
/// gate records **the gate's own words** and reaches nothing behind it.
#[tokio::test]
pub(super) async fn an_automatic_pass_records_the_gates_own_refusal_and_reaches_nothing_behind_it()
{
    use crate::update_auto::SENDER;

    let device = Device::new(&auto_document(false, "window"));
    let (mut driver, cadence) = device.driver();

    // Check and fetch run — neither is gated by the window — and the
    // install is refused by it.
    cadence.advance_past_the_check_interval();
    driver.tick().await;

    let manual = device
        .service
        .request_install(SENDER, &device.descriptor)
        .await
        .expect_err("the window is shut for an operator too");
    let deferred = device.deferral().await;
    assert_eq!(deferred["reason"], "outside-window");
    assert!(
        manual.to_string().ends_with(
            deferred["detail"]
                .as_str()
                .expect("a deferral carries the refusing rule")
        ),
        "manual: {manual}, deferred: {deferred}"
    );
    assert!(
        device.native.lock().expect("native").is_empty(),
        "a refused install must not reach the installer: {:?}",
        device.native.lock().expect("native")
    );
    // The fetch DID run: the window gates installs and only installs.
    assert_eq!(
        device.update_entry("update.lifecycle.deploymentId").await,
        Value::String("e".repeat(64))
    );

    // The operator opens the window; the same driver installs and reboots
    // through the same routes.
    device.rewrite(&auto_document(true, "window"));
    driver.tick().await;
    wait_for_install_status(&device.service, "done").await;
    driver.tick().await;
    assert_eq!(
        *device.power.lock().expect("power"),
        vec!["reboot".to_string()],
        "an open window and an open gate is the only combination that reboots"
    );
}

/// **An automatic path against a closed gate arms no
/// override.**
#[tokio::test]
pub(super) async fn the_automatic_path_against_a_closed_gate_arms_no_override() {
    let device = Device::new(&auto_document(true, "window"));
    let (mut driver, cadence) = device.driver();

    // A pass that installs, so a reboot is owed and the gate is what
    // stands between the driver and it.
    cadence.advance_past_the_check_interval();
    driver.tick().await;
    wait_for_install_status(&device.service, "done").await;
    device
        .service
        .report_health("exporter", "blocking", "mid-transaction")
        .await
        .expect("report");

    for _ in 0..3 {
        driver.tick().await;
    }

    let deferred = device.deferral().await;
    assert_eq!(deferred["reason"], "reboot-gate-closed");
    assert_eq!(
        deferred["attempts"], 3,
        "four automatic attempts were refused, and by what: {deferred}"
    );
    assert!(
        device.power.lock().expect("power").is_empty(),
        "a closed gate is not a gate automation walks through"
    );
    let gate = device.reboot_gate().await;
    assert_eq!(gate["safe"], false);
    assert_eq!(gate["overridden"], false);
    assert!(
        gate["override"].is_null(),
        "the automatic path armed the override: {gate}"
    );

    // The override is a human's judgement, and it is visible in exactly
    // this member when a human makes it — so the assertion above is a
    // measurement and not an empty tree.
    device
        .lifecycle
        .set_reboot_override(":1.7", 120)
        .await
        .expect("an operator may arm it");
    let gate = device.reboot_gate().await;
    assert!(
        gate["override"]["until"].is_string(),
        "an armed override is visible here: {gate}"
    );
    driver.tick().await;
    assert_eq!(
        *device.power.lock().expect("power"),
        vec!["reboot".to_string()],
        "and the reboot the driver deferred goes through on the operator's judgement"
    );
}

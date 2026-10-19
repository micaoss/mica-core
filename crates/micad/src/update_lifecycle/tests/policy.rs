//! Policy refusals, deferrals, the reboot gate and the install window.

use crate::deployment::Status;
use crate::update_codes;
use chrono::{Datelike, Timelike};
use serde_json::json;
use std::sync::atomic::Ordering;

use super::*;

#[tokio::test]
pub(super) async fn every_policy_refusal_is_refused_and_recorded() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Offline: both verbs refused, nothing ever reaches the client.
    let policy = policy_file(
        &dir,
        r#"{"source": {"url": "http://mirror/tuf"}, "network": {"mode": "offline"}}"#,
    );
    let client = MockClient::new(vec![]);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    let refusal = lifecycle.request_check("test").await.expect_err("refused");
    assert!(matches!(refusal, Refusal::Policy(_)), "got {refusal:?}");
    assert!(refusal.message().contains("offline"));
    let refusal = lifecycle.request_fetch("test").await.expect_err("refused");
    assert!(matches!(refusal, Refusal::Policy(_)));
    let recorded = host.last();
    assert!(
        recorded["last_refusal"]
            .as_str()
            .expect("refusal recorded")
            .contains("offline"),
        "got: {recorded}"
    );
    assert_eq!(
        recorded["last_refusal_code"],
        update_codes::REFUSED_NETWORK_OFFLINE,
        "got: {recorded}"
    );
    assert_eq!(recorded["policy"]["networkMode"], "offline");
}

// The deferral half. Every reason the automatic driver mints
// is driven through the recording path an operator polls, and a word from
// outside the vocabulary is driven through the same path to show what
// happens to it.
#[tokio::test]
pub(super) async fn every_deferral_reaches_the_document_as_a_code_and_nothing_else_does() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = policy_file(&dir, r#"{"source": {"url": "http://mirror/tuf"}}"#);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(MockClient::new(vec![]), policy, Arc::clone(&host));

    for reason in [
        update_codes::DEFER_CHECK_REFUSED,
        update_codes::DEFER_NO_NEWER_RELEASE,
        update_codes::DEFER_FETCH_REFUSED,
        update_codes::DEFER_CLOCK_UNTRUSTED,
        update_codes::DEFER_OUTSIDE_WINDOW,
        update_codes::DEFER_DEPLOYMENT_STATUS_UNKNOWN,
        update_codes::DEFER_REBOOT_PENDING,
        update_codes::DEFER_WORKSPACE_UNREADY,
        update_codes::DEFER_RECHECK_FAILED,
        update_codes::DEFER_RECHECK_REFUSED,
        update_codes::DEFER_SUPERSEDED,
        update_codes::DEFER_INSTALL_REFUSED,
        update_codes::DEFER_REBOOT_GATE_CLOSED,
    ] {
        lifecycle.defer(reason, "the refusing rule, in words").await;
        let recorded = host.last();
        assert_eq!(recorded["deferred"]["reason"], reason, "{recorded}");
        assert_eq!(
            recorded["deferred"]["detail"], "the refusing rule, in words",
            "{recorded}"
        );
        assert_eq!(recorded["deferred"]["attempts"], 1, "{reason}: {recorded}");
        lifecycle.resume(None).await;
    }

    // A reason outside the vocabulary. The deferral is still recorded —
    // an invisible refusal is the defect this exists to stop — and
    // its detail, clock and attempt count are untouched. What does not
    // survive is the word: the document says `unknown`, and it does NOT
    // say `no-newer-releases`.
    lifecycle
        .defer("no-newer-releases", "a plausible misspelling")
        .await;
    let recorded = host.last();
    assert_eq!(recorded["deferred"]["reason"], update_codes::UNKNOWN);
    assert_eq!(recorded["deferred"]["detail"], "a plausible misspelling");
    assert_eq!(recorded["deferred"]["attempts"], 1);
    assert!(
        !recorded.to_string().contains("no-newer-releases"),
        "the rejected word must not reach the document by any member: {recorded}"
    );

    // The clamp does not merge two different unknown reasons into one
    // fact by accident of sharing a code: they DO share it, deliberately,
    // which is the information the fallback gives up. `attempts` rising
    // is what says so out loud rather than silently.
    lifecycle
        .defer("another-invention", "and another rule")
        .await;
    let recorded = host.last();
    assert_eq!(recorded["deferred"]["reason"], update_codes::UNKNOWN);
    assert_eq!(recorded["deferred"]["detail"], "and another rule");
    assert_eq!(recorded["deferred"]["attempts"], 2);

    // `resume` names a reason, so it clears by code too: an unmatched
    // word clears nothing rather than clearing the wrong thing.
    lifecycle.resume(Some("another-invention")).await;
    assert_eq!(
        host.last()["deferred"]["reason"],
        update_codes::UNKNOWN,
        "a word that is not the recorded code must not clear it"
    );
    lifecycle.resume(Some(update_codes::UNKNOWN)).await;
    assert!(
        host.last().get("deferred").is_none(),
        "the recorded code clears it"
    );
}

#[tokio::test]
pub(super) async fn metered_mode_refuses_fetch_but_admits_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = policy_file(
        &dir,
        r#"{"source": {"url": "http://mirror/tuf"}, "network": {"mode": "metered"}}"#,
    );
    let client = MockClient::new(vec![ready_probe(), ("check", Ok(no_selection()))]);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    let refusal = lifecycle.request_fetch("test").await.expect_err("refused");
    assert!(refusal.message().contains("metered"));
    assert_eq!(
        host.last()["last_refusal_code"],
        update_codes::REFUSED_NETWORK_METERED
    );
    lifecycle
        .request_check("test")
        .await
        .expect("check admitted");
    let recorded = settled(&host).await;
    assert_eq!(recorded["state"], "idle");
    assert!(recorded.get("available").is_none(), "none selected");
}

#[tokio::test]
pub(super) async fn an_absent_client_is_reported_never_panicked_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = policy_file(&dir, r#"{"source": {"url": "http://mirror/tuf"}}"#);
    let client = MockClient::absent("/usr/bin/mica-deploy is not present on this image");
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    let refusal = lifecycle.request_check("test").await.expect_err("refused");
    assert!(matches!(refusal, Refusal::Unavailable(_)));
    let recorded = host.last();
    assert_eq!(recorded["client"]["available"], false);
    assert!(
        recorded["client"]["reason"]
            .as_str()
            .expect("reason")
            .contains("not present"),
    );
    // `client.available: false` is itself the enumerated fact — a boolean
    // needs no code — but the refusal it produced is on the coded member.
    assert_eq!(
        recorded["last_refusal_code"],
        update_codes::REFUSED_CLIENT_UNAVAILABLE,
        "got: {recorded}"
    );
}

#[tokio::test]
pub(super) async fn a_second_operation_is_refused_while_one_runs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = policy_file(&dir, r#"{"source": {"url": "http://mirror/tuf"}}"#);
    // The sync never answers within the test: an Err after a long sleep
    // would leak; instead gate on a channel-free trick — a script entry
    // that sleeps far longer than the test's second request needs.
    struct SlowClient;
    #[async_trait::async_trait]
    impl UpdateClient for SlowClient {
        fn unavailable(&self) -> Option<String> {
            None
        }
        async fn run(&self, _args: &[String], _timeout: Duration) -> Result<ClientOutput> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            anyhow::bail!("never reached")
        }
    }
    let installing = Arc::new(AtomicBool::new(false));
    let host = TestHost::new();
    let lifecycle = Arc::new(UpdateLifecycle::new(
        Arc::new(SlowClient),
        policy,
        Arc::clone(&host) as Arc<dyn LifecycleHost>,
        Arc::clone(&installing),
        PathBuf::from(DEFAULT_WORKSPACE_ROOT),
    ));
    lifecycle
        .request_check("test")
        .await
        .expect("first accepted");
    let refusal = lifecycle.request_fetch("test").await.expect_err("busy");
    assert!(matches!(refusal, Refusal::Busy(_)));
    assert!(refusal.message().contains("checking"));
    // An install in flight refuses new client operations the same way.
    installing.store(true, Ordering::Release);
    let refusal = lifecycle.request_check("test").await.expect_err("busy");
    assert!(refusal.message().contains("install"));
}

#[tokio::test]
pub(super) async fn the_reboot_gate_blocks_lifts_and_expires() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = policy_file(&dir, "{}");
    let client = MockClient::new(vec![]);
    let host = TestHost::new();
    let (lifecycle, installing) = lifecycle(client, policy, Arc::clone(&host));

    // Open by default.
    assert_eq!(lifecycle.reboot_refusal().await, None);

    // A blocking health report closes it.
    host.set_health(json!({
        "exporter": { "status": "blocking", "detail": "mid-transaction" }
    }));
    let refusal = lifecycle.reboot_refusal().await.expect("closed");
    assert!(refusal.contains("exporter"), "got: {refusal}");
    assert!(refusal.contains("SetRebootOverride"), "got: {refusal}");

    // A bounded override lifts the health block…
    assert!(matches!(
        lifecycle.set_reboot_override("op", 0).await,
        Err(Refusal::Invalid(_))
    ));
    assert!(matches!(
        lifecycle
            .set_reboot_override(
                "op",
                micad_settings::configuration::OVERRIDE_CEILING_SECONDS + 1,
            )
            .await,
        Err(Refusal::Invalid(_))
    ));
    let rendered = lifecycle
        .set_reboot_override(":1.42", 600)
        .await
        .expect("armed");
    assert_eq!(rendered["requestedBy"], ":1.42");
    assert_eq!(lifecycle.reboot_refusal().await, None);
    let recorded = host.last();
    assert_eq!(recorded["reboot_gate"]["safe"], true);
    assert_eq!(recorded["reboot_gate"]["overridden"], true);
    assert!(recorded["reboot_gate"]["override"]["until"].is_string());

    // …but never an install in flight.
    installing.store(true, Ordering::Release);
    let refusal = lifecycle.reboot_refusal().await.expect("install blocks");
    assert!(refusal.contains("install"), "got: {refusal}");
    installing.store(false, Ordering::Release);

    // An expired override is pruned, closing the gate again.
    lifecycle.machine.lock().await.override_record = Some(OverrideRecord {
        expires_at: Utc::now() - chrono::Duration::seconds(1),
        requested_by: "op".to_string(),
    });
    assert!(lifecycle.reboot_refusal().await.is_some());
}

#[tokio::test]
pub(super) async fn native_status_drives_boot_phase_and_consumes_an_installed_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(
        MockClient::new(vec![]),
        policy_file(&dir, "{}"),
        Arc::clone(&host),
    );
    let status = Status::parse(&crate::deployment::tests::fixture().to_string()).unwrap();
    lifecycle.refresh(&status).await;
    assert_eq!(host.last()["state"], "succeeded");
    let mut pending = status;
    pending.state.candidate = Some("e".repeat(64));
    lifecycle.machine.lock().await.descriptor = Some(staged_path());
    lifecycle.installed(&pending).await;
    assert_eq!(host.last()["state"], "reboot-required");
    assert!(lifecycle.staged_descriptor().await.is_none());
}

#[tokio::test]
pub(super) async fn an_install_refusal_follows_the_maintenance_window() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A window that can never contain "now": zero-width windows do not
    // exist (equal start/end wraps to 24 h), so pin an impossible day
    // combination instead — Monday 00:00-00:01 leaves 10079 refused
    // minutes a week; rather than race the clock, assert both sides
    // through the pure function and only the wiring here.
    let policy = policy_file(
        &dir,
        r#"{"maintenance": {"windows": [{"days": ["mon"], "start": "00:00", "end": "00:01"}]}}"#,
    );
    let client = MockClient::new(vec![]);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    let now = Utc::now();
    let inside = now.weekday().num_days_from_monday() == 0 && now.hour() == 0 && now.minute() == 0;
    let refusal = lifecycle.install_refusal();
    if inside {
        assert_eq!(refusal, None);
    } else {
        assert!(refusal.expect("outside the window").contains("maintenance"),);
    }
}

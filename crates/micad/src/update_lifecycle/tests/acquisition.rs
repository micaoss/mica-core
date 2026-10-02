//! Checking, fetching, importing and discarding.

use crate::update_codes;
use serde_json::json;

use super::*;

#[tokio::test]
pub(super) async fn a_check_probes_then_checks_the_signed_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let policy = policy_file(&dir, r#"{"source":{"url":"https://updates.example"}}"#);
    let client = MockClient::new(vec![ready_probe(), ("check", Ok(selection_output()))]);
    let calls = Arc::clone(&client.calls);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    lifecycle.request_check("test").await.unwrap();
    let recorded = settled(&host).await;
    assert_eq!(recorded["state"], "idle");
    assert_eq!(recorded["available"]["version"], "1.1.0");
    assert_eq!(recorded["workspace"]["freeBytes"], 1_000_000_000u64);
    assert!(recorded["last_check"].is_string());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0], ["--max-bytes", "500000000", "probe"]);
    assert_eq!(
        calls[1],
        [
            "--max-bytes",
            "500000000",
            "check",
            "--source",
            "https://updates.example"
        ]
    );
}

#[tokio::test]
pub(super) async fn a_fetch_records_the_verified_descriptor_as_ready() {
    let dir = tempfile::tempdir().unwrap();
    let policy = policy_file(&dir, r#"{"source":{"url":"https://updates.example"}}"#);
    let client = MockClient::new(vec![
        ready_probe(),
        ("fetch", Ok(fetch_output(&staged_path()))),
    ]);
    let calls = Arc::clone(&client.calls);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    lifecycle.request_fetch("test").await.unwrap();
    let recorded = settled(&host).await;
    assert_eq!(recorded["state"], "ready");
    assert_eq!(recorded["deploymentId"], "a".repeat(64));
    assert_eq!(
        calls.lock().unwrap()[1],
        [
            "--max-bytes",
            "500000000",
            "fetch",
            "--source",
            "https://updates.example"
        ]
    );
}

/// An imported archive stages exactly what a fetch stages, and the client
/// is asked for `import` with the file and nothing else -- no source: there
/// is no server in this path.
#[tokio::test]
pub(super) async fn an_import_stages_the_archive_the_way_a_fetch_stages_a_download() {
    let dir = tempfile::tempdir().unwrap();
    // No source configured at all, which would refuse a fetch outright.
    let policy = policy_file(&dir, r#"{"policy":"off"}"#);
    let client = MockClient::new(vec![
        ready_probe(),
        ("import", Ok(fetch_output(&staged_path()))),
    ]);
    let calls = Arc::clone(&client.calls);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    let archive = dir.path().join("release.micaupd");
    std::fs::write(&archive, b"MICAUPD1").unwrap();

    let settled_outcome = lifecycle
        .import_now("test", &archive)
        .await
        .expect("an upload is not refused by the network policy");

    assert!(
        matches!(settled_outcome, Settled::Done(_)),
        "{settled_outcome:?}"
    );
    let recorded = settled(&host).await;
    assert_eq!(recorded["state"], "ready");
    assert_eq!(recorded["deploymentId"], "a".repeat(64));
    assert_eq!(
        calls.lock().unwrap()[1],
        [
            "--max-bytes",
            "500000000",
            "import",
            archive.to_string_lossy().as_ref(),
        ]
    );
}

#[tokio::test]
pub(super) async fn a_fetch_path_outside_verified_is_never_recorded_as_ready() {
    for printed in [
        "/mica/updates/downloads/x.json.part",
        "/mica/updates/downloads/x.json",
        "/mica/updates/verified/x.json.part",
        "/tmp/outside/x.json",
    ] {
        let dir = tempfile::tempdir().expect("tempdir");
        let policy = policy_file(&dir, r#"{"source": {"url": "http://mirror/tuf"}}"#);
        let client = MockClient::new(vec![ready_probe(), ("fetch", Ok(fetch_output(printed)))]);
        let host = TestHost::new();
        let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
        lifecycle.request_fetch("test").await.expect("accepted");
        let recorded = settled(&host).await;
        assert_eq!(recorded["state"], "failed", "{printed}: {recorded}");
        assert!(
            recorded.get("deploymentId").is_none(),
            "{printed}: {recorded}"
        );
        assert!(
            recorded["reason"]
                .as_str()
                .expect("reason")
                .contains("verified"),
            "{printed}: {recorded}"
        );
        assert_eq!(
            recorded["code"],
            update_codes::UNVERIFIED_DEPLOYMENT_PATH,
            "{printed}: {recorded}"
        );
    }
}

#[tokio::test]
pub(super) async fn an_unready_workspace_refuses_acquisition_until_a_probe_passes() {
    let dir = tempfile::tempdir().unwrap();
    let policy = policy_file(&dir, r#"{"source":{"url":"https://updates.example"}}"#);
    let client = MockClient::new(vec![
        ("probe", Ok(output(1, "", "DATA is read-only"))),
        ready_probe(),
        ("check", Ok(no_selection())),
    ]);
    let calls = Arc::clone(&client.calls);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    lifecycle.request_check("test").await.unwrap();
    let recorded = settled(&host).await;
    assert_eq!(recorded["state"], "update-unavailable");
    assert_eq!(recorded["code"], update_codes::WORKSPACE_PROBE_FAILED);
    assert_eq!(recorded["workspace"]["detail"], "DATA is read-only");
    assert_eq!(calls.lock().unwrap().len(), 1);
    lifecycle.request_check("test").await.unwrap();
    let recorded = settled(&host).await;
    assert_eq!(recorded["state"], "idle");
    assert_eq!(recorded["workspace"]["status"], "ready");
    assert!(recorded.get("code").is_none());
    assert_eq!(calls.lock().unwrap().len(), 3);
}

#[tokio::test]
pub(super) async fn a_fetch_failure_never_records_a_descriptor_as_ready() {
    let dir = tempfile::tempdir().unwrap();
    let policy = policy_file(&dir, r#"{"source":{"url":"https://updates.example"}}"#);
    let client = MockClient::new(vec![
        ready_probe(),
        ("fetch", Ok(output(1, "", "component digest mismatch"))),
    ]);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    lifecycle.request_fetch("test").await.unwrap();
    let recorded = settled(&host).await;
    assert_eq!(recorded["state"], "failed");
    assert_eq!(recorded["code"], update_codes::CLIENT_EXIT_FAILURE);
    assert!(recorded.get("deploymentId").is_none());
}

#[test]
pub(super) fn only_a_verified_regular_file_is_installable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join("updates");
    let verified = root.join("verified");
    std::fs::create_dir_all(&verified).expect("verified/");
    std::fs::create_dir_all(root.join("downloads")).expect("downloads/");
    let lifecycle = UpdateLifecycle::new(
        Arc::new(MockClient::new(vec![])),
        PolicyStore::defaults(),
        TestHost::new(),
        Arc::new(AtomicBool::new(false)),
        root.clone(),
    );
    assert_eq!(lifecycle.verified_dir(), verified);

    let good = verified.join(format!("{}.json", "a".repeat(64)));
    std::fs::write(&good, b"verified bytes").expect("seed");
    assert_eq!(lifecycle.installable(&good), Ok(()));

    let refused = |path: &Path, needle: &str| {
        let err = lifecycle.installable(path).expect_err(needle);
        assert!(err.contains(needle), "{}: {err}", path.display());
    };
    let part = verified.join("invalid.json.part");
    std::fs::write(&part, b"partial").expect("seed");
    refused(&part, "not a deployment descriptor");
    let in_downloads = root.join("downloads").join("invalid.json");
    std::fs::write(&in_downloads, b"unverified").expect("seed");
    refused(&in_downloads, "not inside");
    let elsewhere = dir.path().join("invalid.json");
    std::fs::write(&elsewhere, b"unverified").expect("seed");
    refused(&elsewhere, "not inside");
    let link = verified.join(format!("{}.json", "b".repeat(64)));
    std::os::unix::fs::symlink(&elsewhere, &link).expect("symlink");
    refused(&link, "not a regular file");
    refused(
        &verified.join(format!("{}.json", "e".repeat(64))),
        "No such file",
    );
    refused(Path::new("relative.json"), "not inside");
}

#[tokio::test]
pub(super) async fn a_failed_check_is_a_failed_state_with_its_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = policy_file(&dir, r#"{"source": {"url": "http://mirror/tuf"}}"#);
    let client = MockClient::new(vec![
        ready_probe(),
        ("check", Ok(output(1, "", "connection refused\n"))),
    ]);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));

    lifecycle.request_check("test").await.expect("accepted");
    let recorded = settled(&host).await;
    assert_eq!(recorded["state"], "failed");
    assert!(
        recorded["reason"]
            .as_str()
            .expect("reason")
            .contains("connection refused"),
        "got: {recorded}"
    );
    // The sentence stays for the operator, and the class is
    // beside it for everything that is not one.
    assert_eq!(recorded["code"], update_codes::CLIENT_EXIT_FAILURE);
}

// The three remaining `failed` codes, each driven to the recorded
// document rather than asserted at the parser it is minted in.
#[tokio::test]
pub(super) async fn a_client_that_cannot_run_and_one_that_will_not_speak_are_two_codes() {
    for (result, code) in [
        (
            Err("spawn failed".into()),
            update_codes::CLIENT_SPAWN_FAILED,
        ),
        (
            Ok(output(0, "hello", "")),
            update_codes::CLIENT_OUTPUT_UNPARSEABLE,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let policy = policy_file(&dir, r#"{"source":{"url":"https://updates.example"}}"#);
        let host = TestHost::new();
        let (lifecycle, _) = lifecycle(
            MockClient::new(vec![ready_probe(), ("check", result)]),
            policy,
            Arc::clone(&host),
        );
        lifecycle.request_check("test").await.unwrap();
        let recorded = settled(&host).await;
        assert_eq!(recorded["state"], "failed");
        assert_eq!(recorded["code"], code);
    }
}

// The two members of the `last_refusal` set that are not refusals, driven
// through the calls that write them. Both share the coded member with the
// refusals, so both need a code; neither would be reachable from a
// refusal test.
#[tokio::test]
pub(super) async fn discarding_a_staged_deployment_uses_native_workspace_cleanup() {
    let dir = tempfile::tempdir().unwrap();
    let policy = policy_file(&dir, r#"{"source":{"url":"https://updates.example"}}"#);
    let client = MockClient::new(vec![
        ready_probe(),
        ("fetch", Ok(fetch_output(&staged_path()))),
        ("discard", Ok(output(0, "{\"removed\":3}", ""))),
    ]);
    let calls = Arc::clone(&client.calls);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(client, policy, Arc::clone(&host));
    lifecycle.fetch_now("test").await.unwrap();
    lifecycle.discard_descriptor("release withdrawn").await;
    assert_eq!(calls.lock().unwrap().last().unwrap(), &["discard"]);
    assert!(lifecycle.staged_descriptor().await.is_none());
    assert_eq!(
        host.last()["last_refusal_code"],
        update_codes::NOTE_DEPLOYMENT_DISCARDED
    );
}

/// The `since`/`attempts`, the half a code vocabulary does not
/// settle: an unbroken refusal must keep its first instant and count its
/// attempts, and a DIFFERENT refusal must not inherit the clock of the one
/// it replaced.
#[tokio::test]
pub(super) async fn a_repeated_deferral_counts_its_attempts_and_a_changed_reason_starts_over() {
    let dir = tempfile::tempdir().expect("tempdir");
    let policy = policy_file(&dir, r#"{"source": {"url": "http://mirror/tuf"}}"#);
    let host = TestHost::new();
    let (lifecycle, _) = lifecycle(MockClient::new(vec![]), policy, Arc::clone(&host));

    lifecycle
        .defer(
            update_codes::DEFER_REBOOT_GATE_CLOSED,
            "exporter reports blocking",
        )
        .await;
    let first = host.last();
    assert_eq!(first["deferred"]["attempts"], 1);
    assert_eq!(first["deferred"]["since"], first["deferred"]["at"]);

    lifecycle
        .defer(
            update_codes::DEFER_REBOOT_GATE_CLOSED,
            "exporter still reports blocking",
        )
        .await;
    let again = host.last();
    assert_eq!(
        again["deferred"]["attempts"], 2,
        "the same reason again extends the fact rather than replacing it"
    );
    assert_eq!(again["deferred"]["since"], first["deferred"]["since"]);
    assert_eq!(
        again["deferred"]["detail"], "exporter still reports blocking",
        "the newest wording of the refusing rule is the one an operator reads"
    );
    assert!(again["deferred"]["waitedSeconds"].is_number());

    // A different reason is a different refusal, and inheriting the first
    // one's clock would report a wait that never happened. No `resume`
    // between the two: this is the replacement path, not the cleared one.
    lifecycle
        .defer(
            update_codes::DEFER_OUTSIDE_WINDOW,
            "outside every configured maintenance window",
        )
        .await;
    let changed = host.last();
    assert_eq!(
        changed["deferred"]["reason"],
        update_codes::DEFER_OUTSIDE_WINDOW
    );
    assert_eq!(changed["deferred"]["attempts"], 1);
    assert_eq!(changed["deferred"]["since"], changed["deferred"]["at"]);
}
#[test]
pub(super) fn native_acquisition_json_selects_and_stages_only_a_deployment_descriptor() {
    let id = "a".repeat(64);
    let checked = ClientOutput {
        code: Some(0),
        stderr: String::new(),
        stdout: json!({
            "revision":1,"selected":{"deploymentId":id,"deployment":{"version":"1.0"}}
        })
        .to_string(),
    };
    assert_eq!(
        parse_check(&checked).unwrap(),
        CheckOutcome::Selected(Available {
            deployment_id: id.to_string(),
            version: "1.0".into(),
        })
    );
    let none = ClientOutput {
        code: Some(0),
        stderr: String::new(),
        stdout: "{\"revision\":2,\"selected\":null}".into(),
    };
    assert_eq!(parse_check(&none).unwrap(), CheckOutcome::NoneCompatible);
    let ready = ClientOutput { code:Some(0),stderr:String::new(),stdout:json!({"id":id,
        "path":format!("/mica/updates/verified/{id}.json"),"objects":"/mica/updates/verified/objects","version":"1.0","generation":3}).to_string() };
    assert_eq!(
        parse_fetch(&ready, Path::new("/mica/updates/verified")).unwrap(),
        FetchOutcome::Staged(format!("/mica/updates/verified/{id}.json"))
    );
    let probe = ClientOutput {
        code: Some(0),
        stderr: String::new(),
        stdout: "{\"status\":\"ready\",\"freeBytes\":200000000}".into(),
    };
    assert!(matches!(
        parse_probe(&probe).unwrap(),
        ProbeOutcome::Ready(_)
    ));
}

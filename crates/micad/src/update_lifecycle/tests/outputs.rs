//! Parsing the deployment client's answers.

use crate::update_codes;
use serde_json::json;

use super::*;

#[test]
pub(super) fn check_output_parses_native_json_and_reports_failure_codes() {
    assert!(matches!(
        parse_check(&selection_output()),
        Ok(CheckOutcome::Selected(_))
    ));
    assert_eq!(
        parse_check(&no_selection()),
        Ok(CheckOutcome::NoneCompatible)
    );
    assert_eq!(
        parse_check(&output(1, "", "catalog is expired"))
            .unwrap_err()
            .code,
        update_codes::CLIENT_EXIT_FAILURE
    );
    for malformed in ["selected only-a-name", "{}", r#"{"selected":{}}"#] {
        assert_eq!(
            parse_check(&output(0, malformed, "")).unwrap_err().code,
            update_codes::CLIENT_OUTPUT_UNPARSEABLE
        );
    }
    assert_eq!(
        parse_check(&output_signalled("")).unwrap_err().code,
        update_codes::CLIENT_SPAWN_FAILED
    );
}

#[test]
pub(super) fn fetch_output_is_an_identified_verified_descriptor_or_nothing() {
    let verified = Path::new("/mica/updates/verified");
    assert_eq!(
        parse_fetch(&fetch_output(&staged_path()), verified),
        Ok(FetchOutcome::Staged(staged_path()))
    );
    assert_eq!(
        parse_fetch(&output(0, "null", ""), verified),
        Ok(FetchOutcome::NoneCompatible)
    );
    for outside in [
        "/mica/updates/downloads/a.json",
        "/mica/updates/verified/a.json",
        "/tmp/a.json",
        "/mica/updates/verified/a.json.partial",
    ] {
        assert_eq!(
            parse_fetch(&fetch_output(outside), verified)
                .unwrap_err()
                .code,
            update_codes::UNVERIFIED_DEPLOYMENT_PATH
        );
    }
    let mut wrong: Value = serde_json::from_str(&fetch_output(&staged_path()).stdout).unwrap();
    wrong["objects"] = json!("/tmp/objects");
    assert!(parse_fetch(&output(0, &wrong.to_string(), ""), verified).is_err());
    assert_eq!(
        parse_fetch(&output(1, "", "budget exceeded"), verified)
            .unwrap_err()
            .code,
        update_codes::CLIENT_EXIT_FAILURE
    );
}

#[test]
pub(super) fn probe_output_requires_ready_json_and_preserves_native_refusals() {
    let (_, ready) = ready_probe();
    let ProbeOutcome::Ready(report) = parse_probe(&ready.unwrap()).unwrap() else {
        panic!("ready expected")
    };
    assert_eq!(report["freeBytes"], 1_000_000_000u64);
    for detail in [
        "DATA is not mounted",
        "read-only filesystem",
        "insufficient workspace capacity",
    ] {
        let ProbeOutcome::Unready(unready) = parse_probe(&output(1, "", detail)).unwrap() else {
            panic!("refusal expected")
        };
        assert_eq!(unready.kind, update_codes::WORKSPACE_PROBE_FAILED);
        assert_eq!(unready.detail, detail);
    }
    for malformed in ["ready free=123", "{}", r#"{"status":"unknown"}"#] {
        assert_eq!(
            parse_probe(&output(0, malformed, "")).unwrap_err().code,
            update_codes::CLIENT_OUTPUT_UNPARSEABLE
        );
    }
}

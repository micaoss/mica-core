//! The board sections of the shared contract cases, as a device reads them.
//!
//! `cases.json` `boardPolicies` holds each concrete board's signed `board`
//! section: the tests take their geometry from there rather than from a table
//! of their own, so the device and its tests read one statement of each board.
#![allow(dead_code)]

use mica_deploy::board::BoardFacts;
use mica_deploy::components::BootIdentity;
use mica_deploy::fit_env::FitLayout;
use serde_json::Value;

const CASES: &str = include_str!("../component-contracts/cases.json");

fn entry(board: &str) -> Value {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    let entry = cases["boardPolicies"][board].clone();
    assert!(entry.is_object(), "no board policy for {board}");
    entry
}

/// Every board the cases carry a policy for.
pub fn names() -> Vec<String> {
    let cases: Value = serde_json::from_str(CASES).unwrap();
    cases["boardPolicies"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect()
}

/// The `board` section of `board`'s signed boot policy.
pub fn policy(board: &str) -> BoardFacts {
    serde_json::from_value(entry(board)["board"].clone()).unwrap()
}

/// The architecture `board`'s policy is written for.
pub fn arch(board: &str) -> String {
    entry(board)["arch"].as_str().unwrap().to_owned()
}

/// The FIT record geometry of `board`.
pub fn layout(board: &str) -> FitLayout {
    policy(board).records.unwrap()
}

/// The identity a device running `board` boots with; the kernel fields are
/// placeholders for the checks that do not read them.
pub fn identity(board: &str) -> BootIdentity {
    BootIdentity {
        board: board.to_owned(),
        arch: arch(board),
        kernel_build_id: "d".repeat(64),
        kernel_release: "6.12.107".to_owned(),
        support_id: "0".repeat(64),
    }
}

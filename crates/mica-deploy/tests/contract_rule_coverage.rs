//! SET EQUALITY ON RULES, NOT ON FILES: every refusal `components.rs` can
//! produce is either triggered here or named as exercised elsewhere, and the
//! two lists together must account for all of them.
//!
//! The measurement that produced this file: instrumenting the reader and
//! running the whole suite showed 26 of 35 rules firing, so nine were reachable
//! by no test at all. A rule nothing fires is a rule nobody has checked the
//! wording, the ordering or the existence of -- and `float` is how that is
//! usually discovered, one case at a time, by accident: it is named for the
//! integer bound and is refused by serde before that bound is consulted.
//!
//! The rule list is read out of the source at test time, so a refusal added to
//! `components.rs` fails this test until it is triggered or named.

use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::components::{
    MAX_DEPLOYMENT_BYTES, authenticate_deployment, authenticate_payload, parse_deployment,
};
use serde_json::{Value, json};

const SOURCE: &str = include_str!("../src/components.rs");
const PAYLOAD: &str = include_str!("component-contracts/deployment.json");

/// Which reader a trigger is handed to; a heuristic on the input would send
/// `{}` to the descriptor parser and report the wrong rule.
#[derive(Clone, Copy)]
enum Entry {
    Descriptor,
    Envelope,
}

/// Every rule this file triggers, with the input that triggers it.
fn triggered() -> Vec<(&'static str, Entry, String)> {
    let descriptor: Value = serde_json::from_str(PAYLOAD).unwrap();
    let mutate = |pointer: &str, value: Value| {
        let mut d = descriptor.clone();
        *d.pointer_mut(pointer).unwrap() = value;
        serde_json::to_string(&d).unwrap()
    };
    let envelope = |payload: &str, signature: &str| {
        format!(
            "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{payload}\",\"signature\":\"{signature}\"}}",
            key_id(&[0u8; 32])
        )
    };
    vec![
        // The kernel names another architecture than the deployment.
        (
            "component target mismatch",
            Entry::Descriptor,
            mutate("/kernel/arch", json!("arm64")),
        ),
        ("invalid JSON", Entry::Descriptor, "not json".to_string()),
        // No board table: an architecture or boot format the device family
        // does not run at all is the parser's to refuse, whatever the board.
        (
            "unsupported architecture",
            Entry::Descriptor,
            mutate("/arch", json!("riscv64")),
        ),
        (
            "unsupported boot format",
            Entry::Descriptor,
            mutate("/kernel/boot/format", json!("efi")),
        ),
        (
            "envelope too large",
            Entry::Envelope,
            format!("{}{}", envelope("AA==", "AA=="), " ".repeat(24576)),
        ),
        ("invalid envelope", Entry::Envelope, "{}".to_string()),
        (
            "invalid base64",
            Entry::Envelope,
            envelope("not base64!!", "AA=="),
        ),
    ]
}

/// The digest of a key, which an envelope must name for the reader to reach
/// anything after the trust check.
fn key_id(key: &[u8; 32]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, key))
}

/// Every refusal the reader can produce, read out of its source: the literal
/// inside a `ContractError("...")`, and the message argument of a `require`,
/// whether it is written inline or on its own line.
fn rules_in_source() -> Vec<String> {
    let mut rules = Vec::new();
    let bytes = SOURCE.as_bytes();
    let mut i = 0;
    while let Some(start) = SOURCE[i..].find('"') {
        let open = i + start;
        let Some(len) = SOURCE[open + 1..].find('"') else {
            break;
        };
        let literal = &SOURCE[open + 1..open + 1 + len];
        // What precedes the literal, ignoring whitespace: `(` for
        // ContractError("..."), `,` for a require message.
        let before = SOURCE[..open].trim_end().chars().last().unwrap_or(' ');
        // And what follows it: the argument list ends here.
        let after = SOURCE[open + 1 + len + 1..]
            .trim_start()
            .chars()
            .next()
            .unwrap_or(' ');
        let is_message = (before == '(' || before == ',')
            && (after == ')' || after == ',')
            && literal.contains(' ')
            && !literal.contains('{');
        if is_message && SOURCE[..open].contains("fn ") {
            rules.push(literal.to_string());
        }
        i = open + 1 + len + 1;
        let _ = bytes;
    }
    rules.sort();
    rules.dedup();
    rules
}

/// Rules this file does not trigger, each with where it is exercised.
///
/// `invalid support metadata` is the one entry that is not a test name:
/// `serde_json::to_value` of a struct with string keys and finite numbers
/// cannot fail, so no input reaches that arm. It is kept because removing it
/// would silently widen the `?` above it, and it is recorded here rather than
/// pretended to be covered.
const ELSEWHERE: [(&str, &str); 29] = [
    (
        "artifact length or digest mismatch",
        "deployments.rs, verify_file over a truncated object",
    ),
    (
        "board/architecture mismatch",
        "components.rs a_deployment_for_another_board_is_refused_by_the_device, the device's admit",
    ),
    (
        "component identity mismatch",
        "cases.json kernel-substitution, root-substitution, support-substitution",
    ),
    (
        "deployment too large",
        "components.rs noncanonical_duplicate_and_oversized_payloads_fail",
    ),
    (
        "expected object",
        "components.rs component_id over a non-object",
    ),
    (
        "hash tree must immediately follow data",
        "cases.json overlap, unaligned",
    ),
    (
        "image length does not match verity tree",
        "cases.json truncated-tree, extra-image-data",
    ),
    (
        "invalid digest",
        "cases.json traversal, uppercase-hash, invalid-salt, trailing-newline-hash",
    ),
    (
        "invalid identifier",
        "cases.json unsafe-release, unsafe-version, invalid-product, trailing-newline-name",
    ),
    (
        "invalid integer",
        "cases.json unsafe-integer, zero-generation, zero-blocks, zero-signature, large-signature",
    ),
    (
        "invalid product in the product file",
        "contract_cases.rs the_product_file_names_the_device_product",
    ),
    (
        "invalid signature length",
        "contract_protocol.rs, envelope with a short signature",
    ),
    (
        "invalid support metadata",
        "UNREACHABLE: serde_json::to_value of this struct cannot fail",
    ),
    (
        "invalid trust set",
        "components.rs, an empty and a nine-key trust set",
    ),
    (
        "metadata signature rejected",
        "components.rs signed_schema_substitution_and_tampering_fail",
    ),
    (
        "noncanonical base64",
        "UNREACHABLE: the STANDARD engine refuses a noncanonical encoding outright, so the re-encode comparison after it can never disagree",
    ),
    (
        "noncanonical envelope",
        "contract_protocol.rs, the fixture's sorted envelope",
    ),
    (
        "noncanonical or duplicate JSON fields",
        "components.rs noncanonical_duplicate_and_oversized_payloads_fail",
    ),
    (
        "payload too large",
        "contract_protocol.rs, authenticate_payload with a small limit",
    ),
    (
        "product file names more than one product",
        "contract_cases.rs the_product_file_names_the_device_product",
    ),
    (
        "product file names no product",
        "contract_cases.rs the_product_file_names_the_device_product",
    ),
    (
        "running kernel/support mismatch",
        "components.rs existing_server_signature_verifies_and_is_bound_to_running_kernel",
    ),
    (
        "unknown, missing or invalid fields",
        "cases.json unknown-field, float, free-verity-options, rootfs-version",
    ),
    (
        "unsupported deployment schema or DATA policy",
        "cases.json wrong-schema, unsupported-policy",
    ),
    (
        "unsupported verity geometry",
        "cases.json verity-version, algorithm, block-size, hash-block-size",
    ),
    (
        "untrusted metadata key",
        "components.rs signed_schema_substitution_and_tampering_fail",
    ),
    (
        "wrong boot format",
        "components.rs a_deployment_for_another_board_is_refused_by_the_device, the device's admit",
    ),
    (
        "wrong component schema",
        "contract_protocol.rs schema_vocabulary",
    ),
    (
        "wrong envelope schema",
        "contract_protocol.rs schema_vocabulary",
    ),
];

#[test]
fn every_triggered_rule_is_the_one_that_fires() {
    for (rule, entry, input) in triggered() {
        let error = match entry {
            Entry::Envelope => authenticate_deployment(input.as_bytes(), &[[0u8; 32]]).err(),
            Entry::Descriptor => parse_deployment(input.as_bytes()).err(),
        }
        .unwrap_or_else(|| panic!("{rule}: the input was accepted"))
        .to_string();
        assert!(error.contains(rule), "expected {rule:?}, got {error:?}");
    }
}

#[test]
fn the_remaining_rules_are_reachable_where_they_are_named() {
    // The three that are cheap to prove here rather than assert about elsewhere.
    let big = "A".repeat(MAX_DEPLOYMENT_BYTES + 1);
    let envelope = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        key_id(&[0u8; 32]),
        STANDARD.encode(&big),
        STANDARD.encode([0u8; 64])
    );
    let error = authenticate_payload(envelope.as_bytes(), &[[0u8; 32]], MAX_DEPLOYMENT_BYTES)
        .expect_err("an oversized payload")
        .to_string();
    assert!(error.contains("payload too large"), "{error}");

    let short = format!(
        "{{\"schema\":\"mica/update-envelope/v1\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        key_id(&[0u8; 32]),
        STANDARD.encode("{}"),
        STANDARD.encode([0u8; 8])
    );
    let error = authenticate_payload(short.as_bytes(), &[[0u8; 32]], MAX_DEPLOYMENT_BYTES)
        .expect_err("a short signature")
        .to_string();
    assert!(error.contains("invalid signature length"), "{error}");

    let error = authenticate_payload(short.as_bytes(), &[], MAX_DEPLOYMENT_BYTES)
        .expect_err("an empty trust set")
        .to_string();
    assert!(error.contains("invalid trust set"), "{error}");
}

/// THE SUBTRACTION, ENFORCED: producible rules minus triggered minus named is
/// empty, and nothing is named that the reader cannot produce.
#[test]
fn every_rule_is_accounted_for() {
    let produced = rules_in_source();
    let triggered: Vec<&str> = triggered().into_iter().map(|(rule, _, _)| rule).collect();
    let named: Vec<&str> = ELSEWHERE.iter().map(|(rule, _)| *rule).collect();

    let unaccounted: Vec<&String> = produced
        .iter()
        .filter(|rule| !triggered.contains(&rule.as_str()) && !named.contains(&rule.as_str()))
        .collect();
    assert!(
        unaccounted.is_empty(),
        "refusals with no trigger and no named home: {unaccounted:?}"
    );

    let stale: Vec<&str> = triggered
        .iter()
        .chain(named.iter())
        .filter(|rule| !produced.iter().any(|p| p == *rule))
        .copied()
        .collect();
    assert!(
        stale.is_empty(),
        "rules named here that components.rs no longer produces: {stale:?}"
    );
}

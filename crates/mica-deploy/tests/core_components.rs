//! `mica/deployment/v1`: core components beside the kernel and the root.
// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use mica_deploy::components::{component_id, parse_deployment};
use serde_json::{Value, json};

const PAYLOAD: &str = include_str!("component-contracts/deployment.json");

/// A core component record for `package`, its identity computed.
fn core(package: &str, version: &str, needs: Value, root: Value) -> Value {
    let golden: Value = serde_json::from_str(PAYLOAD).unwrap();
    let mut core = json!({
        "schema": "mica/core/v1",
        "id": "",
        "arch": golden["arch"],
        "package": package,
        "version": version,
        "features": [package],
        "needs": needs,
        "root": root,
        "content": golden["rootfs"]["content"],
    });
    core["id"] = json!(component_id(&core).unwrap());
    core
}

/// The golden deployment on a root of `level`, carrying `cores`.
fn deployment(level: u64, cores: Vec<Value>) -> Value {
    let mut d: Value = serde_json::from_str(PAYLOAD).unwrap();
    d["rootfs"]["interfaceLevel"] = json!(level);
    d["rootfs"]["id"] = json!(component_id(&d["rootfs"]).unwrap());
    if !cores.is_empty() {
        d["core"] = Value::Array(cores);
    }
    d
}

fn parse(value: &Value) -> Result<mica_deploy::components::Deployment, String> {
    parse_deployment(&serde_json::to_vec(value).unwrap()).map_err(|err| err.to_string())
}

fn micad() -> Value {
    core("micad", "0.0.1", json!([]), json!({ "min": 1 }))
}

fn console() -> Value {
    core(
        "mica-apid-ui",
        "0.0.1",
        json!([{ "package": "micad", "min": "0.0.1", "max": "0.0.1" }]),
        json!({ "min": 1, "max": 2 }),
    )
}

#[test]
fn a_deployment_carries_core_components_and_names_their_objects() {
    let d = parse(&deployment(1, vec![console(), micad()])).unwrap();
    assert_eq!(d.rootfs.level(), 1);
    let paths = d.paths().unwrap();
    let packages: Vec<_> = paths.core.iter().map(|c| c.package.as_str()).collect();
    assert_eq!(packages, ["mica-apid-ui", "micad"]);
    assert_eq!(
        paths.core[1].image,
        format!("cores/{}/core.img", d.core[1].id)
    );
    assert_eq!(
        paths.core[1].signature,
        format!("cores/{}/core.roothash.p7s", d.core[1].id)
    );
    // A deployment without core components omits them.
    assert!(parse(&deployment(1, vec![])).unwrap().core.is_empty());
}

#[test]
fn the_golden_deployment_has_no_core_components_and_states_its_level() {
    let d = parse_deployment(PAYLOAD.as_bytes()).unwrap();
    assert!(d.core.is_empty());
    assert_eq!(d.rootfs.level(), 1);
    assert!(d.paths().unwrap().core.is_empty());
}

/// `record` with `field` replaced and its identity recomputed.
fn with(record: &Value, field: &str, value: Value) -> Value {
    let mut changed = record.clone();
    changed[field] = value;
    changed["id"] = json!(component_id(&changed).unwrap());
    changed
}

#[test]
fn a_core_component_that_does_not_fit_its_root_or_its_neighbours_is_refused() {
    let refused = |value: Value, rule: &str| {
        let err = parse(&value).expect_err(rule);
        assert!(err.contains(rule), "expected `{rule}`, got: {err}");
    };
    let level = "a core component does not run on this root's interface level";
    refused(
        deployment(1, vec![with(&micad(), "root", json!({ "min": 2 }))]),
        level,
    );
    refused(deployment(3, vec![console(), micad()]), level);
    refused(
        deployment(1, vec![console()]),
        "a core component's need is not in the deployment",
    );
    refused(
        deployment(
            1,
            vec![console(), with(&micad(), "version", json!("0.0.2"))],
        ),
        "a core component's need is outside its version range",
    );
    let unsorted = "core components not unique and sorted by package";
    refused(deployment(1, vec![micad(), console()]), unsorted);
    refused(deployment(1, vec![micad(), micad()]), unsorted);
    let many: Vec<Value> = (0..17)
        .map(|n| core(&format!("p{n:02}"), "1", json!([]), json!({ "min": 0 })))
        .collect();
    refused(deployment(1, many), "too many core components");

    for (field, value, rule) in [
        ("schema", json!("mica/core/v2"), "wrong component schema"),
        ("arch", json!("riscv64"), "component target mismatch"),
        ("version", json!("1.x"), "invalid version"),
        ("features", json!([]), "invalid core features"),
        (
            "root",
            json!({ "min": 2, "max": 1 }),
            "empty root interface range",
        ),
        (
            "needs",
            json!([{ "package": "micad", "min": "1" }]),
            "a core component needs itself",
        ),
        (
            "needs",
            json!([{ "package": "other", "min": "2", "max": "1" }]),
            "empty version range",
        ),
        (
            "needs",
            Value::Array(
                (0..17)
                    .map(|n| json!({ "package": format!("p{n}"), "min": "1" }))
                    .collect(),
            ),
            "invalid core needs",
        ),
    ] {
        refused(deployment(1, vec![with(&micad(), field, value)]), rule);
    }
    let mut stolen = micad();
    stolen["id"] = json!("00".repeat(32));
    refused(deployment(1, vec![stolen]), "component identity mismatch");
    let mut unknown = micad();
    unknown["role"] = json!("core");
    refused(
        deployment(1, vec![unknown]),
        "unknown, missing or invalid fields",
    );
}

#[test]
fn a_root_states_its_level_under_its_own_schema() {
    let refused = |value: Value, why: &str| assert!(parse(&value).is_err(), "{why}");
    let mut d = deployment(1, vec![micad()]);
    d["rootfs"]["schema"] = json!("mica/rootfs/v2");
    d["rootfs"]["id"] = json!(component_id(&d["rootfs"]).unwrap());
    refused(d, "another root schema");
    let mut d = deployment(1, vec![]);
    d["rootfs"]
        .as_object_mut()
        .unwrap()
        .remove("interfaceLevel");
    d["rootfs"]["id"] = json!(component_id(&d["rootfs"]).unwrap());
    refused(d, "a root without a level");
    refused(deployment(0, vec![]), "a root at level 0");
}

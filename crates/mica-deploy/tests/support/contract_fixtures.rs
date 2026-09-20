//! The generator of `tests/component-contracts/`: the four shared contract
//! files, derived from their unsigned inputs with TEST-ONLY signing keys.
//!
//! Inputs are the unsigned parts of the committed `cases.json` (the valid
//! descriptor, the refused mutations, the running identity) and
//! `firmware.json` (the manifests). Everything else is derived: component
//! ids, the deployment id, the running kernel's support id, the canonical
//! `deployment.json`, and every signed envelope. Ed25519 signatures are
//! deterministic, so the same inputs always give the same bytes, and the
//! committed files are a fixed point of [`generate`] (`tests/contract_fixtures.rs`
//! asserts it).
//!
//! The keys are derived from the labels below and exist for these fixtures
//! only. They are not, and must never become, trusted by any image, update
//! server or production trust set.

use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::components::component_id;
use ring::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde_json::{Value, json};

/// The label the deployment envelope key's seed is the SHA-256 of.
pub const DEPLOYMENT_KEY_LABEL: &str =
    "mica-core TEST-ONLY component-contract key: deployment envelope; never trusted";
/// The label the firmware envelope key's seed is the SHA-256 of.
pub const FIRMWARE_KEY_LABEL: &str =
    "mica-core TEST-ONLY component-contract key: firmware envelopes; never trusted";
/// The envelope schema every signed record carries.
pub const ENVELOPE_SCHEMA: &str = "mica/update-envelope/v1";

/// The five files, as their exact bytes.
pub struct Fixtures {
    pub deployment: String,
    pub envelope: String,
    pub firmware: String,
    pub cases: String,
    pub catalog: String,
}

/// The Ed25519 key a label names.
pub fn key(label: &str) -> Ed25519KeyPair {
    let seed = digest::digest(&digest::SHA256, label.as_bytes());
    Ed25519KeyPair::from_seed_unchecked(seed.as_ref()).expect("a 32-byte seed")
}

/// `{"schema","keyId","payload","signature"}` in that order, the canonical
/// envelope the readers require.
pub fn sign(key: &Ed25519KeyPair, payload: &[u8]) -> String {
    format!(
        "{{\"schema\":\"{ENVELOPE_SCHEMA}\",\"keyId\":\"{}\",\"payload\":\"{}\",\"signature\":\"{}\"}}",
        hex::encode(digest::digest(&digest::SHA256, key.public_key().as_ref())),
        STANDARD.encode(payload),
        STANDARD.encode(key.sign(payload).as_ref())
    )
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("JSON");
    text.push('\n');
    text
}

/// Derive the five files from the unsigned parts of `cases` and `firmware`.
pub fn generate(cases: &Value, firmware: &Value) -> Fixtures {
    let mut cases = cases.clone();
    let valid = cases["valid"].as_object_mut().expect("valid descriptor");
    for component in ["kernel", "rootfs"] {
        let id = component_id(&valid[component]).expect("component id");
        valid[component]["id"] = json!(id);
    }
    let valid = cases["valid"].clone();
    cases["deploymentId"] = json!(component_id(&valid).expect("deployment id"));
    cases["context"] = json!({
        "board": valid["board"],
        "arch": valid["arch"],
        "kernelBuildId": valid["kernel"]["buildId"],
        "kernelRelease": valid["kernel"]["release"],
        "supportId": component_id(&valid["kernel"]["support"]).expect("support id"),
    });
    let deployment = serde_json::to_string(&valid).expect("canonical descriptor");

    let deployment_key = key(DEPLOYMENT_KEY_LABEL);
    // The signed envelope as BYTES, not as a Value: its four fields are in the
    // order the readers require, which is not the order a Value serializes in.
    let signed_deployment = sign(&deployment_key, deployment.as_bytes());
    let envelope_value: Value = serde_json::from_str(&signed_deployment).expect("envelope");
    let envelope = pretty(&json!({
        "publicKey": STANDARD.encode(deployment_key.public_key().as_ref()),
        "envelope": envelope_value,
    }));

    let firmware_key = key(FIRMWARE_KEY_LABEL);
    let records: Vec<Value> = firmware["records"]
        .as_array()
        .expect("firmware records")
        .iter()
        .map(|record| {
            let mut manifest = record["manifest"].clone();
            manifest["id"] = json!(component_id(&manifest).expect("firmware id"));
            let payload = serde_json::to_vec(&manifest).expect("canonical manifest");
            json!({"manifest": manifest, "envelope": sign(&firmware_key, &payload)})
        })
        .collect();
    let firmware = pretty(&json!({
        "publicKey": STANDARD.encode(firmware_key.public_key().as_ref()),
        "records": records,
    }));

    // The catalog vector: one signed `mica/catalog/v2` document serving exactly
    // the golden deployment, at the source and instant `cases.json` names. It
    // is what an online update looks like on the wire, so a server
    // implementation has bytes to verify itself against rather than a schema
    // string it spells from memory.
    let input = &cases["catalog"];
    let source = input["source"].as_str().expect("catalog source");
    let origin = source
        .strip_suffix("/v1/manifest.json")
        .expect("a source URL ends in /v1/manifest.json");
    let mut objects = std::collections::BTreeMap::new();
    for pointer in [
        "/kernel/boot/artifact",
        "/kernel/support/image",
        "/kernel/support/signature",
        "/rootfs/content/image",
        "/rootfs/content/signature",
    ] {
        let artifact = valid.pointer(pointer).expect("artifact");
        let sha = artifact["sha256"].as_str().expect("sha256").to_owned();
        let url = format!("{origin}/v1/objects/{sha}");
        objects.insert(
            sha.clone(),
            json!({"sha256": sha, "bytes": artifact["bytes"], "url": url}),
        );
    }
    let payload = serde_json::to_vec(&json!({
        "schema": "mica/catalog/v2",
        "revision": input["revision"],
        "issuedAt": input["issuedAt"],
        "expiresAt": input["expiresAt"],
        "channels": [{
            "board": valid["board"],
            "product": valid["product"],
            "channel": input["channel"],
            "releaseId": input["releaseId"],
            "generation": valid["generation"],
        }],
        "releases": [{
            "id": input["releaseId"],
            "channel": input["channel"],
            "notes": input["notes"],
            "deployment": signed_deployment,
            "objects": objects.values().collect::<Vec<_>>(),
        }],
    }))
    .expect("canonical catalog");
    let catalog = pretty(&json!({
        "publicKey": STANDARD.encode(deployment_key.public_key().as_ref()),
        "source": source,
        "now": input["now"],
        "envelope": serde_json::from_str::<Value>(&sign(&deployment_key, &payload)).expect("envelope"),
    }));

    Fixtures {
        deployment,
        envelope,
        firmware,
        cases: pretty(&cases),
        catalog,
    }
}

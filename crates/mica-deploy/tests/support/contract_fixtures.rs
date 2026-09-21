//! The generator of `tests/component-contracts/`: the shared contract files,
//! derived from their unsigned inputs with TEST-ONLY signing keys.
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
use mica_deploy::{
    chunks,
    components::{Artifact, component_id},
};
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

/// What `envelopeWireOrder` says in every fixture that carries an envelope.
///
/// The stored object is tidy; the wire form is not. `authenticate_payload`
/// re-serialises the envelope and compares it against the bytes it was given,
/// so a consumer must rebuild this order before feeding a fixture to a reader.
pub const WIRE_ORDER: &str = "the envelope object is stored sorted for readability; the WIRE FORM is \
     schema, keyId, payload, signature, and a reader refuses any other order \
     as `noncanonical envelope`. Rebuild the order before authenticating.";

/// The five files, as their exact bytes.
pub struct Fixtures {
    pub deployment: String,
    pub envelope: String,
    pub firmware: String,
    pub cases: String,
    pub catalog: String,
    pub chunker: String,
}

/// The chunker vector's input, derived so that a port reproduces it without
/// shipping 256 KiB of test data: `sha256("mica-chunker-vector/v1" || uint32be(i))`
/// for `i` in `0..8192`, concatenated.
pub fn chunker_vector_input() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(8192 * 32);
    for index in 0_u32..8192 {
        let mut seed = b"mica-chunker-vector/v1".to_vec();
        seed.extend_from_slice(&index.to_be_bytes());
        bytes.extend_from_slice(digest::digest(&digest::SHA256, &seed).as_ref());
    }
    bytes
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
    // THE OBJECT BELOW IS STORED TIDY AND IS NOT THE WIRE FORM. A reader
    // re-serialises the envelope and compares it against the bytes it was
    // handed, so the four fields must arrive in the declared order; a `Value`
    // sorts them alphabetically and is refused as `noncanonical envelope`. The
    // note says so in the bytes, because the alternative is each new consumer
    // learning it by debugging (mica-build hit it on 2026-09-20).
    let envelope = pretty(&json!({
        "publicKey": STANDARD.encode(deployment_key.public_key().as_ref()),
        "envelopeWireOrder": WIRE_ORDER,
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
        "envelopeWireOrder": WIRE_ORDER,
        "envelope": serde_json::from_str::<Value>(&sign(&deployment_key, &payload)).expect("envelope"),
    }));

    // The chunker vector is signed by nothing and depends on no input above:
    // a delta is a transport for an object whose digest is already signed, so
    // what this file pins is agreement between two implementations of the cut,
    // which is the only thing a delta needs and the only thing a port can get
    // wrong silently.
    let input = chunker_vector_input();
    let mut cut = Vec::new();
    chunks::chunk(&mut input.as_slice(), |offset, length, sha| {
        cut.push(json!({"offset": offset, "length": length, "sha256": sha}));
    })
    .expect("chunking a slice cannot fail");
    let object = Artifact {
        sha256: hex::encode(digest::digest(&digest::SHA256, &input)),
        bytes: input.len() as u64,
    };
    let entries: Vec<(String, u64)> = cut
        .iter()
        .map(|chunk| {
            (
                chunk["sha256"].as_str().expect("digest").to_owned(),
                chunk["length"].as_u64().expect("length"),
            )
        })
        .collect();
    let chunker = pretty(&json!({
        "note": "The cut both sides must agree on. The origin chunks what it publishes; \
                 the device re-chunks what it already holds. Disagreement does not fail \
                 loudly -- it silently reuses nothing.",
        "input": {
            "derivation": "sha256(\"mica-chunker-vector/v1\" || uint32be(i)) for i in 0..8192, concatenated",
            "bytes": object.bytes,
            "sha256": object.sha256,
        },
        "parameters": {
            "gear": "GEAR[b] = sha256(\"mica-chunker/v1\" || b)[0..8] as big-endian uint64",
            "roll": "hash = (hash << 1) + GEAR[byte], wrapping at 64 bits, reset at every cut",
            "mask": "0xFFFC000000000000",
            "cutWhen": "length >= minChunk and (hash & mask) == 0, or length == maxChunk",
            "minChunk": chunks::MIN_CHUNK,
            "maxChunk": chunks::MAX_CHUNK,
        },
        "gearPrefix": (0..4).map(|b| format!("{:016x}", {
            let mut seed = b"mica-chunker/v1".to_vec();
            seed.push(b as u8);
            u64::from_be_bytes(digest::digest(&digest::SHA256, &seed).as_ref()[..8].try_into().expect("8 bytes"))
        })).collect::<Vec<_>>(),
        "chunks": cut,
        "index": hex::encode(chunks::write_index(&object, &entries)),
    }));

    Fixtures {
        deployment,
        envelope,
        firmware,
        cases: pretty(&cases),
        catalog,
        chunker,
    }
}

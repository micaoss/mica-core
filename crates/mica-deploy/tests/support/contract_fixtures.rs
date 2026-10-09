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

use aws_lc_rs::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use mica_deploy::{
    chunks,
    components::{Artifact, component_id},
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

/// Why the core set vector holds its payload and envelope as strings.
pub const CORE_SET_WIRE_FORM: &str = "`payload` is the signed bytes: JSON with no whitespace and every \
     object's keys in sorted order, which a reader re-serializes and compares. `envelope` is the \
     signed envelope in its wire order, and `coreSetId` the SHA-256 of `payload`";

/// The selection rule, in the vector's own bytes.
pub const SELECTION_RULE: &str = "A device composes the components with at least one feature among \
     its product's FEATURES, in package order. The selection is refused when a selected component \
     does not run on the root's interfaceLevel (root.min <= level <= root.max, an absent max \
     unbounded), or needs a package that is not selected or is selected at a version outside \
     min..max (dotted numbers compared numerically). Selecting nothing is not a refusal";

/// Why the catalog vector holds its documents as strings.
pub const CATALOG_WIRE_FORM: &str = "`manifest`, `release` and `descriptor` are exact wire bytes, \
     each served at the URL beside it. The manifest and the release's document are JSON with no \
     whitespace and every object's keys in sorted order: a reader re-serializes what it parses and \
     refuses a document that does not come back byte for byte. The descriptor is the signed envelope \
     in its wire order";

/// The files, as their exact bytes.
pub struct Fixtures {
    pub deployment: String,
    pub envelope: String,
    pub firmware: String,
    pub cases: String,
    pub catalog: String,
    pub chunker: String,
    pub core_set: String,
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

/// One broken index per rule the reader holds an index to, each derived from
/// the vector's valid index by the smallest change that breaks that rule and no
/// earlier one.
fn invalid_indexes(object: &Artifact, entries: &[(String, u64)]) -> Vec<Value> {
    let valid = chunks::write_index(object, entries);
    // Header: magic 8, digest 64, length 8, count 4; each entry: digest 64, length 4.
    let first_length = 84 + 64;
    let mutate = |change: &dyn Fn(&mut Vec<u8>)| {
        let mut bytes = valid.clone();
        change(&mut bytes);
        hex::encode(bytes)
    };
    let set_u32 = |bytes: &mut Vec<u8>, at: usize, value: u32| {
        bytes[at..at + 4].copy_from_slice(&value.to_be_bytes());
    };
    let first = u32::try_from(entries[0].1).expect("chunk length fits u32");
    vec![
        json!({"name": "oversized", "refusal": "transfer index exceeds the bound its object allows",
            "index": mutate(&|b: &mut Vec<u8>| b.resize(chunks::index_limit(object.bytes) as usize + 1, 0))}),
        json!({"name": "wrong-magic", "refusal": "invalid transfer index",
            "index": mutate(&|b: &mut Vec<u8>| b[7] = b'2')}),
        json!({"name": "another-object", "refusal": "transfer index is for another object",
            "index": mutate(&|b: &mut Vec<u8>| b[8] = if b[8] == b'0' { b'1' } else { b'0' })}),
        json!({"name": "wrong-object-length", "refusal": "transfer index disagrees with the signed object length",
            "index": mutate(&|b: &mut Vec<u8>| b[79] ^= 1)}),
        json!({"name": "count-mismatch", "refusal": "transfer index length does not match its chunk count",
            "index": mutate(&|b: &mut Vec<u8>| b[83] ^= 1)}),
        json!({"name": "uppercase-digest", "refusal": "invalid chunk digest",
        "index": mutate(&|b: &mut Vec<u8>| {
            let at = (84..84 + 64).find(|at| b[*at].is_ascii_lowercase()).expect("a hex letter");
            b[at] = b[at].to_ascii_uppercase();
        })}),
        json!({"name": "zero-length-chunk", "refusal": "invalid chunk length",
            "index": mutate(&|b: &mut Vec<u8>| set_u32(b, first_length, 0))}),
        json!({"name": "lengths-do-not-sum", "refusal": "chunk lengths do not sum to the signed object length",
            "index": mutate(&|b: &mut Vec<u8>| set_u32(b, first_length, first - 1))}),
    ]
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
    // note says so in the fixture's own bytes, so a consumer reading only the
    // fixture learns it.
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

    // The catalog vector: what an online update of the golden deployment
    // looks like on the wire. The manifest (`mica/catalog/v2`) at
    // `MANIFEST_PATH` below the source, the release's document
    // (`mica/release/v1`) it links to, and the signed descriptor that
    // document links to, each with the URL a reader fetches it from; every
    // file on another host than the source. A server implementation has bytes
    // to verify its writer against rather than a schema string it spells from
    // memory. Each document is stored as its exact wire bytes: a reader
    // refuses any other serialization of the same value.
    let input = &cases["catalog"];
    let source = input["source"].as_str().expect("catalog source");
    let downloads = input["downloadBase"]
        .as_str()
        .expect("catalog download base");
    let product = valid["product"].as_str().expect("product");
    let stamp = input["stamp"].as_str().expect("release stamp");
    let release_dir = format!("mica/{product}/{stamp}/");
    let release_base = format!("{downloads}{release_dir}");
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
        objects.insert(
            sha.clone(),
            json!({"sha256": sha, "bytes": artifact["bytes"], "path": format!("objects/{sha}")}),
        );
    }
    let release_id = input["releaseId"].as_str().expect("release id");
    let manifest = serde_json::to_string(&json!({
        "schema": "mica/catalog/v2",
        "revision": input["revision"],
        "baseUrl": downloads,
        "products": [{
            "product": product,
            "board": valid["board"],
            "variant": input["variant"],
            "latest": {
                "id": release_id,
                "generation": valid["generation"],
                "notes": input["notes"],
                "path": format!("{release_dir}index.json"),
            },
        }],
    }))
    .expect("canonical manifest");
    let release = serde_json::to_string(&json!({
        "schema": "mica/release/v1",
        "baseUrl": release_base,
        "id": release_id,
        "product": product,
        "board": valid["board"],
        "variant": input["variant"],
        "version": input["version"],
        "generation": valid["generation"],
        "notes": input["notes"],
        "publishedAt": input["publishedAt"],
        "files": input["files"],
        "deployment": {
            "path": "deployment.json",
            "sha256": hex::encode(digest::digest(&digest::SHA256, signed_deployment.as_bytes())),
            "bytes": signed_deployment.len(),
        },
        "objects": objects.values().collect::<Vec<_>>(),
    }))
    .expect("canonical release document");
    // The core set vector: every component of one channel for the golden
    // architecture, each carrying the golden root's content, signed with the
    // deployment key, and what the selection rule makes of it for the feature
    // sets and root levels `cases.json` lists. The selections are an input: the
    // readers are tested against them, and so is the producer's port of the rule.
    let core_input = &cases["coreSet"];
    let components: Vec<Value> = core_input["components"]
        .as_array()
        .expect("core components")
        .iter()
        .map(|input| {
            let mut component = json!({
                "schema": "mica/core/v1",
                "arch": valid["arch"],
                "package": input["package"],
                "version": input["version"],
                "features": input["features"],
                "needs": input["needs"],
                "root": input["root"],
                "content": valid["rootfs"]["content"],
            });
            component["id"] = json!(component_id(&component).expect("core id"));
            component
        })
        .collect();
    let core_payload = serde_json::to_string(&json!({
        "schema": "mica/core-set/v1",
        "channel": core_input["channel"],
        "arch": valid["arch"],
        "generation": core_input["generation"],
        "version": core_input["version"],
        "components": components,
    }))
    .expect("canonical core set");
    let signed_core_set = sign(&deployment_key, core_payload.as_bytes());
    let core_set = pretty(&json!({
        "publicKey": STANDARD.encode(deployment_key.public_key().as_ref()),
        "wireForm": CORE_SET_WIRE_FORM,
        "payload": core_payload,
        "coreSetId": hex::encode(digest::digest(&digest::SHA256, core_payload.as_bytes())),
        "envelope": signed_core_set,
        "selectionRule": SELECTION_RULE,
        "selections": core_input["selections"],
    }));

    // The catalog's core line for that set: a core release has one directory
    // per architecture, holding its document, the signed set and its objects.
    let channel = core_input["channel"].as_str().expect("core channel");
    let core_stamp = core_input["stamp"].as_str().expect("core stamp");
    let arch = valid["arch"].as_str().expect("arch");
    let core_id = format!("core.{channel}.{core_stamp}");
    let core_dir = format!("mica/core.{channel}/{core_stamp}/{arch}/");
    let core_base = format!("{downloads}{core_dir}");
    let content = &valid["rootfs"]["content"];
    let mut core_objects = std::collections::BTreeMap::new();
    for part in ["image", "signature"] {
        let sha = content[part]["sha256"].as_str().expect("sha256").to_owned();
        core_objects.insert(
            sha.clone(),
            json!({"sha256": sha, "bytes": content[part]["bytes"], "path": format!("objects/{sha}")}),
        );
    }
    let core_release = serde_json::to_string(&json!({
        "schema": "mica/core-release/v1",
        "baseUrl": core_base,
        "id": core_id,
        "channel": channel,
        "arch": arch,
        "generation": core_input["generation"],
        "version": core_input["version"],
        "coreSet": {
            "path": "core-set.json",
            "sha256": hex::encode(digest::digest(&digest::SHA256, signed_core_set.as_bytes())),
            "bytes": signed_core_set.len(),
        },
        "objects": core_objects.values().collect::<Vec<_>>(),
    }))
    .expect("canonical core release document");
    let mut manifest_value: Value = serde_json::from_str(&manifest).expect("manifest");
    manifest_value["cores"] = json!([{
        "channel": channel,
        "arch": arch,
        "latest": {
            "id": core_id,
            "generation": core_input["generation"],
            "path": format!("{core_dir}index.json"),
        },
    }]);
    let manifest = serde_json::to_string(&manifest_value).expect("canonical manifest");

    let catalog = pretty(&json!({
        "publicKey": STANDARD.encode(deployment_key.public_key().as_ref()),
        "source": source,
        "documentWireForm": CATALOG_WIRE_FORM,
        "manifestUrl": format!("{source}v2/manifest.json"),
        "manifest": manifest,
        "releaseUrl": format!("{release_base}index.json"),
        "release": release,
        "descriptorUrl": format!("{release_base}deployment.json"),
        "descriptor": signed_deployment,
        "coreReleaseUrl": format!("{core_base}index.json"),
        "coreRelease": core_release,
        "coreSetUrl": format!("{core_base}core-set.json"),
        "coreSet": signed_core_set,
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
        "invalidIndexNote": "Each case breaks one rule of the index and names the refusal it must \
                 draw. A device that meets any of them fetches the whole object instead.",
        "invalidIndexes": invalid_indexes(&object, &entries),
    }));

    Fixtures {
        deployment,
        envelope,
        firmware,
        cases: pretty(&cases),
        catalog,
        chunker,
        core_set,
    }
}

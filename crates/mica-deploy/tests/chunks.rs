//! The cut, the index, and what an unsigned index is allowed to do.

use mica_deploy::{
    chunks::{self, MAX_CHUNK, MIN_CHUNK, index_limit, parse_index, write_index},
    components::Artifact,
};
use ring::digest;
use serde_json::Value;

// Only the vector's input derivation is used here; the rest of the generator
// belongs to the fixture tests. One derivation, one place, rather than a
// second copy of the bytes this vector is about.
#[allow(dead_code)]
#[path = "support/contract_fixtures.rs"]
mod contract_fixtures;

const CHUNKER: &str = include_str!("component-contracts/chunker.json");

fn cut(bytes: &[u8]) -> Vec<(u64, u64, String)> {
    let mut chunks = Vec::new();
    chunks::chunk(&mut &bytes[..], |offset, length, sha| {
        chunks.push((offset, length, sha))
    })
    .unwrap();
    chunks
}

fn artifact(bytes: &[u8]) -> Artifact {
    Artifact {
        sha256: hex::encode(digest::digest(&digest::SHA256, bytes)),
        bytes: bytes.len() as u64,
    }
}

/// The vector is the contract with any second implementation of the cut, so
/// this repository must reproduce it from the derivation alone.
#[test]
fn the_committed_vector_is_what_this_chunker_cuts() {
    let vector: Value = serde_json::from_str(CHUNKER).unwrap();
    let input = contract_fixtures::chunker_vector_input();
    assert_eq!(
        vector["input"]["bytes"].as_u64().unwrap(),
        input.len() as u64
    );
    assert_eq!(vector["input"]["sha256"], artifact(&input).sha256);
    let expected = vector["chunks"].as_array().unwrap();
    let found = cut(&input);
    assert_eq!(found.len(), expected.len(), "chunk count");
    for (chunk, want) in found.iter().zip(expected) {
        assert_eq!(chunk.0, want["offset"].as_u64().unwrap());
        assert_eq!(chunk.1, want["length"].as_u64().unwrap());
        assert_eq!(chunk.2, want["sha256"].as_str().unwrap());
    }
    let entries: Vec<(String, u64)> = found
        .iter()
        .map(|(_, length, sha)| (sha.clone(), *length))
        .collect();
    assert_eq!(
        hex::encode(write_index(&artifact(&input), &entries)),
        vector["index"].as_str().unwrap(),
        "the index bytes are part of the vector, not only the boundaries"
    );
}

#[test]
fn every_chunk_respects_the_bounds_and_covers_the_input() {
    let input = contract_fixtures::chunker_vector_input();
    let found = cut(&input);
    assert!(found.len() > 4, "the vector must exercise several cuts");
    let mut offset = 0;
    for (index, (at, length, _)) in found.iter().enumerate() {
        assert_eq!(*at, offset);
        assert!(*length <= MAX_CHUNK);
        if index + 1 < found.len() {
            assert!(*length >= MIN_CHUNK, "only the last chunk may be short");
        }
        offset += length;
    }
    assert_eq!(offset, input.len() as u64);
}

/// The property the whole design rests on: an insertion near the start must
/// not renumber everything after it. A fixed-size chunker fails this test,
/// which is why it is here rather than in a comment.
#[test]
fn a_shift_near_the_start_leaves_the_later_chunks_alone() {
    let input = contract_fixtures::chunker_vector_input();
    let mut shifted = input.clone();
    shifted.splice(1000..1000, *b"one short insertion");
    let before: Vec<String> = cut(&input).into_iter().map(|c| c.2).collect();
    let after: Vec<String> = cut(&shifted).into_iter().map(|c| c.2).collect();
    let shared = before.iter().filter(|sha| after.contains(sha)).count();
    assert!(
        shared * 10 >= before.len() * 8,
        "only {shared} of {} chunks survived a 19-byte insertion",
        before.len()
    );
}

#[test]
fn an_index_must_describe_the_object_the_descriptor_signed() {
    let input = contract_fixtures::chunker_vector_input();
    let object = artifact(&input);
    let entries: Vec<(String, u64)> = cut(&input)
        .into_iter()
        .map(|(_, length, sha)| (sha, length))
        .collect();
    let good = write_index(&object, &entries);
    assert_eq!(parse_index(&good, &object).unwrap().0.len(), entries.len());

    let other = Artifact {
        sha256: "0".repeat(64),
        bytes: object.bytes,
    };
    for (name, bytes, against) in [
        ("truncated", good[..good.len() - 1].to_vec(), &object),
        ("empty", Vec::new(), &object),
        (
            "wrong magic",
            [b"MICAIDX2".to_vec(), good[8..].to_vec()].concat(),
            &object,
        ),
        ("another object", good.clone(), &other),
        (
            "a length that does not sum",
            {
                let mut bad = good.clone();
                let at = bad.len() - 4;
                bad[at..].copy_from_slice(&1_u32.to_be_bytes());
                bad
            },
            &object,
        ),
        (
            "a count its length contradicts",
            {
                let mut bad = good.clone();
                bad[80..84].copy_from_slice(&(entries.len() as u32 + 1).to_be_bytes());
                bad
            },
            &object,
        ),
        (
            "a chunk over the maximum",
            {
                let mut bad = good.clone();
                bad[84 + 64..84 + 68].copy_from_slice(&(MAX_CHUNK as u32 + 1).to_be_bytes());
                bad
            },
            &object,
        ),
        (
            "a digest that is not hex",
            {
                let mut bad = good.clone();
                bad[84] = b'z';
                bad
            },
            &object,
        ),
    ] {
        assert!(
            parse_index(&bytes, against).is_err(),
            "{name} must be refused"
        );
    }
}

/// The ceiling comes from the signed length, so it is known before the index
/// is fetched rather than after it has been read into memory.
#[test]
fn the_index_bound_is_derived_from_the_signed_object_length() {
    assert_eq!(index_limit(0), 84);
    assert_eq!(index_limit(MIN_CHUNK), 84 + 68);
    assert_eq!(index_limit(MIN_CHUNK + 1), 84 + 2 * 68);
    let input = contract_fixtures::chunker_vector_input();
    let object = artifact(&input);
    let entries: Vec<(String, u64)> = cut(&input)
        .into_iter()
        .map(|(_, length, sha)| (sha, length))
        .collect();
    let index = write_index(&object, &entries);
    assert!(index.len() as u64 <= index_limit(object.bytes));
    assert!(parse_index(&vec![0; index_limit(object.bytes) as usize + 1], &object).is_err());
}

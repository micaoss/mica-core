//! Content-defined chunking and the unsigned transfer index.
//!
//! A delta is a transport for an object whose identity is already signed: the
//! descriptor names every object by digest and length, and the assembled file
//! goes through the same verification a whole download does. **So nothing in
//! this module is trusted.** A hostile index or a substituted chunk can waste
//! bandwidth; it cannot install bytes, because the object that leaves here is
//! checked against a signed digest before it is promoted.
//!
//! The chunker is pinned rather than chosen at run time: the origin cuts what
//! it publishes and the device re-cuts what it already holds, and the two must
//! agree byte for byte or nothing is reused. `tests/component-contracts/
//! chunker.json` fixes the boundaries for a known input so a second
//! implementation is checked rather than described.
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufReader, Read},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Result, ensure};
use ring::digest;

use crate::components::Artifact;

/// No boundary is taken before this many bytes, so a run of cut points cannot
/// produce a chunk store full of tiny objects.
pub const MIN_CHUNK: u64 = 4096;
/// A chunk is cut here whether the hash agrees or not, which bounds both the
/// memory one chunk costs and the bytes a single HTTP request carries.
pub const MAX_CHUNK: u64 = 65536;
/// The top fourteen bits of the gear hash, giving one boundary every 16 KiB on
/// random input. The **top** bits and not the low ones: `hash << 1` shifts a
/// zero into bit 0, so the low bits of the gear hash depend on the last few
/// bytes alone, and a mask over them would cut on a window far shorter than
/// the 64 bytes the hash actually covers.
const MASK: u64 = 0xFFFC_0000_0000_0000;

/// `MICAIDX1`, the object digest, its length, and the chunk count.
pub const INDEX_HEADER: u64 = 8 + 64 + 8 + 4;
/// One chunk: its digest and its length.
pub const INDEX_ENTRY: u64 = 64 + 4;

/// Derived rather than tabulated, so another language reproduces it in three
/// lines instead of copying 256 constants and being believed.
/// `GEAR[i] = sha256("mica-chunker/v1" || i)[0..8]`, big-endian.
fn gear() -> &'static [u64; 256] {
    static TABLE: OnceLock<[u64; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0_u64; 256];
        for (index, entry) in table.iter_mut().enumerate() {
            let mut input = b"mica-chunker/v1".to_vec();
            input.push(index as u8);
            let hash = digest::digest(&digest::SHA256, &input);
            *entry = u64::from_be_bytes(hash.as_ref()[..8].try_into().expect("8 of 32 bytes"));
        }
        table
    })
}

fn hex_digest(bytes: &[u8]) -> String {
    hex::encode(digest::digest(&digest::SHA256, bytes))
}

/// Cut `reader` into chunks, reporting `(offset, length, digest)` in order.
/// Streams: a seed is read once and never held whole in memory.
pub fn chunk(reader: &mut impl Read, mut found: impl FnMut(u64, u64, String)) -> Result<u64> {
    let table = gear();
    let mut reader = BufReader::with_capacity(1 << 16, reader);
    let mut buffer = [0_u8; 1 << 16];
    let mut chunk = Vec::with_capacity(MAX_CHUNK as usize);
    let mut offset = 0_u64;
    let mut hash = 0_u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        for byte in &buffer[..read] {
            chunk.push(*byte);
            hash = hash.wrapping_shl(1).wrapping_add(table[*byte as usize]);
            let length = chunk.len() as u64;
            if (length >= MIN_CHUNK && hash & MASK == 0) || length == MAX_CHUNK {
                found(offset, length, hex_digest(&chunk));
                offset += length;
                chunk.clear();
                hash = 0;
            }
        }
    }
    if !chunk.is_empty() {
        let length = chunk.len() as u64;
        found(offset, length, hex_digest(&chunk));
        offset += length;
    }
    Ok(offset)
}

/// Where each distinct chunk digest can be read on this device. The first
/// source of a digest wins; a seed that disagrees with itself is not a
/// question worth asking, because every byte copied out is checked again.
pub fn seed_map(paths: &[PathBuf]) -> Result<BTreeMap<String, (PathBuf, u64, u64)>> {
    let mut map = BTreeMap::new();
    for path in paths {
        let Ok(file) = File::open(path) else { continue };
        let mut file = file;
        chunk(&mut file, |offset, length, sha| {
            map.entry(sha).or_insert((path.clone(), offset, length));
        })?;
    }
    Ok(map)
}

/// The chunk list of one object, in order.
pub struct Index(pub Vec<(String, u64)>);

/// The largest legal index for an object of `bytes`, derived from the signed
/// length: the device knows the ceiling before it fetches, so an index that
/// would not fit is refused without reading it.
pub fn index_limit(bytes: u64) -> u64 {
    INDEX_HEADER + INDEX_ENTRY * bytes.div_ceil(MIN_CHUNK)
}

/// `MICAIDX1`, object digest (64 ASCII), object length (u64 BE), chunk count
/// (u32 BE), then each chunk's digest (64 ASCII) and length (u32 BE).
/// The index must describe exactly the object the descriptor signed.
pub fn parse_index(bytes: &[u8], object: &Artifact) -> Result<Index> {
    ensure!(
        bytes.len() as u64 <= index_limit(object.bytes),
        "transfer index exceeds the bound its object allows"
    );
    ensure!(
        bytes.len() as u64 >= INDEX_HEADER && &bytes[..8] == b"MICAIDX1",
        "invalid transfer index"
    );
    ensure!(
        &bytes[8..72] == object.sha256.as_bytes(),
        "transfer index is for another object"
    );
    ensure!(
        u64::from_be_bytes(bytes[72..80].try_into()?) == object.bytes,
        "transfer index disagrees with the signed object length"
    );
    let count = u32::from_be_bytes(bytes[80..84].try_into()?) as u64;
    ensure!(
        INDEX_HEADER + INDEX_ENTRY * count == bytes.len() as u64,
        "transfer index length does not match its chunk count"
    );
    let mut chunks = Vec::with_capacity(count as usize);
    let mut total = 0_u64;
    for entry in 0..count as usize {
        let at = INDEX_HEADER as usize + entry * INDEX_ENTRY as usize;
        let sha = std::str::from_utf8(&bytes[at..at + 64])?;
        ensure!(
            sha.len() == 64
                && sha
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid chunk digest"
        );
        let length = u32::from_be_bytes(bytes[at + 64..at + 68].try_into()?) as u64;
        ensure!(length > 0 && length <= MAX_CHUNK, "invalid chunk length");
        total += length;
        chunks.push((sha.to_owned(), length));
    }
    ensure!(
        total == object.bytes,
        "chunk lengths do not sum to the signed object length"
    );
    Ok(Index(chunks))
}

/// Serialise an index. Present so a producer and the tests cut the same bytes;
/// the device never writes one.
pub fn write_index(object: &Artifact, chunks: &[(String, u64)]) -> Vec<u8> {
    let mut bytes = b"MICAIDX1".to_vec();
    bytes.extend_from_slice(object.sha256.as_bytes());
    bytes.extend_from_slice(&object.bytes.to_be_bytes());
    bytes.extend_from_slice(&(chunks.len() as u32).to_be_bytes());
    for (sha, length) in chunks {
        bytes.extend_from_slice(sha.as_bytes());
        bytes.extend_from_slice(&(*length as u32).to_be_bytes());
    }
    bytes
}

/// Read one chunk out of a seed and check it against the digest that named it.
pub fn read_seed(path: &Path, offset: u64, length: u64, sha: &str) -> Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; length as usize];
    file.read_exact(&mut bytes)?;
    ensure!(hex_digest(&bytes) == sha, "seed chunk changed under us");
    Ok(bytes)
}

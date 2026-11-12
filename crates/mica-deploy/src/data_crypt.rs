//! The DATA encryption layer: a LUKS2 volume in one fixed profile, written and
//! read here with no libcryptsetup, so the runkit can format, open and change
//! keyslots on its own.
//!
//! The profile: two 16 KiB header copies, a 16 MiB data offset, one `crypt`
//! segment of size `dynamic` with 4096-byte sectors (`aes-xts-plain64` with a
//! 512-bit key, or `xchacha12,aes-adiantum-plain64` with a 256-bit key), one
//! pbkdf2-sha256 digest, and keyslots whose area is `aes-xts-plain64` with the
//! `luks1` sha256 anti-forensic split at 4000 stripes, derived with pbkdf2-sha256
//! or argon2id. Tokens are kept as given; this layer reads only their `type`
//! and `keyslots`. `cryptsetup open` opens such a volume, and this layer opens
//! one `cryptsetup luksFormat` made with the same profile.
//!
//! It does not know where a keyslot's secret comes from: it takes a secret and
//! gives back the volume key.

mod af;
pub mod provider;
pub mod runkit;
pub mod tpm2;
mod xts;

use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::fs::FileExt;

use anyhow::{Context, Result, bail, ensure};
use aws_lc_rs::digest::{SHA256, digest};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use zeroize::Zeroizing;

/// The size of each header copy: the binary header and the JSON area.
pub const HEADER_SIZE: u64 = 16384;
const BINARY_SIZE: usize = 4096;
const JSON_SIZE: u64 = HEADER_SIZE - BINARY_SIZE as u64;
/// Where the payload starts.
pub const DATA_OFFSET: u64 = 16 * 1024 * 1024;
const KEYSLOTS_OFFSET: u64 = 2 * HEADER_SIZE;
const KEYSLOTS_SIZE: u64 = DATA_OFFSET - KEYSLOTS_OFFSET;
const STRIPES: usize = 4000;
const AREA_KEY_SIZE: usize = 64;
const AREA_CIPHER: &str = "aes-xts-plain64";
const SECTOR_SIZE: u32 = 4096;
const MAX_KEYSLOTS: usize = 32;
const DIGEST_ITERATIONS: u32 = 1000;
const PRIMARY_MAGIC: &[u8; 6] = b"LUKS\xba\xbe";
const SECONDARY_MAGIC: &[u8; 6] = b"SKUL\xba\xbe";

/// A volume key, wiped when dropped.
pub type VolumeKey = Zeroizing<Vec<u8>>;

/// The payload cipher, set per board and never chosen here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cipher {
    /// `aes-xts-plain64`, 512-bit key: boards with AES acceleration.
    AesXts,
    /// `xchacha12,aes-adiantum-plain64`, 256-bit key: boards without.
    Adiantum,
}

impl Cipher {
    /// The dm-crypt and LUKS2 spelling.
    #[must_use]
    pub const fn spec(self) -> &'static str {
        match self {
            Self::AesXts => "aes-xts-plain64",
            Self::Adiantum => "xchacha12,aes-adiantum-plain64",
        }
    }

    /// The volume key size in bytes.
    #[must_use]
    pub const fn key_size(self) -> usize {
        match self {
            Self::AesXts => 64,
            Self::Adiantum => 32,
        }
    }

    /// The cipher of `spec`, or `None` for one outside the profile.
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        [Self::AesXts, Self::Adiantum]
            .into_iter()
            .find(|cipher| cipher.spec() == spec)
    }
}

/// How a keyslot's secret is stretched into its area key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kdf {
    /// pbkdf2-sha256: for a high-entropy secret (a sealed TPM secret).
    Pbkdf2 { iterations: u32 },
    /// argon2id: for a passphrase a person keeps (the recovery key).
    Argon2id {
        time: u32,
        memory_kib: u32,
        cpus: u32,
    },
}

/// A keyslot to add: its secret, its KDF and, when a provider owns it, the
/// token that names it (an object with a `type`; its `keyslots` are set here).
pub struct NewKeyslot<'a> {
    pub secret: &'a [u8],
    pub kdf: Kdf,
    pub token: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Metadata {
    keyslots: BTreeMap<String, Keyslot>,
    tokens: BTreeMap<String, Map<String, Value>>,
    segments: BTreeMap<String, Segment>,
    digests: BTreeMap<String, Digest>,
    config: Config,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Keyslot {
    #[serde(rename = "type")]
    kind: String,
    key_size: usize,
    af: Af,
    area: Area,
    kdf: KdfJson,
    #[serde(flatten)]
    other: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Af {
    #[serde(rename = "type")]
    kind: String,
    stripes: usize,
    hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Area {
    #[serde(rename = "type")]
    kind: String,
    offset: String,
    size: String,
    encryption: String,
    key_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KdfJson {
    #[serde(rename = "type")]
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    iterations: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    time: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cpus: Option<u32>,
    salt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Segment {
    #[serde(rename = "type")]
    kind: String,
    offset: String,
    size: String,
    iv_tweak: String,
    encryption: String,
    sector_size: u32,
    #[serde(flatten)]
    other: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Digest {
    #[serde(rename = "type")]
    kind: String,
    keyslots: Vec<String>,
    segments: Vec<String>,
    hash: String,
    iterations: u32,
    salt: String,
    digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Config {
    json_size: String,
    keyslots_size: String,
    #[serde(flatten)]
    other: Map<String, Value>,
}

/// An opened header: the metadata of the newest valid copy.
#[derive(Debug, Clone)]
pub struct Volume {
    metadata: Metadata,
    seqid: u64,
    uuid: String,
}

fn random(len: usize) -> Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(vec![0u8; len]);
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| anyhow::anyhow!("random"))?;
    Ok(bytes)
}

fn area_size(key_size: usize) -> u64 {
    ((key_size * STRIPES) as u64).div_ceil(4096) * 4096
}

fn pbkdf2(secret: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) -> Result<()> {
    let iterations = std::num::NonZeroU32::new(iterations).context("zero pbkdf2 iterations")?;
    aws_lc_rs::pbkdf2::derive(
        aws_lc_rs::pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        secret,
        out,
    );
    Ok(())
}

fn derive(kdf: &KdfJson, secret: &[u8], out: &mut [u8]) -> Result<()> {
    let salt = B64.decode(&kdf.salt).context("keyslot salt")?;
    match kdf.kind.as_str() {
        "pbkdf2" => {
            ensure!(
                kdf.hash.as_deref() == Some("sha256"),
                "keyslot pbkdf2 hash outside the profile"
            );
            pbkdf2(
                secret,
                &salt,
                kdf.iterations.context("pbkdf2 iterations")?,
                out,
            )
        }
        "argon2id" => {
            let params = argon2::Params::new(
                kdf.memory.context("argon2 memory")?,
                kdf.time.context("argon2 time")?,
                kdf.cpus.context("argon2 cpus")?,
                Some(out.len()),
            )
            .map_err(|err| anyhow::anyhow!("argon2 parameters: {err}"))?;
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
                .hash_password_into(secret, &salt, out)
                .map_err(|err| anyhow::anyhow!("argon2: {err}"))
        }
        other => bail!("keyslot kdf {other} outside the profile"),
    }
}

fn kdf_json(kdf: Kdf) -> Result<KdfJson> {
    let salt = B64.encode(&*random(32)?);
    Ok(match kdf {
        Kdf::Pbkdf2 { iterations } => KdfJson {
            kind: "pbkdf2".into(),
            hash: Some("sha256".into()),
            iterations: Some(iterations),
            time: None,
            memory: None,
            cpus: None,
            salt,
        },
        Kdf::Argon2id {
            time,
            memory_kib,
            cpus,
        } => KdfJson {
            kind: "argon2id".into(),
            hash: None,
            iterations: None,
            time: Some(time),
            memory: Some(memory_kib),
            cpus: Some(cpus),
            salt,
        },
    })
}

fn number(text: &str) -> Result<u64> {
    text.parse()
        .with_context(|| format!("{text} is not a number"))
}

/// Whether `device` starts with a LUKS2 header, either copy. Read before any
/// flag is looked at, so an encrypted DATA is never mounted or formatted as
/// plain.
pub fn has_header(device: &File) -> Result<bool> {
    let mut magic = [0u8; 6];
    for offset in [0, HEADER_SIZE] {
        match device.read_exact_at(&mut magic, offset) {
            Ok(()) if &magic == PRIMARY_MAGIC || &magic == SECONDARY_MAGIC => return Ok(true),
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {}
            Err(err) => return Err(err).context("read the DATA header"),
        }
    }
    Ok(false)
}

/// One header copy at `offset`, when it is a valid one: its seqid, uuid and
/// metadata.
fn read_copy(device: &File, offset: u64) -> Result<(u64, String, Metadata)> {
    let mut area = vec![0u8; HEADER_SIZE as usize];
    device.read_exact_at(&mut area, offset)?;
    let magic = if offset == 0 {
        PRIMARY_MAGIC
    } else {
        SECONDARY_MAGIC
    };
    ensure!(&area[..6] == magic, "no LUKS2 magic at {offset}");
    let be = |area: &[u8], range: std::ops::Range<usize>| -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&area[range]);
        u64::from_be_bytes(bytes)
    };
    ensure!(area[6..8] == [0, 2], "not a LUKS2 header");
    ensure!(
        be(&area, 8..16) == HEADER_SIZE,
        "header size outside the profile"
    );
    ensure!(
        be(&area, 256..264) == offset,
        "header copy at the wrong offset"
    );
    ensure!(
        &area[72..79] == b"sha256\0",
        "header checksum outside the profile"
    );
    let mut stored = [0u8; 32];
    stored.copy_from_slice(&area[448..480]);
    area[448..512].fill(0);
    ensure!(
        digest(&SHA256, &area).as_ref() == stored,
        "header checksum mismatch at {offset}"
    );
    let seqid = be(&area, 16..24);
    let uuid = String::from_utf8_lossy(&area[168..208])
        .trim_end_matches('\0')
        .to_string();
    let json = &area[BINARY_SIZE..];
    let end = json.iter().position(|b| *b == 0).unwrap_or(json.len());
    let metadata: Metadata = serde_json::from_slice(&json[..end]).context("header JSON")?;
    Ok((seqid, uuid, metadata))
}

impl Volume {
    /// Read the newest valid header copy of `device` and check it is the
    /// profile this layer writes.
    pub fn read(device: &File) -> Result<Self> {
        let copies = [read_copy(device, 0), read_copy(device, HEADER_SIZE)];
        let (seqid, uuid, metadata) = match copies {
            [Ok(a), Ok(b)] => {
                if a.0 >= b.0 {
                    a
                } else {
                    b
                }
            }
            [Ok(a), Err(_)] => a,
            [Err(_), Ok(b)] => b,
            [Err(a), Err(_)] => return Err(a).context("no valid LUKS2 header copy"),
        };
        let volume = Self {
            metadata,
            seqid,
            uuid,
        };
        volume.check_profile()?;
        Ok(volume)
    }

    fn check_profile(&self) -> Result<()> {
        let m = &self.metadata;
        ensure!(
            number(&m.config.json_size)? == JSON_SIZE
                && number(&m.config.keyslots_size)? == KEYSLOTS_SIZE,
            "header areas outside the profile"
        );
        ensure!(m.segments.len() == 1, "one segment expected");
        let segment = m.segments.get("0").context("segment 0")?;
        ensure!(
            segment.kind == "crypt"
                && number(&segment.offset)? == DATA_OFFSET
                && segment.size == "dynamic"
                && segment.iv_tweak == "0"
                && segment.sector_size == SECTOR_SIZE
                && Cipher::parse(&segment.encryption).is_some(),
            "segment outside the profile"
        );
        ensure!(m.digests.len() == 1, "one digest expected");
        let digest = m.digests.values().next().context("digest")?;
        ensure!(
            digest.kind == "pbkdf2" && digest.hash == "sha256" && digest.segments == ["0"],
            "digest outside the profile"
        );
        let key_size = self.cipher().key_size();
        for (id, slot) in &m.keyslots {
            let offset = number(&slot.area.offset)?;
            let size = number(&slot.area.size)?;
            ensure!(
                slot.kind == "luks2"
                    && slot.key_size == key_size
                    && slot.af.kind == "luks1"
                    && slot.af.hash == "sha256"
                    && slot.af.stripes >= 1
                    && slot.area.kind == "raw"
                    && slot.area.encryption == AREA_CIPHER
                    && slot.area.key_size == AREA_KEY_SIZE
                    && offset >= KEYSLOTS_OFFSET
                    && size >= (key_size * slot.af.stripes) as u64
                    && offset
                        .checked_add(size)
                        .is_some_and(|end| end <= DATA_OFFSET),
                "keyslot {id} outside the profile"
            );
        }
        Ok(())
    }

    /// Format `device`: a new volume key, the header with one keyslot per
    /// `slots` (at least one), both copies written.
    pub fn format(
        device: &File,
        cipher: Cipher,
        uuid: &str,
        slots: &[NewKeyslot<'_>],
    ) -> Result<(Self, VolumeKey)> {
        ensure!(!slots.is_empty(), "a volume needs a keyslot");
        ensure!(uuid.len() <= 39 && uuid.is_ascii(), "invalid volume uuid");
        let key = random(cipher.key_size())?;
        let salt = random(32)?;
        let mut digest_value = [0u8; 32];
        pbkdf2(&key, &salt, DIGEST_ITERATIONS, &mut digest_value)?;
        let mut volume = Self {
            metadata: Metadata {
                keyslots: BTreeMap::new(),
                tokens: BTreeMap::new(),
                segments: BTreeMap::from([(
                    "0".to_string(),
                    Segment {
                        kind: "crypt".into(),
                        offset: DATA_OFFSET.to_string(),
                        size: "dynamic".into(),
                        iv_tweak: "0".into(),
                        encryption: cipher.spec().into(),
                        sector_size: SECTOR_SIZE,
                        other: Map::new(),
                    },
                )]),
                digests: BTreeMap::from([(
                    "0".to_string(),
                    Digest {
                        kind: "pbkdf2".into(),
                        keyslots: Vec::new(),
                        segments: vec!["0".into()],
                        hash: "sha256".into(),
                        iterations: DIGEST_ITERATIONS,
                        salt: B64.encode(&*salt),
                        digest: B64.encode(digest_value),
                    },
                )]),
                config: Config {
                    json_size: JSON_SIZE.to_string(),
                    keyslots_size: KEYSLOTS_SIZE.to_string(),
                    other: Map::new(),
                },
            },
            seqid: 0,
            uuid: uuid.to_string(),
        };
        for slot in slots {
            volume.place_keyslot(device, &key, slot)?;
        }
        volume.write(device)?;
        Ok((volume, key))
    }

    /// The payload cipher.
    #[must_use]
    pub fn cipher(&self) -> Cipher {
        self.metadata
            .segments
            .get("0")
            .and_then(|segment| Cipher::parse(&segment.encryption))
            .unwrap_or(Cipher::AesXts)
    }

    /// The volume uuid.
    #[must_use]
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// The keyslots, by number.
    #[must_use]
    pub fn keyslots(&self) -> Vec<usize> {
        self.metadata
            .keyslots
            .keys()
            .filter_map(|id| id.parse().ok())
            .collect()
    }

    /// The tokens: each one's object and the keyslots it names.
    #[must_use]
    pub fn tokens(&self) -> Vec<(&Map<String, Value>, Vec<usize>)> {
        self.metadata
            .tokens
            .values()
            .map(|token| {
                let slots = token
                    .get("keyslots")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|slot| slot.as_str()?.parse().ok())
                    .collect();
                (token, slots)
            })
            .collect()
    }

    /// The volume key, from `secret` opening `keyslot`.
    pub fn unlock(&self, device: &File, keyslot: usize, secret: &[u8]) -> Result<VolumeKey> {
        let slot = self
            .metadata
            .keyslots
            .get(&keyslot.to_string())
            .with_context(|| format!("no keyslot {keyslot}"))?;
        let mut area_key = Zeroizing::new(vec![0u8; slot.area.key_size]);
        derive(&slot.kdf, secret, &mut area_key)?;
        let length = (slot.key_size * slot.af.stripes).div_ceil(512) * 512;
        let mut material = Zeroizing::new(vec![0u8; length]);
        device
            .read_exact_at(&mut material, number(&slot.area.offset)?)
            .context("read keyslot area")?;
        xts::decrypt(&area_key, &mut material)?;
        let key = af::merge(&material, slot.key_size, slot.af.stripes)?;
        ensure!(
            self.verify(&key, keyslot)?,
            "keyslot {keyslot} does not open with this secret"
        );
        Ok(key)
    }

    /// The volume key from the first keyslot `secret` opens.
    pub fn unlock_any(&self, device: &File, secret: &[u8]) -> Result<VolumeKey> {
        for keyslot in self.keyslots() {
            if let Ok(key) = self.unlock(device, keyslot, secret) {
                return Ok(key);
            }
        }
        bail!("no keyslot opens with this secret")
    }

    fn verify(&self, key: &[u8], keyslot: usize) -> Result<bool> {
        let digest = self.metadata.digests.values().next().context("digest")?;
        let mut computed = Zeroizing::new(vec![0u8; 32]);
        pbkdf2(
            key,
            &B64.decode(&digest.salt).context("digest salt")?,
            digest.iterations,
            &mut computed,
        )?;
        let expected = B64.decode(&digest.digest).context("digest")?;
        Ok(digest.keyslots.contains(&keyslot.to_string())
            && expected.len() == computed.len()
            && aws_lc_rs::constant_time::verify_slices_are_equal(&expected, &computed).is_ok())
    }

    /// Add a keyslot for `slot` with the volume key: its area is written
    /// first, then the header, so a cut leaves the old header whole.
    pub fn add_keyslot(
        &mut self,
        device: &File,
        key: &[u8],
        slot: &NewKeyslot<'_>,
    ) -> Result<usize> {
        let mut verified = false;
        for keyslot in self.keyslots() {
            verified |= self.verify(key, keyslot)?;
        }
        ensure!(verified, "the key given is not this volume's key");
        let id = self.place_keyslot(device, key, slot)?;
        self.write(device)?;
        Ok(id)
    }

    /// Remove `keyslot` and the tokens left naming none: the header first,
    /// then its area is overwritten. The last keyslot cannot be removed.
    pub fn kill_keyslot(&mut self, device: &File, keyslot: usize) -> Result<()> {
        let id = keyslot.to_string();
        ensure!(
            self.metadata.keyslots.len() > 1,
            "the last keyslot cannot be removed"
        );
        let slot = self
            .metadata
            .keyslots
            .remove(&id)
            .with_context(|| format!("no keyslot {keyslot}"))?;
        for digest in self.metadata.digests.values_mut() {
            digest.keyslots.retain(|slot| slot != &id);
        }
        self.metadata.tokens.retain(|_, token| {
            if let Some(Value::Array(slots)) = token.get_mut("keyslots") {
                slots.retain(|slot| slot.as_str() != Some(&id));
                !slots.is_empty()
            } else {
                true
            }
        });
        self.write(device)?;
        let size = usize::try_from(number(&slot.area.size)?)?;
        device
            .write_all_at(&random(size)?, number(&slot.area.offset)?)
            .context("overwrite the keyslot area")?;
        device.sync_data().context("sync the keyslot area")?;
        Ok(())
    }

    /// Write `slot`'s area and add it to the metadata (not yet written).
    fn place_keyslot(&mut self, device: &File, key: &[u8], slot: &NewKeyslot<'_>) -> Result<usize> {
        let id = (0..MAX_KEYSLOTS)
            .find(|id| !self.metadata.keyslots.contains_key(&id.to_string()))
            .context("no free keyslot")?;
        let size = area_size(key.len());
        let mut used: Vec<(u64, u64)> = self
            .metadata
            .keyslots
            .values()
            .map(|slot| Ok((number(&slot.area.offset)?, number(&slot.area.size)?)))
            .collect::<Result<_>>()?;
        used.sort_unstable();
        let mut offset = KEYSLOTS_OFFSET;
        for (start, length) in used {
            if offset + size <= start {
                break;
            }
            offset = offset.max(start + length);
        }
        ensure!(offset + size <= DATA_OFFSET, "no room for another keyslot");
        let kdf = kdf_json(slot.kdf)?;
        let mut area_key = Zeroizing::new(vec![0u8; AREA_KEY_SIZE]);
        derive(&kdf, slot.secret, &mut area_key)?;
        let split = af::split(key, STRIPES)?;
        let mut material = Zeroizing::new(vec![0u8; usize::try_from(size)?]);
        material[..split.len()].copy_from_slice(&split);
        let encrypted = split.len().div_ceil(512) * 512;
        xts::encrypt(&area_key, &mut material[..encrypted])?;
        device
            .write_all_at(&material, offset)
            .context("write the keyslot area")?;
        device.sync_data().context("sync the keyslot area")?;
        self.metadata.keyslots.insert(
            id.to_string(),
            Keyslot {
                kind: "luks2".into(),
                key_size: key.len(),
                af: Af {
                    kind: "luks1".into(),
                    stripes: STRIPES,
                    hash: "sha256".into(),
                },
                area: Area {
                    kind: "raw".into(),
                    offset: offset.to_string(),
                    size: size.to_string(),
                    encryption: AREA_CIPHER.into(),
                    key_size: AREA_KEY_SIZE,
                },
                kdf,
                other: Map::new(),
            },
        );
        for digest in self.metadata.digests.values_mut() {
            digest.keyslots.push(id.to_string());
        }
        if let Some(mut token) = slot.token.clone() {
            ensure!(
                token
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| !kind.is_empty()),
                "a token needs a type"
            );
            token.insert("keyslots".into(), Value::from(vec![id.to_string()]));
            let token_id = (0..MAX_KEYSLOTS)
                .find(|id| !self.metadata.tokens.contains_key(&id.to_string()))
                .context("no free token")?;
            self.metadata.tokens.insert(token_id.to_string(), token);
        }
        Ok(id)
    }

    /// Write both header copies with the next seqid: the primary, synced,
    /// then the secondary, so a cut leaves one whole copy.
    fn write(&mut self, device: &File) -> Result<()> {
        let json = serde_json::to_vec(&self.metadata)?;
        ensure!((json.len() as u64) < JSON_SIZE, "header JSON does not fit");
        self.seqid += 1;
        let salt = random(64)?;
        for (offset, magic) in [(0, PRIMARY_MAGIC), (HEADER_SIZE, SECONDARY_MAGIC)] {
            let mut area = vec![0u8; HEADER_SIZE as usize];
            area[..6].copy_from_slice(magic);
            area[6..8].copy_from_slice(&2u16.to_be_bytes());
            area[8..16].copy_from_slice(&HEADER_SIZE.to_be_bytes());
            area[16..24].copy_from_slice(&self.seqid.to_be_bytes());
            area[72..78].copy_from_slice(b"sha256");
            area[104..168].copy_from_slice(&salt);
            area[168..168 + self.uuid.len()].copy_from_slice(self.uuid.as_bytes());
            area[256..264].copy_from_slice(&offset.to_be_bytes());
            area[BINARY_SIZE..BINARY_SIZE + json.len()].copy_from_slice(&json);
            let checksum = digest(&SHA256, &area);
            area[448..480].copy_from_slice(checksum.as_ref());
            device
                .write_all_at(&area, offset)
                .context("write a header copy")?;
            device.sync_data().context("sync a header copy")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

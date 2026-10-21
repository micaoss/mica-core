//! The protocol's framing and record layer (`docs/ble-provisioning.md`):
//! fragments, the X25519 exchange of `none`, the session keys, and the
//! AES-256-GCM records of `command` and `event`.

use aws_lc_rs::{aead, agreement, digest};

use super::spake2plus::expand;

/// AAD of every record, and the prefix of every derivation label.
pub const AAD: &[u8] = b"mica-provisioning/1";
const KEYS_INFO: &[u8] = b"mica-provisioning/1 session keys";
const NONE_SALT_LABEL: &[u8] = b"mica-provisioning/1 none";
/// The largest message, fragments joined.
pub const MAX_MESSAGE: usize = 16 * 1024;
/// A fragment's header: `seq` (u16 BE) and the flags.
pub const HEADER: usize = 3;
const LAST: u8 = 1;
/// The smallest and largest fragment.
pub const MIN_FRAGMENT: usize = 20;
pub const MAX_FRAGMENT: usize = 512;

/// The two directions' keys of a session.
#[derive(Clone)]
pub struct SessionKeys {
    pub phone_to_device: [u8; 32],
    pub device_to_phone: [u8; 32],
}

impl std::fmt::Debug for SessionKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SessionKeys(..)")
    }
}

/// The session keys from `ikm` under `salt`.
///
/// # Errors
/// HKDF failed.
pub fn session_keys(salt: &[u8], ikm: &[u8]) -> Result<SessionKeys, String> {
    let mut out = [0u8; 64];
    expand(salt, ikm, KEYS_INFO, &mut out)?;
    let mut keys = SessionKeys {
        phone_to_device: [0; 32],
        device_to_phone: [0; 32],
    };
    keys.phone_to_device.copy_from_slice(&out[..32]);
    keys.device_to_phone.copy_from_slice(&out[32..]);
    Ok(keys)
}

/// The device's half of a `none` exchange: its public key and the session
/// keys, from the phone's public key and the device's private key.
///
/// # Errors
/// The phone's key is not 32 bytes, or the shared secret is all zero.
pub fn none_exchange(
    device: &agreement::PrivateKey,
    phone_public: &[u8],
) -> Result<([u8; 32], SessionKeys), String> {
    if phone_public.len() != 32 {
        return Err("an X25519 public key is 32 bytes".to_string());
    }
    let device_public = device
        .compute_public_key()
        .map_err(|_| "the device key has no public half".to_string())?;
    let mut public = [0u8; 32];
    public.copy_from_slice(device_public.as_ref());
    let peer = agreement::UnparsedPublicKey::new(&agreement::X25519, phone_public);
    let keys = agreement::agree(device, peer, "X25519 failed", |secret| {
        if secret.iter().all(|byte| *byte == 0) {
            return Err("X25519 failed");
        }
        let mut salt = Vec::with_capacity(NONE_SALT_LABEL.len() + 64);
        salt.extend_from_slice(NONE_SALT_LABEL);
        salt.extend_from_slice(phone_public);
        salt.extend_from_slice(&public);
        let salt = digest::digest(&digest::SHA256, &salt);
        session_keys(salt.as_ref(), secret).map_err(|_| "HKDF failed")
    })
    .map_err(|_| "the X25519 exchange failed".to_string())?;
    Ok((public, keys))
}

/// One direction of the record layer: its key and the next counter.
pub struct Records {
    key: aead::LessSafeKey,
    counter: u64,
}

impl Records {
    /// # Errors
    /// aws-lc-rs refuses the key.
    pub fn new(key: &[u8; 32]) -> Result<Self, String> {
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, key)
            .map_err(|_| "an AES-256-GCM key is 32 bytes".to_string())?;
        Ok(Self {
            key: aead::LessSafeKey::new(key),
            counter: 0,
        })
    }

    fn nonce(&mut self) -> Result<aead::Nonce, String> {
        if self.counter == u64::MAX {
            return Err("the record counter would wrap".to_string());
        }
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&self.counter.to_be_bytes());
        self.counter += 1;
        Ok(aead::Nonce::assume_unique_for_key(nonce))
    }

    /// The next record carrying `plaintext`.
    ///
    /// # Errors
    /// The counter would wrap.
    pub fn seal(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let nonce = self.nonce()?;
        let mut record = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(nonce, aead::Aad::from(AAD), &mut record)
            .map_err(|_| "sealing failed".to_string())?;
        Ok(record)
    }

    /// The plaintext of the next record.
    ///
    /// # Errors
    /// The record does not authenticate under the next counter, or the
    /// counter would wrap.
    pub fn open(&mut self, record: &[u8]) -> Result<Vec<u8>, String> {
        let nonce = self.nonce()?;
        let mut buffer = record.to_vec();
        let plaintext = self
            .key
            .open_in_place(nonce, aead::Aad::from(AAD), &mut buffer)
            .map_err(|_| "the record does not authenticate".to_string())?;
        Ok(plaintext.to_vec())
    }
}

/// `message` cut into fragments of at most `size` bytes, header included.
#[must_use]
pub fn fragments(message: &[u8], size: usize) -> Vec<Vec<u8>> {
    let body = size.clamp(MIN_FRAGMENT, MAX_FRAGMENT) - HEADER;
    let chunks: Vec<&[u8]> = if message.is_empty() {
        vec![&[]]
    } else {
        message.chunks(body).collect()
    };
    let count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(seq, chunk)| {
            let mut fragment = Vec::with_capacity(HEADER + chunk.len());
            fragment.extend_from_slice(&(seq as u16).to_be_bytes());
            fragment.push(if seq + 1 == count { LAST } else { 0 });
            fragment.extend_from_slice(chunk);
            fragment
        })
        .collect()
}

/// Joins one characteristic's fragments into messages.
#[derive(Default)]
pub struct Reassembly {
    buffer: Vec<u8>,
    next: u16,
}

impl Reassembly {
    /// Take one fragment: `Ok(Some(message))` when it completes one,
    /// `Ok(None)` when more are due.
    ///
    /// # Errors
    /// The fragment is out of order, malformed or makes the message too
    /// long; the partial message is dropped.
    pub fn push(&mut self, fragment: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let outcome = self.accept(fragment);
        if !matches!(outcome, Ok(None)) {
            self.buffer.clear();
            self.next = 0;
        }
        outcome
    }

    fn accept(&mut self, fragment: &[u8]) -> Result<Option<Vec<u8>>, String> {
        if fragment.len() < HEADER {
            return Err("a fragment is at least its 3-byte header".to_string());
        }
        let seq = u16::from_be_bytes([fragment[0], fragment[1]]);
        let flags = fragment[2];
        if flags & !LAST != 0 {
            return Err("unknown fragment flags".to_string());
        }
        if seq != self.next {
            return Err(format!("fragment {seq} where {} was due", self.next));
        }
        if self.buffer.len() + fragment.len() - HEADER > MAX_MESSAGE {
            return Err("the message is over 16384 bytes".to_string());
        }
        self.buffer.extend_from_slice(&fragment[HEADER..]);
        if flags & LAST != 0 {
            return Ok(Some(std::mem::take(&mut self.buffer)));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| "too many fragments".to_string())?;
        Ok(None)
    }
}

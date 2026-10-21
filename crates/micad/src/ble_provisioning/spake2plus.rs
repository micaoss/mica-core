//! SPAKE2+ (RFC 9383), ciphersuite P256-SHA256-HKDF-SHA256-HMAC-SHA256, the
//! Verifier's side and, for the tests, the Prover's.
//!
//! The group arithmetic is `p256`'s; PBKDF2, HKDF, HMAC and SHA-256 are
//! aws-lc-rs's. `docs/ble-provisioning.md` fixes how the protocol uses it.

use std::num::NonZeroU32;

use aws_lc_rs::{digest, hkdf, hmac, pbkdf2};
use p256::elliptic_curve::PrimeField;
use p256::elliptic_curve::group::Group;
use p256::elliptic_curve::ops::Reduce;
use p256::elliptic_curve::sec1::{FromSec1Point, ToSec1Point};
use p256::{FieldBytes, ProjectivePoint, Scalar};

/// RFC 9383 section 4, P-256, compressed.
const M: [u8; 33] = [
    0x02, 0x88, 0x6e, 0x2f, 0x97, 0xac, 0xe4, 0x6e, 0x55, 0xba, 0x9d, 0xd7, 0x24, 0x25, 0x79, 0xf2,
    0x99, 0x3b, 0x64, 0xe1, 0x6e, 0xf3, 0xdc, 0xab, 0x95, 0xaf, 0xd4, 0x97, 0x33, 0x3d, 0x8f, 0xa1,
    0x2f,
];
const N: [u8; 33] = [
    0x03, 0xd8, 0xbb, 0xd6, 0xc6, 0x39, 0xc6, 0x29, 0x37, 0xb0, 0x4d, 0x99, 0x7f, 0x38, 0xc3, 0x77,
    0x07, 0x19, 0xc6, 0x29, 0xd7, 0x01, 0x4d, 0x49, 0xa2, 0x4b, 0x4f, 0x98, 0xba, 0xa1, 0x29, 0x2b,
    0x49,
];

/// Bytes of each half of the PBKDF output: ceil(log2 p) + 64 bits.
const HALF: usize = 40;
/// An uncompressed point.
pub const POINT_LEN: usize = 65;

fn constant(bytes: &[u8]) -> Result<ProjectivePoint, String> {
    ProjectivePoint::from_sec1_bytes(bytes)
        .map_err(|_| "an RFC 9383 constant is not a point".to_string())
}

/// `point` SEC1 uncompressed.
#[must_use]
pub fn encode(point: &ProjectivePoint) -> [u8; POINT_LEN] {
    let mut out = [0u8; POINT_LEN];
    let encoded = point.to_affine().to_sec1_point(false);
    // The identity encodes as one byte. It never reaches here, and the
    // all-zero result would be refused by any peer.
    if encoded.as_bytes().len() == POINT_LEN {
        out.copy_from_slice(encoded.as_bytes());
    }
    out
}

/// A peer's share: 65 bytes, uncompressed, on the curve, not the identity.
///
/// # Errors
/// Any of those does not hold.
pub fn decode_share(bytes: &[u8]) -> Result<ProjectivePoint, String> {
    if bytes.len() != POINT_LEN || bytes[0] != 0x04 {
        return Err("a share is a 65-byte uncompressed point".to_string());
    }
    let point = ProjectivePoint::from_sec1_bytes(bytes)
        .map_err(|_| "the share is not a point on P-256".to_string())?;
    if bool::from(point.is_identity()) {
        return Err("the share is the identity".to_string());
    }
    Ok(point)
}

/// `bytes`, big-endian, reduced modulo the group order.
fn reduce_wide(bytes: &[u8]) -> Scalar {
    let base = Scalar::from(256u64);
    bytes.iter().fold(Scalar::ZERO, |acc, &byte| {
        acc * base + Scalar::from(u64::from(byte))
    })
}

/// A scalar drawn from the system RNG (bias under 2^-64).
fn random_scalar() -> Result<Scalar, String> {
    let mut bytes = [0u8; HALF];
    aws_lc_rs::rand::fill(&mut bytes).map_err(|_| "the system RNG failed".to_string())?;
    Ok(reduce_wide(&bytes))
}

fn len8(out: &mut Vec<u8>, part: &[u8]) {
    out.extend_from_slice(&(part.len() as u64).to_le_bytes());
    out.extend_from_slice(part);
}

/// What the device keeps instead of the password: `{salt, iterations, w0, L}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub salt: Vec<u8>,
    pub iterations: u32,
    pub w0: [u8; 32],
    pub l: [u8; POINT_LEN],
}

/// `w0` and `w1` of `pw` under `salt` and `iterations`, for the two
/// identities.
#[must_use]
pub fn derive(
    pw: &[u8],
    salt: &[u8],
    iterations: NonZeroU32,
    id_prover: &[u8],
    id_verifier: &[u8],
) -> (Scalar, Scalar) {
    let mut input = Vec::new();
    len8(&mut input, pw);
    len8(&mut input, id_prover);
    len8(&mut input, id_verifier);
    let mut out = [0u8; 2 * HALF];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        iterations,
        salt,
        &input,
        &mut out,
    );
    (reduce_wide(&out[..HALF]), reduce_wide(&out[HALF..]))
}

/// The record of `pw` under `salt` and `iterations`.
#[must_use]
pub fn register(
    pw: &[u8],
    salt: &[u8],
    iterations: NonZeroU32,
    id_prover: &[u8],
    id_verifier: &[u8],
) -> Record {
    let (w0, w1) = derive(pw, salt, iterations, id_prover, id_verifier);
    Record {
        salt: salt.to_vec(),
        iterations: iterations.get(),
        w0: w0.to_repr().into(),
        l: encode(&(ProjectivePoint::generator() * w1)),
    }
}

/// The identities and context of one exchange.
pub struct Transcript<'a> {
    pub context: &'a [u8],
    pub id_prover: &'a [u8],
    pub id_verifier: &'a [u8],
}

/// The keys of a finished exchange.
#[derive(Clone, PartialEq, Eq)]
pub struct Keys {
    pub confirm_p: [u8; 32],
    pub confirm_v: [u8; 32],
    pub shared: [u8; 32],
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Keys(..)")
    }
}

fn schedule(
    t: &Transcript<'_>,
    share_p: &[u8],
    share_v: &[u8],
    z: &ProjectivePoint,
    v: &ProjectivePoint,
    w0: &Scalar,
) -> Result<Keys, String> {
    let mut tt = Vec::new();
    len8(&mut tt, t.context);
    len8(&mut tt, t.id_prover);
    len8(&mut tt, t.id_verifier);
    len8(&mut tt, &encode(&constant(&M)?));
    len8(&mut tt, &encode(&constant(&N)?));
    len8(&mut tt, share_p);
    len8(&mut tt, share_v);
    len8(&mut tt, &encode(z));
    len8(&mut tt, &encode(v));
    len8(&mut tt, &w0.to_repr());
    let k_main = digest::digest(&digest::SHA256, &tt);
    let mut confirmation = [0u8; 64];
    expand(&[], k_main.as_ref(), b"ConfirmationKeys", &mut confirmation)?;
    let mut shared = [0u8; 32];
    expand(&[], k_main.as_ref(), b"SharedKey", &mut shared)?;
    let mac = |key: &[u8], message: &[u8]| -> [u8; 32] {
        let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), message);
        let mut out = [0u8; 32];
        out.copy_from_slice(tag.as_ref());
        out
    };
    Ok(Keys {
        confirm_p: mac(&confirmation[..32], share_v),
        confirm_v: mac(&confirmation[32..], share_p),
        shared,
    })
}

/// HKDF-SHA256 of `ikm` under `salt` with `info`, filling `out`.
///
/// # Errors
/// `out` is longer than HKDF-SHA256 can fill.
pub fn expand(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) -> Result<(), String> {
    struct Len(usize);
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let info = [info];
    hkdf::Salt::new(hkdf::HKDF_SHA256, salt)
        .extract(ikm)
        .expand(&info, Len(out.len()))
        .and_then(|okm| okm.fill(out))
        .map_err(|_| "HKDF-SHA256 cannot fill that much".to_string())
}

/// The Verifier's answer to `share_p`: its share and the keys.
pub struct Answer {
    pub share_v: [u8; POINT_LEN],
    pub keys: Keys,
}

/// Answer `share_p` from `record`, with `y` drawn at random.
///
/// # Errors
/// The record or the share is not valid, or the RNG failed.
pub fn answer(record: &Record, t: &Transcript<'_>, share_p: &[u8]) -> Result<Answer, String> {
    answer_with(record, t, share_p, random_scalar()?)
}

/// [`answer`] with a given `y`.
///
/// # Errors
/// The record or the share is not valid.
pub fn answer_with(
    record: &Record,
    t: &Transcript<'_>,
    share_p: &[u8],
    y: Scalar,
) -> Result<Answer, String> {
    let w0 = Option::<Scalar>::from(Scalar::from_repr(FieldBytes::from(record.w0)))
        .ok_or_else(|| "the record's w0 is not a scalar".to_string())?;
    let l = decode_share(&record.l).map_err(|_| "the record's L is not a point".to_string())?;
    let x = decode_share(share_p)?;
    let share_v = encode(&(ProjectivePoint::generator() * y + constant(&N)? * w0));
    let z = (x - constant(&M)? * w0) * y;
    let v = l * y;
    Ok(Answer {
        share_v,
        keys: schedule(t, share_p, &share_v, &z, &v, &w0)?,
    })
}

/// A scalar from 32 big-endian bytes, reduced.
#[must_use]
pub fn scalar(bytes: &[u8; 32]) -> Scalar {
    <Scalar as Reduce<FieldBytes>>::reduce(&FieldBytes::from(*bytes))
}

/// The Prover's side, for the tests: its share for `x`, and the keys once it
/// has the Verifier's share.
#[cfg(test)]
pub mod prover {
    use super::*;

    pub fn share(w0: &Scalar, x: &Scalar) -> [u8; POINT_LEN] {
        encode(&(ProjectivePoint::generator() * x + constant(&M).unwrap() * w0))
    }

    pub fn finish(
        t: &Transcript<'_>,
        w0: &Scalar,
        w1: &Scalar,
        x: &Scalar,
        share_v: &[u8],
    ) -> Result<Keys, String> {
        let share_p = share(w0, x);
        let y = decode_share(share_v)? - constant(&N)? * w0;
        schedule(t, &share_p, share_v, &(y * x), &(y * w1), w0)
    }
}

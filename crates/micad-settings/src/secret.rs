//! Randomness and password hashing, shared so a secret minted or a hash
//! written by one daemon is one the other reads.

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};

/// Bytes of salt behind each password hash; the PHC recommendation.
const SALT_BYTES: usize = 16;

/// `N` bytes from the operating system's CSPRNG.
#[must_use]
pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    #[allow(clippy::expect_used)]
    aws_lc_rs::rand::fill(&mut bytes)
        .expect("INVARIANT: AWS-LC's RAND_bytes aborts the process rather than return a failure");
    bytes
}

/// Hash `password` with Argon2id default parameters into a PHC string.
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    let salt = SaltString::encode_b64(&random_bytes::<SALT_BYTES>())?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)?
        .to_string())
}

/// True when `password` matches the PHC-formatted `hash`.
///
/// A malformed hash verifies as false rather than erroring: a corrupt stored
/// credential must reject every password, not accept any.
#[must_use]
pub fn verify_password(hash: &str, password: &str) -> bool {
    PasswordHash::new(hash)
        .and_then(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed))
        .is_ok()
}

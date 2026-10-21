//! Randomness and password hashing, shared so a secret minted or a hash
//! written by one daemon is one the other reads.

use argon2::Argon2;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};

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

/// Characters of a generated access-point key: 80 bits.
const GENERATED_AP_PSK_CHARS: usize = 16;
/// The alphabet a generated key is drawn from, the device code's: no `0`,
/// `O`, `1` or `I` to misread off a screen. 32 divides 256, so a byte folds
/// into it without bias.
const AP_PSK_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";

/// A fresh access-point key, as a claim that names none sets it.
#[must_use]
pub fn generate_ap_psk() -> String {
    random_bytes::<GENERATED_AP_PSK_CHARS>()
        .iter()
        .map(|byte| char::from(AP_PSK_ALPHABET[usize::from(byte % 32)]))
        .collect()
}

/// Hash `password` with Argon2id default parameters into a PHC string.
pub fn hash_password(password: &str) -> Result<String, argon2::password_hash::Error> {
    Ok(Argon2::default()
        .hash_password_with_salt(password.as_bytes(), &random_bytes::<SALT_BYTES>())?
        .to_string())
}

/// True when `password` matches the PHC-formatted `hash`.
///
/// A malformed hash verifies as false rather than erroring: a corrupt stored
/// credential must reject every password, not accept any.
#[must_use]
pub fn verify_password(hash: &str, password: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), hash)
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn randomness_comes_from_the_kernel_not_cpu_jitter() {
        // AWS-LC seeds from CPU jitter unless built without it; on the RK3576's
        // 24 MHz counter jitter fails its health test most of the time and
        // AWS-LC aborts the process on the first draw (apid died on every
        // login). .cargo/config.toml builds it with getrandom only.
        assert!(
            aws_lc_rs::try_fips_cpu_jitter_entropy().is_err(),
            "AWS-LC is seeding from CPU jitter; AWS_LC_SYS_NO_JITTER_ENTROPY is not set"
        );
        assert_ne!(random_bytes::<16>(), [0u8; 16]);
    }

    /// A stored hash verifies its own password and refuses every other one.
    #[test]
    fn a_stored_hash_still_verifies() {
        const STORED: &str = "$argon2id$v=19$m=19456,t=2,p=1$D6IR8Aq6fn1gG+HkYUPmOA$YM6lgHyab1IFa8QjnX1i1NTov+TDZtowvNQ3qoSc7nU";
        assert!(verify_password(STORED, "mica-argon2-0.5-fixture"));
        assert!(!verify_password(STORED, "mica-argon2-0.5-fixturf"));
        assert!(!verify_password(
            "$argon2id$v=19$garbage",
            "mica-argon2-0.5-fixture"
        ));
    }

    /// A new hash is Argon2id v19 at m=19456, t=2, p=1, with a 16-byte salt and
    /// a 32-byte output, the PHC string every stored credential has.
    #[test]
    fn a_new_hash_keeps_the_stored_parameters() {
        let hash = hash_password("correct horse").unwrap();
        let fields: Vec<&str> = hash.split('$').collect();
        assert_eq!(
            fields[..4],
            ["", "argon2id", "v=19", "m=19456,t=2,p=1"],
            "{hash}"
        );
        assert_eq!(
            fields[4].len(),
            22,
            "a 16-byte salt in unpadded B64: {hash}"
        );
        assert_eq!(
            fields[5].len(),
            43,
            "a 32-byte output in unpadded B64: {hash}"
        );
        assert!(verify_password(&hash, "correct horse"));
        assert_ne!(
            hash,
            hash_password("correct horse").unwrap(),
            "the salt is fresh"
        );
    }
}

//! AES-256-XTS with the `plain64` IV, the keyslot area cipher, in 512-byte
//! sectors numbered from the start of the area.

use anyhow::{Context, Result, ensure};
use aws_lc_rs::cipher::{AES_256, DecryptingKey, EncryptingKey, UnboundCipherKey};

const SECTOR: usize = 512;
const BLOCK: usize = 16;

/// Multiply the tweak by the primitive element of GF(2^128), little-endian.
fn double(tweak: &mut [u8; BLOCK]) {
    let carry = tweak[BLOCK - 1] >> 7;
    for index in (1..BLOCK).rev() {
        tweak[index] = (tweak[index] << 1) | (tweak[index - 1] >> 7);
    }
    tweak[0] = (tweak[0] << 1) ^ (0x87 * carry);
}

fn keys(key: &[u8]) -> Result<(&[u8], &[u8])> {
    ensure!(key.len() == 64, "aes-xts-plain64 takes a 512-bit key");
    Ok(key.split_at(32))
}

fn tweak_key(key: &[u8]) -> Result<EncryptingKey> {
    EncryptingKey::ecb(
        UnboundCipherKey::new(&AES_256, key)
            .ok()
            .context("tweak key")?,
    )
    .ok()
    .context("tweak key")
}

/// The first tweak of `sector`: its number, little-endian, encrypted with the
/// second key.
fn first_tweak(tweaker: &EncryptingKey, sector: u64) -> Result<[u8; BLOCK]> {
    let mut tweak = [0u8; BLOCK];
    tweak[..8].copy_from_slice(&sector.to_le_bytes());
    tweaker.encrypt(&mut tweak).ok().context("tweak")?;
    Ok(tweak)
}

/// Encrypt `data` (whole sectors) in place.
pub fn encrypt(key: &[u8], data: &mut [u8]) -> Result<()> {
    ensure!(
        data.len().is_multiple_of(SECTOR),
        "xts data is not whole sectors"
    );
    let (data_key, tweak) = keys(key)?;
    let cipher = EncryptingKey::ecb(
        UnboundCipherKey::new(&AES_256, data_key)
            .ok()
            .context("data key")?,
    )
    .ok()
    .context("data key")?;
    let tweaker = tweak_key(tweak)?;
    for (sector, chunk) in data.chunks_mut(SECTOR).enumerate() {
        let mut tweak = first_tweak(&tweaker, sector as u64)?;
        for block in chunk.chunks_mut(BLOCK) {
            block.iter_mut().zip(tweak).for_each(|(b, t)| *b ^= t);
            cipher.encrypt(block).ok().context("xts encrypt")?;
            block.iter_mut().zip(tweak).for_each(|(b, t)| *b ^= t);
            double(&mut tweak);
        }
    }
    Ok(())
}

/// Decrypt `data` (whole sectors) in place.
pub fn decrypt(key: &[u8], data: &mut [u8]) -> Result<()> {
    ensure!(
        data.len().is_multiple_of(SECTOR),
        "xts data is not whole sectors"
    );
    let (data_key, tweak) = keys(key)?;
    let cipher = DecryptingKey::ecb(
        UnboundCipherKey::new(&AES_256, data_key)
            .ok()
            .context("data key")?,
    )
    .ok()
    .context("data key")?;
    let tweaker = tweak_key(tweak)?;
    for (sector, chunk) in data.chunks_mut(SECTOR).enumerate() {
        let mut tweak = first_tweak(&tweaker, sector as u64)?;
        for block in chunk.chunks_mut(BLOCK) {
            block.iter_mut().zip(tweak).for_each(|(b, t)| *b ^= t);
            cipher
                .decrypt(block, aws_lc_rs::cipher::DecryptionContext::None)
                .ok()
                .context("xts decrypt")?;
            block.iter_mut().zip(tweak).for_each(|(b, t)| *b ^= t);
            double(&mut tweak);
        }
    }
    Ok(())
}

//! The LUKS anti-forensic splitter (`af.type` `luks1`, hash sha256): a key is
//! stored as `stripes` blocks, all of which are needed to merge it back.

use anyhow::{Result, ensure};
use aws_lc_rs::digest::{Context, SHA256};
use zeroize::Zeroizing;

/// The sha256 diffusion of one stripe-sized buffer, block by block, each
/// block hashed with its big-endian index in front.
fn diffuse(buffer: &mut [u8]) {
    let mut out = Zeroizing::new(Vec::with_capacity(buffer.len()));
    for (index, block) in buffer.chunks(32).enumerate() {
        let mut hash = Context::new(&SHA256);
        hash.update(&u32::try_from(index).unwrap_or(u32::MAX).to_be_bytes());
        hash.update(block);
        out.extend_from_slice(&hash.finish().as_ref()[..block.len()]);
    }
    buffer.copy_from_slice(&out);
}

/// Split `key` over `stripes` blocks, the first `stripes - 1` random.
pub fn split(key: &[u8], stripes: usize) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(stripes >= 1 && !key.is_empty(), "invalid af split");
    let size = key.len();
    let mut out = Zeroizing::new(vec![0u8; size * stripes]);
    let mut acc = Zeroizing::new(vec![0u8; size]);
    for stripe in 0..stripes - 1 {
        let block = &mut out[stripe * size..(stripe + 1) * size];
        aws_lc_rs::rand::fill(block).map_err(|_| anyhow::anyhow!("random"))?;
        acc.iter_mut().zip(block.iter()).for_each(|(a, b)| *a ^= b);
        diffuse(&mut acc);
    }
    let last = &mut out[(stripes - 1) * size..];
    last.iter_mut()
        .zip(acc.iter().zip(key))
        .for_each(|(o, (a, k))| *o = a ^ k);
    Ok(out)
}

/// Merge `material` (`stripes` blocks of `size` bytes) back into the key.
pub fn merge(material: &[u8], size: usize, stripes: usize) -> Result<Zeroizing<Vec<u8>>> {
    ensure!(
        stripes >= 1 && size > 0 && material.len() >= size * stripes,
        "invalid af merge"
    );
    let mut acc = Zeroizing::new(vec![0u8; size]);
    for stripe in 0..stripes - 1 {
        let block = &material[stripe * size..(stripe + 1) * size];
        acc.iter_mut().zip(block).for_each(|(a, b)| *a ^= b);
        diffuse(&mut acc);
    }
    let last = &material[(stripes - 1) * size..stripes * size];
    acc.iter_mut().zip(last).for_each(|(a, b)| *a ^= b);
    Ok(acc)
}

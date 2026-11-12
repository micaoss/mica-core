//! The `tpm2` key provider: a 32-byte keyslot secret sealed under the TPM's
//! ECC P-256 storage key to PCR 7 (Secure Boot policy) and PCR 15 = 0, kept
//! in a `systemd-tpm2` token. Its seal-off extends PCR 15, so the booted
//! system can never unseal again.

mod client;
mod wire;

#[cfg(test)]
mod tests;

use std::fs::{File, OpenOptions};
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Map, Value, json};
use zeroize::Zeroizing;

pub use client::Transport;
use client::{Client, POLICY_PCRS, policy_pcr_digest, random, sha256};

use super::provider::{Availability, KeyProvider, Secret};

/// The device the runkit talks to the TPM through.
pub const DEVICE: &str = "/dev/tpmrm0";
/// The LUKS2 token type, systemd's.
pub const TOKEN: &str = "systemd-tpm2";
/// What PCR 15 is extended with on every path before the root starts.
pub const SEALED_OFF: &[u8] = b"mica-data-unsealed";
/// The keyslot secret, before base64 makes it the keyslot passphrase.
const SECRET_SIZE: usize = 32;

pub struct Tpm2<T> {
    client: Result<Client<T>, String>,
}

impl Tpm2<File> {
    /// The provider over `path` (normally [`DEVICE`]); a TPM that cannot be
    /// opened makes it unavailable.
    #[must_use]
    pub fn open(path: &Path) -> Self {
        let transport = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|err| format!("{}: {err}", path.display()));
        Self {
            client: transport.map(Client::new),
        }
    }
}

impl<T: Transport> Tpm2<T> {
    pub const fn new(transport: T) -> Self {
        Self {
            client: Ok(Client::new(transport)),
        }
    }

    fn client(&mut self) -> Result<&mut Client<T>> {
        self.client
            .as_mut()
            .map_err(|why| anyhow::anyhow!("no TPM: {why}"))
    }
}

/// The keyslot passphrase of a sealed secret: base64, as systemd's.
fn passphrase(secret: &[u8]) -> Secret {
    Zeroizing::new(B64.encode(secret).into_bytes())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

/// The TPM2B_PRIVATE and TPM2B_PUBLIC contents of a token's `tpm2-blob`.
fn split_blob(blob: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut r = wire::R::new(blob);
    let parsed = (|| {
        let private = r.b()?.to_vec();
        let public = r.b()?.to_vec();
        r.done()?;
        Ok::<_, String>((private, public))
    })();
    parsed.map_err(|err| anyhow::anyhow!("tpm2-blob: {err}"))
}

impl<T: Transport> KeyProvider for Tpm2<T> {
    fn token_type(&self) -> &'static str {
        TOKEN
    }

    fn available(&mut self) -> Availability {
        match &self.client {
            Ok(_) => Availability::Ready,
            Err(why) => Availability::Unavailable(why.clone()),
        }
    }

    fn enroll(&mut self) -> Result<(Secret, Map<String, Value>)> {
        let client = self.client()?;
        let primary = client.create_primary()?;
        let sealed = (|| {
            let pcrs = client.pcr_read(&POLICY_PCRS[..1])?;
            let [pcr7] = pcrs.as_slice() else {
                bail!("PCR 7 read gave {} values", pcrs.len());
            };
            // This boot's PCR 7 and PCR 15 as every boot starts it.
            let policy = policy_pcr_digest(&[pcr7.clone(), vec![0; 32]]);
            let secret = random(SECRET_SIZE)?;
            let (private, public) = client.seal(&primary, &secret, &policy)?;
            Ok((secret, private, public, policy))
        })();
        client.flush(primary.handle);
        let (secret, private, public, policy) = sealed?;
        let mut blob = wire::W::default();
        blob.b(&private).b(&public);
        let token = json!({
            "type": TOKEN,
            "keyslots": [],
            "tpm2-blob": B64.encode(&blob.0),
            "tpm2-pcrs": POLICY_PCRS,
            "tpm2-pcr-bank": "sha256",
            "tpm2-primary-alg": "ecc",
            "tpm2-policy-hash": hex(&policy),
            "tpm2-pin": false,
            "mica-srk-name": hex(&primary.name),
        });
        let Value::Object(token) = token else {
            unreachable!("a JSON object literal")
        };
        Ok((passphrase(&secret), token))
    }

    fn unlock(&mut self, token: &Map<String, Value>) -> Result<Secret> {
        let field = |name: &str| {
            token
                .get(name)
                .and_then(Value::as_str)
                .with_context(|| format!("token without {name}"))
        };
        let blob = B64.decode(field("tpm2-blob")?).context("tpm2-blob")?;
        let (private, public) = split_blob(&blob)?;
        let want = unhex(field("mica-srk-name")?).context("mica-srk-name")?;
        let client = self.client()?;
        let primary = client.create_primary()?;
        // The name is checked before the session is salted to the key, so an
        // interposer cannot offer its own.
        let secret = if primary.name == want {
            client
                .unseal(&primary, &private, &public)
                .map_err(anyhow::Error::from)
        } else {
            Err(anyhow::anyhow!(
                "the storage key is not the one sealed to: refused"
            ))
        };
        client.flush(primary.handle);
        Ok(passphrase(&secret?))
    }

    fn seal_off(&mut self) -> Result<()> {
        let client = self.client()?;
        client.pcr_extend(15, &sha256(&[SEALED_OFF]))?;
        Ok(())
    }
}

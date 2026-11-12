//! Key providers and the order the runkit opens or formats DATA in.
//!
//! A provider owns one LUKS2 token type: it enrols a keyslot secret with its
//! token, unlocks a token back into that secret, and seals itself off before
//! the root starts. The recovery passphrase is not a provider: it is always
//! enrolled, as an argon2id keyslot with no token.

use std::fs::File;

use anyhow::Result;
use serde_json::{Map, Value};
use zeroize::Zeroizing;

use super::{Cipher, Kdf, NewKeyslot, Volume, VolumeKey, has_header, random};

/// A keyslot secret, wiped when dropped.
pub type Secret = Zeroizing<Vec<u8>>;

/// Whether a provider can be used now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Ready,
    Unavailable(String),
}

pub trait KeyProvider {
    /// The LUKS2 token type it owns.
    fn token_type(&self) -> &'static str;
    /// Usable now, or why not.
    fn available(&mut self) -> Availability;
    /// A new keyslot secret and its token (`keyslots` is set by the layer).
    fn enroll(&mut self) -> Result<(Secret, Map<String, Value>)>;
    /// The keyslot secret `token` holds.
    fn unlock(&mut self, token: &Map<String, Value>) -> Result<Secret>;
    /// Make the secret unreachable for the rest of this boot.
    fn seal_off(&mut self) -> Result<()>;
}

/// What an encrypted product does when no listed provider enrols at the first
/// format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NoProvider {
    /// Leave DATA unformatted and boot locked.
    Refuse,
    /// Format plain DATA, for good.
    Plain,
}

/// How to format a fresh DATA of a product with encryption on.
pub struct Format {
    pub cipher: Cipher,
    pub no_provider: NoProvider,
    /// The recovery keyslot's argon2id costs.
    pub recovery: Kdf,
    /// Whether the kernel has the cipher, or why not: checked before anything
    /// is enrolled or written.
    pub cipher_ready: Result<(), String>,
}

/// A provider's failure, by token type.
pub type Failures = Vec<(&'static str, String)>;

/// What became of DATA.
pub enum Opened {
    /// No header, and DATA stays plain: encryption off, or not a first
    /// format.
    Plain,
    /// No listed provider enrolled and the product allows plain DATA.
    PlainFallback { failures: Failures },
    /// A provider's token opened the volume.
    Unlocked {
        volume: Volume,
        key: VolumeKey,
        provider: &'static str,
    },
    /// A fresh DATA formatted: the providers that enrolled, and the recovery
    /// passphrase.
    Formatted {
        volume: Volume,
        key: VolumeKey,
        enrolled: Vec<&'static str>,
        recovery: Secret,
        failures: Failures,
    },
    /// No listed provider enrolled, and the product refuses plain DATA:
    /// DATA left untouched.
    NoProvider { failures: Failures },
    /// The kernel lacks the cipher: DATA left untouched, never plain.
    CipherUnavailable { why: String },
    /// A header, and no provider's token opens it.
    Locked { failures: Failures },
}

/// The outcome, with each provider's seal-off failure: seal-off runs for
/// every provider on every path.
pub struct Outcome {
    pub opened: Result<Opened>,
    pub seal_off_failures: Failures,
}

/// The first-boot keyslot KDF of a provider's high-entropy secret, as
/// systemd-cryptenroll uses.
const PROVIDER_KDF: Kdf = Kdf::Pbkdf2 { iterations: 1000 };
/// The recovery passphrase: groups of base32 characters.
const RECOVERY_GROUPS: usize = 8;
const RECOVERY_GROUP: usize = 5;
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// Open DATA on `device`, or format it when `format` is given (encryption on
/// and DATA fresh): the tokens are tried in `providers` order, the first
/// that unlocks wins. A header is honoured whatever `format` says. Then every
/// provider seals off, whatever happened.
pub fn open(
    device: &File,
    providers: &mut [&mut dyn KeyProvider],
    format: Option<&Format>,
) -> Outcome {
    let opened = open_or_format(device, providers, format);
    let seal_off_failures = providers
        .iter_mut()
        .filter_map(|provider| {
            provider
                .seal_off()
                .err()
                .map(|err| (provider.token_type(), format!("{err:#}")))
        })
        .collect();
    Outcome {
        opened,
        seal_off_failures,
    }
}

fn open_or_format(
    device: &File,
    providers: &mut [&mut dyn KeyProvider],
    format: Option<&Format>,
) -> Result<Opened> {
    if has_header(device)? {
        return unlock(device, providers);
    }
    let Some(format) = format else {
        return Ok(Opened::Plain);
    };
    if let Err(why) = &format.cipher_ready {
        return Ok(Opened::CipherUnavailable { why: why.clone() });
    }
    let mut failures = Failures::new();
    let mut enrolled = Vec::new();
    for provider in providers.iter_mut() {
        let kind = provider.token_type();
        if let Availability::Unavailable(why) = provider.available() {
            failures.push((kind, why));
            continue;
        }
        match provider.enroll() {
            Ok((secret, token)) => enrolled.push((kind, secret, token)),
            Err(err) => failures.push((kind, format!("{err:#}"))),
        }
    }
    if enrolled.is_empty() {
        return Ok(match format.no_provider {
            NoProvider::Refuse => Opened::NoProvider { failures },
            NoProvider::Plain => Opened::PlainFallback { failures },
        });
    }
    let recovery = recovery_passphrase()?;
    let mut slots: Vec<NewKeyslot<'_>> = enrolled
        .iter()
        .map(|(_, secret, token)| NewKeyslot {
            secret,
            kdf: PROVIDER_KDF,
            token: Some(token.clone()),
        })
        .collect();
    slots.push(NewKeyslot {
        secret: &recovery,
        kdf: format.recovery,
        token: None,
    });
    let (volume, key) = Volume::format(device, format.cipher, &uuid()?, &slots)?;
    Ok(Opened::Formatted {
        volume,
        key,
        enrolled: enrolled.iter().map(|(kind, ..)| *kind).collect(),
        recovery,
        failures,
    })
}

fn unlock(device: &File, providers: &mut [&mut dyn KeyProvider]) -> Result<Opened> {
    let volume = Volume::read(device)?;
    let mut failures = Failures::new();
    for provider in providers.iter_mut() {
        let kind = provider.token_type();
        let tokens: Vec<_> = volume
            .tokens()
            .into_iter()
            .filter(|(token, _)| token.get("type").and_then(Value::as_str) == Some(kind))
            .map(|(token, slots)| (token.clone(), slots))
            .collect();
        if tokens.is_empty() {
            failures.push((kind, "no token".into()));
            continue;
        }
        if let Availability::Unavailable(why) = provider.available() {
            failures.push((kind, why));
            continue;
        }
        for (token, slots) in tokens {
            let opened = provider.unlock(&token).and_then(|secret| {
                slots
                    .iter()
                    .find_map(|slot| volume.unlock(device, *slot, &secret).ok())
                    .ok_or_else(|| anyhow::anyhow!("its secret opens none of its keyslots"))
            });
            match opened {
                Ok(key) => {
                    return Ok(Opened::Unlocked {
                        volume,
                        key,
                        provider: kind,
                    });
                }
                Err(err) => failures.push((kind, format!("{err:#}"))),
            }
        }
    }
    Ok(Opened::Locked { failures })
}

/// A new recovery passphrase: 8 groups of 5 base32 characters (200 bits).
fn recovery_passphrase() -> Result<Secret> {
    let bytes = random(RECOVERY_GROUPS * RECOVERY_GROUP)?;
    let mut out = Zeroizing::new(Vec::with_capacity(RECOVERY_GROUPS * (RECOVERY_GROUP + 1)));
    for (at, byte) in bytes.iter().enumerate() {
        if at > 0 && at % RECOVERY_GROUP == 0 {
            out.push(b'-');
        }
        out.push(BASE32[usize::from(byte & 31)]);
    }
    Ok(out)
}

/// A random (version 4) volume uuid.
fn uuid() -> Result<String> {
    let mut bytes = random(16)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod tests;

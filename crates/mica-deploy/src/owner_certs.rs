//! The owner's enrolled certificates on SYSTEM (`docs/mica-core.md` 6.3 and
//! 6.5): checked and added or removed here, loaded into the owner keyring by
//! the runkit at the next boot (`boot::trust`), every change in an audit
//! record beside them.
//!
//! A certificate is `owner-certs/<sha256>.der`, the SHA-256 of its DER. The
//! audit record is `owner-certs/audit.json`, the last [`MAX_AUDIT`] changes,
//! written before the change it records; the runkit reads only the
//! certificates.

use crate::boot::trust::{
    CertState, MAX_OWNER_CERT_BYTES, MAX_OWNER_CERTS, OWNER_CERTS_DIR, Trust, owner_certs,
};
use crate::factory_reset::Requester;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use x509_parser::oid_registry::{OID_EC_P256, OID_NIST_EC_P384};
use x509_parser::prelude::{FromDer, X509Certificate, X509Version};
use x509_parser::public_key::PublicKey;

/// The audit record, in [`OWNER_CERTS_DIR`].
pub const AUDIT: &str = "audit.json";
/// The changes the audit record keeps.
pub const MAX_AUDIT: usize = 64;

/// What was done to a certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Add,
    Remove,
}

/// One change in the audit record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AuditEntry {
    /// Unix seconds, as the device's clock read them.
    pub at: u64,
    pub change: Change,
    pub sha256: String,
    pub subject: String,
    pub by: Requester,
    /// The peer or mechanism that asked.
    pub source: String,
    /// What was not checked: [`UNCHECKED_VALIDITY`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// The warning of an enrolment whose validity period was not checked.
pub const UNCHECKED_VALIDITY: &str =
    "the validity period was not checked: the device's clock is not synchronised";

/// What a certificate says, as far as an owner reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Described {
    pub subject: String,
    pub issuer: String,
    /// Unix seconds.
    pub not_before: i64,
    pub not_after: i64,
    /// `rsa-<bits>`, `ecdsa-p256` or `ecdsa-p384`.
    pub key: String,
}

/// Where an enrolled certificate stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Standing {
    /// In this boot's owner keyring.
    Loaded,
    /// Offered at this boot and refused; `reason` says why.
    Rejected,
    /// Enrolled after this boot: the next boot loads it.
    PendingNextBoot,
    /// The product requires a confirmation on the device's console, which
    /// no boot has had: a boot asks for it, and loads it once confirmed.
    PendingConfirmation,
    /// Removed after this boot, which loaded it: the next boot does not.
    RemovedNextBoot,
}

/// An enrolled certificate, or one this boot loaded and that is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Enrolled {
    pub sha256: String,
    pub state: Standing,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What it says, when it is still on SYSTEM and reads.
    #[serde(skip_serializing_if = "Option::is_none", flatten)]
    pub described: Option<Described>,
}

fn dir(system: &Path) -> PathBuf {
    system.join(OWNER_CERTS_DIR)
}

/// The DER of the one certificate in `bytes`, PEM or DER.
///
/// # Errors
///
/// Not a certificate, more than one, or too large.
pub fn der(bytes: &[u8]) -> Result<Vec<u8>> {
    let der = if bytes.starts_with(b"-----BEGIN") {
        let (rest, pem) = x509_parser::pem::parse_x509_pem(bytes)
            .map_err(|_| anyhow::anyhow!("not a PEM certificate"))?;
        ensure!(pem.label == "CERTIFICATE", "not a PEM certificate");
        ensure!(
            !String::from_utf8_lossy(rest).contains("-----BEGIN"),
            "one certificate at a time"
        );
        pem.contents
    } else {
        bytes.to_vec()
    };
    ensure!(
        der.len() as u64 <= MAX_OWNER_CERT_BYTES,
        "the certificate is larger than {MAX_OWNER_CERT_BYTES} bytes"
    );
    Ok(der)
}

/// Hold the DER certificate `der` to what an owner keyring takes: X.509 v3,
/// nothing after it, an RSA key of at least 2048 bits or an ECDSA key on
/// P-256 or P-384, and, when `now` (Unix seconds) is given, within its
/// validity period; and say what it says.
///
/// # Errors
///
/// Any of those that does not hold.
pub fn describe(der: &[u8], now: Option<i64>) -> Result<Described> {
    let (rest, cert) =
        X509Certificate::from_der(der).map_err(|_| anyhow::anyhow!("not an X.509 certificate"))?;
    ensure!(rest.is_empty(), "bytes after the certificate");
    ensure!(
        cert.version() == X509Version::V3,
        "not an X.509 v3 certificate"
    );
    let spki = cert.public_key();
    let key = match spki.parsed().context("unreadable public key")? {
        PublicKey::RSA(rsa) if rsa.key_size() >= 2048 => format!("rsa-{}", rsa.key_size()),
        PublicKey::RSA(_) => bail!("an RSA key of fewer than 2048 bits"),
        PublicKey::EC(_) => {
            let curve = spki
                .algorithm
                .parameters
                .as_ref()
                .and_then(|parameters| parameters.as_oid().ok());
            match curve {
                Some(oid) if oid == OID_EC_P256 => "ecdsa-p256".to_owned(),
                Some(oid) if oid == OID_NIST_EC_P384 => "ecdsa-p384".to_owned(),
                _ => bail!("an ECDSA key on a curve other than P-256 or P-384"),
            }
        }
        _ => bail!("neither an RSA nor an ECDSA key"),
    };
    let validity = cert.validity();
    let (not_before, not_after) = (
        validity.not_before.timestamp(),
        validity.not_after.timestamp(),
    );
    if let Some(now) = now {
        ensure!(not_before <= now, "the certificate is not valid yet");
        ensure!(now <= not_after, "the certificate has expired");
    }
    Ok(Described {
        subject: cert.subject().to_string(),
        issuer: cert.issuer().to_string(),
        not_before,
        not_after,
        key,
    })
}

/// The audit record on `system`; none is empty.
///
/// # Errors
///
/// A record that exists and cannot be read.
pub fn audit(system: &Path) -> Result<Vec<AuditEntry>> {
    let path = dir(system).join(AUDIT);
    if path.symlink_metadata().is_err() {
        return Ok(Vec::new());
    }
    serde_json::from_slice(&mica_fs::read_bounded(&path, 65536)?)
        .context("invalid owner certificate audit record")
}

fn record(system: &Path, entry: AuditEntry) -> Result<()> {
    let mut entries = audit(system)?;
    entries.push(entry);
    let excess = entries.len().saturating_sub(MAX_AUDIT);
    entries.drain(..excess);
    crate::deployments::atomic_write(&dir(system).join(AUDIT), &serde_json::to_vec(&entries)?)
}

/// Enrol the certificate in `bytes` (PEM or DER) on `system` for the next
/// boot, recorded as asked `by` from `source` at `at`; answer its SHA-256.
/// Its validity period is held to `now` when the device's clock is known to
/// be right; without it, the record carries [`UNCHECKED_VALIDITY`]. The caller holds the transaction lock and a writable SYSTEM.
///
/// # Errors
///
/// A certificate that does not hold ([`describe`]), one enrolled already, too
/// many, or a write that fails.
pub fn add(
    system: &Path,
    bytes: &[u8],
    now: Option<i64>,
    at: u64,
    by: Requester,
    source: &str,
) -> Result<String> {
    let der = der(bytes)?;
    let described = describe(&der, now)?;
    let sha256 = crate::components::sha256(&der);
    let enrolled = owner_certs(system)?;
    ensure!(
        !enrolled.iter().any(|(held, _)| *held == sha256),
        "the certificate is enrolled already"
    );
    ensure!(
        enrolled.len() < MAX_OWNER_CERTS,
        "at most {MAX_OWNER_CERTS} certificates are enrolled"
    );
    crate::deployments::directory(&dir(system))?;
    // The record first: no change goes unrecorded.
    record(
        system,
        AuditEntry {
            at,
            change: Change::Add,
            sha256: sha256.clone(),
            subject: described.subject,
            by,
            source: source.to_owned(),
            warning: now.is_none().then(|| UNCHECKED_VALIDITY.to_owned()),
        },
    )?;
    crate::deployments::atomic_write(&dir(system).join(format!("{sha256}.der")), &der)?;
    Ok(sha256)
}

/// Remove the enrolled certificate `sha256` from `system` for the next boot,
/// recorded as [`add`] records; answer whether it was enrolled. The caller
/// holds the transaction lock and a writable SYSTEM.
///
/// # Errors
///
/// An invalid digest, or a write that fails.
pub fn remove(system: &Path, sha256: &str, at: u64, by: Requester, source: &str) -> Result<bool> {
    crate::deployments::valid_id(sha256)?;
    let path = dir(system).join(format!("{sha256}.der"));
    if path.symlink_metadata().is_err() {
        return Ok(false);
    }
    let subject = mica_fs::read_bounded(&path, MAX_OWNER_CERT_BYTES)
        .ok()
        .and_then(|der| describe(&der, None).ok())
        .map(|described| described.subject)
        .unwrap_or_default();
    record(
        system,
        AuditEntry {
            at,
            change: Change::Remove,
            sha256: sha256.to_owned(),
            subject,
            by,
            source: source.to_owned(),
            warning: None,
        },
    )?;
    std::fs::remove_file(&path)?;
    mica_fs::sync_dir(&dir(system))?;
    Ok(true)
}

/// The certificates enrolled on `system`, each with where it stands against
/// what this boot did (`trust`, the runkit's record), and those this boot
/// loaded that are removed since. Where the product requires a confirmation
/// on the console, `confirmed` holds the ones confirmed so far.
///
/// # Errors
///
/// The enrolled certificates cannot be listed.
pub fn list(
    system: &Path,
    trust: Option<&Trust>,
    confirmed: Option<&std::collections::BTreeSet<String>>,
) -> Result<Vec<Enrolled>> {
    let booted = |sha256: &str| {
        trust.and_then(|trust| trust.owner_certs.iter().find(|cert| cert.sha256 == sha256))
    };
    let mut listed: Vec<Enrolled> = owner_certs(system)?
        .into_iter()
        .map(|(sha256, der)| {
            let (state, reason) = match booted(&sha256) {
                Some(cert) if cert.state == CertState::Loaded => (Standing::Loaded, None),
                Some(cert) if cert.state == CertState::Unconfirmed => {
                    (Standing::PendingConfirmation, None)
                }
                Some(cert) => (Standing::Rejected, cert.reason.clone()),
                None if confirmed.is_some_and(|confirmed| !confirmed.contains(&sha256)) => {
                    (Standing::PendingConfirmation, None)
                }
                None => (Standing::PendingNextBoot, None),
            };
            Enrolled {
                described: der.ok().and_then(|der| describe(&der, None).ok()),
                sha256,
                state,
                reason,
            }
        })
        .collect();
    for cert in trust.iter().flat_map(|trust| &trust.owner_certs) {
        if cert.state == CertState::Loaded && !listed.iter().any(|held| held.sha256 == cert.sha256)
        {
            listed.push(Enrolled {
                sha256: cert.sha256.clone(),
                state: Standing::RemovedNextBoot,
                reason: None,
                described: None,
            });
        }
    }
    Ok(listed)
}

#[cfg(test)]
mod tests;

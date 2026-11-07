//! Which keys the kernel trusts for dm-verity root hash signatures, and in
//! which order the runkit lets that trust grow (`docs/mica-core.md` 6.5).
//!
//! The kernel checks every root hash signature against its secondary
//! keyring, which holds its builtin keys and the owner keyring `.mica_owner`.
//! The owner keyring is empty and open until the runkit fills it with the
//! certificates enrolled on SYSTEM and seals it. So the runkit maps the
//! kernel's support image, the root and every core component while the
//! owner keyring is still empty, enrols and seals, and maps driver
//! extensions only after that. The order is in the types: a
//! [`BuiltinTrust`] admits the system's mappings and is spent by
//! [`enrol_and_seal`], and only the [`Sealed`] it answers admits an
//! extension. A process takes one [`BuiltinTrust`], so none is left to admit
//! anything after the seal.
//!
//! ```
//! use mica_deploy::boot::trust::{BuiltinTrust, Enrolment, OwnerKeyring, enrol_and_seal};
//! fn boot(keyring: &mut dyn OwnerKeyring, root: &[u8]) {
//!     let mut trust = BuiltinTrust::take().unwrap();
//!     trust.admit(root).unwrap();
//!     let sealed = enrol_and_seal(trust, keyring, Enrolment::None);
//!     let _extensions_may_map_now = &sealed;
//!     assert!(BuiltinTrust::take().is_none());
//! }
//! ```
//!
//! Nothing of the system maps once the owner's certificates are in:
//!
//! ```compile_fail
//! use mica_deploy::boot::trust::{BuiltinTrust, Enrolment, OwnerKeyring, enrol_and_seal};
//! fn boot(mut trust: BuiltinTrust, keyring: &mut dyn OwnerKeyring, core: &[u8]) {
//!     let _sealed = enrol_and_seal(trust, keyring, Enrolment::None);
//!     trust.admit(core).unwrap();
//! }
//! ```
//!
//! In userspace as well, a signature the system maps or installs is held to
//! the form mica signs in, which an owner's signature does not take
//! ([`system_signature`]): at boot by [`BuiltinTrust::admit`], and in the
//! running system for every root and core component it installs
//! ([`require_system_signature`]).

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// What the runkit records of this boot's trust: [`Trust`].
pub const TRUST: &str = "/run/mica/trust.json";
/// The owner's enrolled certificates on SYSTEM: `<sha256 of the DER>.der`.
pub const OWNER_CERTS_DIR: &str = "owner-certs";
/// The most certificates enrolled, and the most bytes one holds.
pub const MAX_OWNER_CERTS: usize = 16;
pub const MAX_OWNER_CERT_BYTES: u64 = 16384;
/// The kernel's owner keyring (mica-linux `CONFIG_MICA_OWNER_KEYRING`).
pub const OWNER_KEYRING: &str = ".mica_owner";

/// One DER element: its tag, its content, the whole element, and what
/// follows it.
type Element<'a> = (u8, &'a [u8], &'a [u8], &'a [u8]);

fn element(input: &[u8]) -> Result<Element<'_>> {
    let (&tag, rest) = input.split_first().context("truncated signature")?;
    let (&first, rest) = rest.split_first().context("truncated signature")?;
    let (length, rest) = match first {
        0..=0x7f => (usize::from(first), rest),
        0x81..=0x83 => {
            let width = usize::from(first & 0x7f);
            ensure!(rest.len() >= width, "truncated signature");
            let (bytes, rest) = rest.split_at(width);
            let length = bytes.iter().fold(0, |n, b| (n << 8) | usize::from(*b));
            ensure!(
                length >= 0x80 && bytes[0] != 0,
                "noncanonical signature length"
            );
            (length, rest)
        }
        _ => bail!("unsupported signature length"),
    };
    ensure!(rest.len() >= length, "truncated signature");
    let (content, after) = rest.split_at(length);
    let whole = &input[..input.len() - after.len()];
    Ok((tag, content, whole, after))
}

/// `1.2.840.113549.1.7.2`, PKCS#7 signed data.
const SIGNED_DATA: [u8; 9] = [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x07, 0x02];

/// What a PKCS#7 root hash signature says of its one signer: whether it
/// carries certificates, and the issuer and serial number of the signing
/// certificate (the contents of both, as the kernel looks the key up), or
/// `None` when it names the signer by its subject key identifier.
struct Signer<'a> {
    certificates: bool,
    issuer_serial: Option<(&'a [u8], &'a [u8])>,
}

fn signer(p7s: &[u8]) -> Result<Signer<'_>> {
    let (tag, info, _, rest) = element(p7s)?;
    ensure!(tag == 0x30 && rest.is_empty(), "invalid signature");
    let (tag, oid, _, rest) = element(info)?;
    ensure!(tag == 0x06 && oid == SIGNED_DATA, "not a PKCS#7 signature");
    let (tag, explicit, _, rest) = element(rest)?;
    ensure!(tag == 0xa0 && rest.is_empty(), "invalid signature");
    let (tag, mut signed, _, rest) = element(explicit)?;
    ensure!(tag == 0x30 && rest.is_empty(), "invalid signature");
    let mut fields = Vec::new();
    while !signed.is_empty() {
        let (tag, content, _, rest) = element(signed)?;
        fields.push((tag, content));
        signed = rest;
    }
    // version, digest algorithms, content, [certificates], [crls], signers
    let Some(&(0x31, signers)) = fields.last() else {
        bail!("invalid signature");
    };
    ensure!(
        fields.len() >= 4 && fields[0].0 == 0x02 && fields[1].0 == 0x31 && fields[2].0 == 0x30,
        "invalid signature"
    );
    let (tag, signer_info, _, rest) = element(signers)?;
    ensure!(tag == 0x30, "invalid signature");
    ensure!(rest.is_empty(), "a root hash signature has one signer");
    let (tag, _, _, rest) = element(signer_info)?;
    ensure!(tag == 0x02, "invalid signature");
    let (tag, identifier, _, _) = element(rest)?;
    let issuer_serial = match tag {
        0x30 => {
            let (tag, issuer, _, rest) = element(identifier)?;
            ensure!(tag == 0x30, "invalid signer identifier");
            let (tag, serial, _, rest) = element(rest)?;
            ensure!(tag == 0x02 && rest.is_empty(), "invalid signer identifier");
            Some((issuer, serial))
        }
        0x80 => None,
        _ => bail!("invalid signer identifier"),
    };
    Ok(Signer {
        certificates: fields[3..].iter().any(|(tag, _)| *tag == 0xa0),
        issuer_serial,
    })
}

/// The issuer and serial number of the DER certificate `der`, as [`Signer`]
/// has them.
fn certificate_issuer_serial(der: &[u8]) -> Result<(&[u8], &[u8])> {
    let (tag, certificate, _, rest) = element(der)?;
    ensure!(tag == 0x30 && rest.is_empty(), "invalid certificate");
    let (tag, tbs, _, _) = element(certificate)?;
    ensure!(tag == 0x30, "invalid certificate");
    let (mut tag, mut serial, _, mut rest) = element(tbs)?;
    if tag == 0xa0 {
        (tag, serial, _, rest) = element(rest)?;
    }
    ensure!(tag == 0x02, "invalid certificate");
    let (tag, _, _, rest) = element(rest)?;
    ensure!(tag == 0x30, "invalid certificate");
    let (tag, issuer, _, _) = element(rest)?;
    ensure!(tag == 0x30, "invalid certificate");
    Ok((issuer, serial))
}

/// Hold the root hash signature `p7s` of a mapping of the system (the
/// support image, a root, a core component) to the form mica signs in: one
/// signer, named by the issuer and serial number of a certificate the
/// kernel holds, and no certificates carried, by none of the owner's
/// `enrolled` certificates. An owner's signature by a key the kernel only
/// trusts through an enrolled certificate authority carries its chain.
///
/// # Errors
///
/// `p7s` is not a signature, or not in that form.
pub fn system_signature(p7s: &[u8], enrolled: &[Vec<u8>]) -> Result<()> {
    let signer = signer(p7s)?;
    let Some(issuer_serial) = signer.issuer_serial else {
        bail!("a system root hash signature names its signer by issuer and serial number");
    };
    ensure!(
        !signer.certificates,
        "a system root hash signature carries no certificates"
    );
    ensure!(
        !enrolled
            .iter()
            .any(|der| certificate_issuer_serial(der).is_ok_and(|cert| cert == issuer_serial)),
        "a system root hash signature is not by an enrolled owner certificate"
    );
    Ok(())
}

/// Whether this process has taken its [`BuiltinTrust`].
static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The system's root hash trust at this boot, before the owner's: it admits
/// the signatures of the support image, the root and the core components.
/// A process has one, so none is left once the seal has spent it.
#[derive(Debug)]
pub struct BuiltinTrust {
    _spent_by_the_seal: (),
}

impl BuiltinTrust {
    /// This process's trust; `None` once taken.
    #[must_use]
    pub fn take() -> Option<Self> {
        (!TAKEN.swap(true, std::sync::atomic::Ordering::SeqCst)).then_some(Self {
            _spent_by_the_seal: (),
        })
    }

    #[cfg(test)]
    fn unchecked() -> Self {
        Self {
            _spent_by_the_seal: (),
        }
    }

    /// Admit a mapping of the system with the root hash signature `p7s`,
    /// which the kernel checks against its builtin keys alone.
    ///
    /// # Errors
    ///
    /// `p7s` is not in the system's form ([`system_signature`]).
    #[allow(clippy::unused_self)]
    pub fn admit(&mut self, p7s: &[u8]) -> Result<()> {
        system_signature(p7s, &[])
    }
}

/// The kernel's owner keyring, as the runkit drives it.
pub trait OwnerKeyring {
    /// The keyring's serial; `None`, the kernel has none.
    ///
    /// # Errors
    ///
    /// The kernel's key list cannot be read.
    fn find(&mut self) -> Result<Option<i32>>;
    /// Add a DER certificate to it.
    ///
    /// # Errors
    ///
    /// The kernel refuses the certificate.
    fn add(&mut self, keyring: i32, der: &[u8]) -> std::io::Result<()>;
    /// Seal it; `EEXIST` when it is sealed already.
    ///
    /// # Errors
    ///
    /// The kernel refuses the seal.
    fn seal(&mut self, keyring: i32) -> std::io::Result<()>;
}

/// The running kernel's.
pub struct KernelKeyring;

impl OwnerKeyring for KernelKeyring {
    fn find(&mut self) -> Result<Option<i32>> {
        Ok(lifecycle_sys::keyring_named(
            &std::fs::read_to_string("/proc/keys")?,
            OWNER_KEYRING,
        ))
    }

    fn add(&mut self, keyring: i32, der: &[u8]) -> std::io::Result<()> {
        lifecycle_sys::add_asymmetric_key(keyring, der)?;
        Ok(())
    }

    fn seal(&mut self, keyring: i32) -> std::io::Result<()> {
        Ok(lifecycle_sys::restrict_keyring(keyring)?)
    }
}

/// What became of an enrolled certificate at this boot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CertState {
    /// The kernel trusts it.
    Loaded,
    /// The kernel or the runkit refused it; `reason` says why.
    Rejected,
    /// Not offered: the product requires a confirmation on the device's
    /// console, which this boot did not have ([`confirm_enrolment`]).
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OwnerCert {
    pub sha256: String,
    pub state: CertState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The owner keyring at the end of the enrolment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyringState {
    /// Sealed: nothing can be added to it until the next boot.
    Sealed,
    /// The kernel has no owner keyring: only its builtin keys are trusted.
    Absent,
}

/// This boot's trust, in [`TRUST`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Trust {
    pub owner_keyring: KeyringState,
    pub owner_certs: Vec<OwnerCert>,
}

/// The signed boot policy's product section, as far as the owner keyring
/// reads it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProductPolicy {
    #[serde(default)]
    pub owner_enrolment: OwnerEnrolmentPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OwnerEnrolmentPolicy {
    /// A certificate is offered only once someone at the device confirmed
    /// it on its console, at a boot.
    #[serde(default)]
    pub require_physical_confirmation: bool,
}

/// The certificates confirmed on the console, in DATA's `meta/`: the SHA-256
/// of each, a sorted JSON array. It goes with DATA, and the certificates
/// with it, at a factory reset.
pub const CONFIRMED: &str = "owner-confirmed.json";

/// The certificates confirmed in `meta`; a record that cannot be read
/// confirms none, which asks again.
#[must_use]
pub fn confirmed(meta: &Path) -> std::collections::BTreeSet<String> {
    mica_fs::read_bounded(&meta.join(CONFIRMED), 8192)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Replace the confirmed certificates in `meta`.
///
/// # Errors
///
/// A write that fails.
pub fn write_confirmed(meta: &Path, confirmed: &std::collections::BTreeSet<String>) -> Result<()> {
    crate::deployments::atomic_write(&meta.join(CONFIRMED), &serde_json::to_vec(confirmed)?)
}

/// Where a product requires a confirmation on the console: offer the
/// enrolled certificates (`listed`) confirmed at an earlier boot, and those
/// `ask` confirms now (given the SHA-256 and the DER), and hold the rest
/// back, answered by SHA-256; they are asked again at the next boot.
/// `confirmed` becomes the offered ones, so a certificate that is removed is
/// asked again when it is enrolled again. A certificate that cannot be read
/// is offered as it is, and refused.
pub fn confirm_enrolment(
    listed: Listed,
    confirmed: &mut std::collections::BTreeSet<String>,
    mut ask: impl FnMut(&str, &[u8]) -> bool,
) -> (Listed, Vec<String>) {
    let (mut offered, mut held) = (Vec::new(), Vec::new());
    let mut kept = std::collections::BTreeSet::new();
    for (sha256, der) in listed {
        let yes = match &der {
            Err(_) => true,
            Ok(der) => confirmed.contains(&sha256) || ask(&sha256, der),
        };
        if !yes {
            held.push(sha256);
            continue;
        }
        if der.is_ok() {
            kept.insert(sha256.clone());
        }
        offered.push((sha256, der));
    }
    *confirmed = kept;
    (offered, held)
}

/// What the runkit offers the owner keyring at this boot.
pub enum Enrolment {
    /// The certificates enrolled on SYSTEM ([`owner_certs`]).
    Load(Listed),
    /// None: the keyring is sealed empty.
    None,
}

/// The owner keyring sealed: from here an extension may map.
#[derive(Debug)]
pub struct Sealed {
    pub trust: Trust,
}

/// Add the owner's certificates to the kernel's owner keyring and seal it,
/// spending the system's trust: no system mapping is admitted after this.
/// The keyring is sealed whether or not a certificate was added, so nothing
/// can add one later in this boot.
///
/// # Panics
///
/// The kernel has an owner keyring and does not seal it: the boot cannot
/// go on with a keyring anything could add a key to.
#[allow(clippy::panic, clippy::needless_pass_by_value)]
pub fn enrol_and_seal(
    _trust: BuiltinTrust,
    keyring: &mut dyn OwnerKeyring,
    enrolment: Enrolment,
) -> Sealed {
    let ring = match keyring.find() {
        Ok(ring) => ring,
        Err(error) => panic!("the kernel's keys cannot be read: {error:#}"),
    };
    let mut certs = Vec::new();
    match enrolment {
        Enrolment::None => {}
        Enrolment::Load(listed) => {
            for (sha256, der) in listed {
                let outcome = match (ring, der) {
                    (_, Err(error)) => Err(format!("{error:#}")),
                    (None, Ok(_)) => Err("the kernel has no owner keyring".to_owned()),
                    (Some(ring), Ok(der)) => keyring
                        .add(ring, &der)
                        .map_err(|error| format!("the kernel refused it: {error}")),
                };
                certs.push(OwnerCert {
                    sha256,
                    state: if outcome.is_ok() {
                        CertState::Loaded
                    } else {
                        CertState::Rejected
                    },
                    reason: outcome.err(),
                });
            }
        }
    }
    let owner_keyring = match ring {
        None => KeyringState::Absent,
        Some(ring) => match keyring.seal(ring) {
            Ok(()) => KeyringState::Sealed,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => KeyringState::Sealed,
            Err(error) => panic!("the owner keyring cannot be sealed: {error}"),
        },
    };
    Sealed {
        trust: Trust {
            owner_keyring,
            owner_certs: certs,
        },
    }
}

/// Enrolled certificates by SHA-256, as [`owner_certs`] lists them.
pub type Listed = Vec<(String, Result<Vec<u8>>)>;

/// The certificates enrolled on the SYSTEM mounted at `system`, each by the
/// SHA-256 its file is named by, in name order: its DER, or why it is not
/// offered. At most [`MAX_OWNER_CERTS`] are listed.
///
/// # Errors
///
/// The directory exists and cannot be listed.
pub fn owner_certs(system: &Path) -> Result<Listed> {
    let directory = system.join(OWNER_CERTS_DIR);
    if directory.symlink_metadata().is_err() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        let name = entry?.file_name();
        if let Some(sha256) = name.to_str().and_then(|n| n.strip_suffix(".der"))
            && crate::deployments::valid_id(sha256).is_ok()
        {
            names.push(sha256.to_owned());
        }
    }
    names.sort();
    names.truncate(MAX_OWNER_CERTS);
    Ok(names
        .into_iter()
        .map(|sha256| {
            let der = mica_fs::read_bounded(
                &directory.join(format!("{sha256}.der")),
                MAX_OWNER_CERT_BYTES,
            )
            .map_err(anyhow::Error::from)
            .and_then(|der| {
                ensure!(
                    crate::components::sha256(&der) == sha256,
                    "the certificate is not the one its name says"
                );
                Ok(der)
            });
            (sha256, der)
        })
        .collect())
}

/// What the runkit recorded at `path` (normally [`TRUST`]), if anything.
///
/// # Errors
///
/// A record that exists and cannot be read.
pub fn recorded(path: &Path) -> Result<Option<Trust>> {
    if path.symlink_metadata().is_err() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&mica_fs::read_bounded(
        path, 65536,
    )?)?))
}

/// Hold the root hash signature `p7s` of a root or a core component the
/// running system is about to install to the system's form
/// ([`system_signature`]), with the owner certificates enrolled on the
/// SYSTEM at `system`, where this boot's runkit sealed an owner keyring (its
/// record at `trust`, normally [`TRUST`]). Without the record, from a runkit
/// that enrols nothing, the kernel trusts no owner's key.
///
/// # Errors
///
/// `p7s` is not in the system's form, or the record cannot be read.
pub fn require_system_signature(trust: &Path, system: &Path, p7s: &[u8]) -> Result<()> {
    if recorded(trust)?.is_none() {
        return Ok(());
    }
    let enrolled: Vec<Vec<u8>> = owner_certs(system)?
        .into_iter()
        .filter_map(|(_, der)| der.ok())
        .collect();
    system_signature(p7s, &enrolled)
}

#[cfg(test)]
mod tests;

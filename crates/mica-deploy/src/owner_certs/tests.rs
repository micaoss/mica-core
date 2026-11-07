// Tests and fixtures answer a broken expectation by panicking.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The fixtures are self-signed test certificates made with openssl, whose
//! keys were not kept.

use super::*;
use crate::boot::trust::{KeyringState, OwnerCert};

const P256: &[u8] = include_bytes!("ecdsa-p256.der");
const P256_PEM: &[u8] = include_bytes!("ecdsa-p256.pem");
const P384: &[u8] = include_bytes!("ecdsa-p384.der");
const RSA_2048: &[u8] = include_bytes!("rsa-2048.der");

fn add_to(system: &Path, bytes: &[u8]) -> Result<String> {
    add(system, bytes, None, 7, Requester::Console, "192.0.2.1")
}

#[test]
fn rsa_of_2048_bits_and_ecdsa_on_p256_or_p384_are_taken() {
    for (bytes, key) in [
        (RSA_2048, "rsa-2048"),
        (P256, "ecdsa-p256"),
        (P384, "ecdsa-p384"),
    ] {
        let described = describe(bytes, None).unwrap();
        assert_eq!(described.key, key);
        assert_eq!(described.subject, format!("CN={key}"));
    }
    for (bytes, why) in [
        (&include_bytes!("rsa-1024.der")[..], "fewer than 2048"),
        (include_bytes!("ecdsa-p521.der"), "other than P-256"),
        (include_bytes!("ed25519.der"), "neither"),
        (b"not a certificate", "not an X.509"),
    ] {
        let error = describe(bytes, None).unwrap_err().to_string();
        assert!(error.contains(why), "{error}");
    }
    let mut trailing = P256.to_vec();
    trailing.push(0);
    assert!(describe(&trailing, None).is_err());
}

#[test]
fn the_validity_period_is_held_to_a_known_clock() {
    let described = describe(P256, None).unwrap();
    assert!(describe(P256, Some(described.not_before)).is_ok());
    assert!(describe(P256, Some(described.not_after)).is_ok());
    let error = describe(P256, Some(described.not_before - 1))
        .unwrap_err()
        .to_string();
    assert!(error.contains("not valid yet"), "{error}");
    let error = describe(P256, Some(described.not_after + 1))
        .unwrap_err()
        .to_string();
    assert!(error.contains("expired"), "{error}");
}

#[test]
fn pem_is_read_as_its_der() {
    assert_eq!(der(P256_PEM).unwrap(), P256);
    assert_eq!(der(P256).unwrap(), P256);
    let mut two = P256_PEM.to_vec();
    two.extend(P256_PEM);
    assert!(
        der(&two)
            .unwrap_err()
            .to_string()
            .contains("one certificate")
    );
    assert!(der(&vec![0x30; 16385]).is_err());
}

#[test]
fn a_certificate_is_enrolled_for_the_next_boot_and_audited() {
    let dir = tempfile::TempDir::new().unwrap();
    let system = dir.path();
    let sha256 = add_to(system, P256_PEM).unwrap();
    assert_eq!(sha256, crate::components::sha256(P256));
    assert_eq!(
        std::fs::read(system.join(format!("owner-certs/{sha256}.der"))).unwrap(),
        P256
    );
    let error = add_to(system, P256).unwrap_err().to_string();
    assert!(error.contains("enrolled already"), "{error}");
    let listed = list(system, None, None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, Standing::PendingNextBoot);
    assert_eq!(listed[0].described.as_ref().unwrap().key, "ecdsa-p256");

    assert!(remove(system, &sha256, 8, Requester::Api, "token").unwrap());
    assert!(!remove(system, &sha256, 9, Requester::Api, "token").unwrap());
    assert!(remove(system, "../x", 9, Requester::Api, "token").is_err());
    let audit = audit(system).unwrap();
    assert_eq!(
        audit
            .iter()
            .map(|entry| (entry.change, entry.at, entry.subject.as_str()))
            .collect::<Vec<_>>(),
        [
            (Change::Add, 7, "CN=ecdsa-p256"),
            (Change::Remove, 8, "CN=ecdsa-p256")
        ]
    );
    assert_eq!(audit[0].warning.as_deref(), Some(UNCHECKED_VALIDITY));
    let described = describe(P384, None).unwrap();
    add(
        system,
        P384,
        Some(described.not_before),
        9,
        Requester::Cli,
        "cli",
    )
    .unwrap();
    assert_eq!(super::audit(system).unwrap()[2].warning, None);
}

#[test]
fn enrolment_is_bounded_and_the_audit_keeps_the_last_changes() {
    let dir = tempfile::TempDir::new().unwrap();
    let system = dir.path();
    std::fs::create_dir_all(system.join(OWNER_CERTS_DIR)).unwrap();
    for n in 0..MAX_OWNER_CERTS {
        std::fs::write(system.join(format!("owner-certs/{n:064x}.der")), b"").unwrap();
    }
    // Sixteen held, whatever they are, leave no room.
    let held = std::fs::read_dir(system.join(OWNER_CERTS_DIR))
        .unwrap()
        .count();
    assert_eq!(held, MAX_OWNER_CERTS);
    let error = add_to(system, P256).unwrap_err().to_string();
    assert!(error.contains("at most"), "{error}");

    let dir = tempfile::TempDir::new().unwrap();
    let system = dir.path();
    let sha256 = add_to(system, P256).unwrap();
    for at in 0..MAX_AUDIT as u64 {
        remove(system, &sha256, at, Requester::Cli, "cli").unwrap();
        add(system, P256, None, at, Requester::Cli, "cli").unwrap();
    }
    assert_eq!(audit(system).unwrap().len(), MAX_AUDIT);
}

#[test]
fn each_certificate_stands_as_this_boot_left_it() {
    let dir = tempfile::TempDir::new().unwrap();
    let system = dir.path();
    let loaded = add_to(system, P256).unwrap();
    let rejected = add_to(system, P384).unwrap();
    let gone = "e".repeat(64);
    let trust = Trust {
        owner_keyring: KeyringState::Sealed,
        owner_certs: vec![
            OwnerCert {
                sha256: loaded.clone(),
                state: CertState::Loaded,
                reason: None,
            },
            OwnerCert {
                sha256: rejected.clone(),
                state: CertState::Rejected,
                reason: Some("EKEYREJECTED".into()),
            },
            OwnerCert {
                sha256: gone.clone(),
                state: CertState::Loaded,
                reason: None,
            },
        ],
    };
    let pending = add_to(system, RSA_2048).unwrap();
    let states: Vec<_> = list(system, Some(&trust), None)
        .unwrap()
        .into_iter()
        .map(|cert| (cert.sha256, cert.state, cert.reason))
        .collect();
    for expected in [
        (loaded, Standing::Loaded, None),
        (
            rejected,
            Standing::Rejected,
            Some("EKEYREJECTED".to_owned()),
        ),
        (pending, Standing::PendingNextBoot, None),
        (gone, Standing::RemovedNextBoot, None),
    ] {
        assert!(states.contains(&expected), "{expected:?} in {states:?}");
    }
}

/// Where the product requires a confirmation on the console, a certificate
/// no boot has confirmed waits for one.
#[test]
fn a_certificate_waits_for_its_confirmation_where_one_is_required() {
    let dir = tempfile::TempDir::new().unwrap();
    let system = dir.path();
    let waiting = add_to(system, P256).unwrap();
    let confirmed_once = add_to(system, P384).unwrap();
    let confirmed = std::collections::BTreeSet::from([confirmed_once.clone()]);
    let trust = Trust {
        owner_keyring: KeyringState::Sealed,
        owner_certs: vec![OwnerCert {
            sha256: waiting.clone(),
            state: CertState::Unconfirmed,
            reason: Some("not confirmed".into()),
        }],
    };
    let states: Vec<_> = list(system, Some(&trust), Some(&confirmed))
        .unwrap()
        .into_iter()
        .map(|cert| (cert.sha256, cert.state))
        .collect();
    assert!(states.contains(&(waiting, Standing::PendingConfirmation)));
    assert!(states.contains(&(confirmed_once, Standing::PendingNextBoot)));
    let fresh = add_to(system, RSA_2048).unwrap();
    let listed = list(system, None, Some(&confirmed)).unwrap();
    assert!(
        listed
            .iter()
            .any(|cert| cert.sha256 == fresh && cert.state == Standing::PendingConfirmation)
    );
}

use super::*;

/// Real root hash signatures of mica-build's lifecycle suite: the root's and
/// the support image's by the development verity key, and an extension's by
/// an owner's signing key with its CA chain; and the certificates of the
/// development verity key and of that owner's CA.
const ROOT: &[u8] = include_bytes!("mica-root.p7s");
const SUPPORT: &[u8] = include_bytes!("mica-support.p7s");
const OWNER: &[u8] = include_bytes!("owner-ext.p7s");
const MICA_CERT: &[u8] = include_bytes!("mica-content.der");
const OWNER_CA: &[u8] = include_bytes!("owner-ca.der");

#[derive(Default)]
struct Fake {
    ring: Option<i32>,
    refuse: Vec<Vec<u8>>,
    seal: Option<std::io::ErrorKind>,
    added: Vec<Vec<u8>>,
    sealed: bool,
    calls: Vec<&'static str>,
}

impl OwnerKeyring for Fake {
    fn find(&mut self) -> Result<Option<i32>> {
        self.calls.push("find");
        Ok(self.ring)
    }
    fn add(&mut self, keyring: i32, der: &[u8]) -> std::io::Result<()> {
        self.calls.push("add");
        assert_eq!(Some(keyring), self.ring);
        assert!(!self.sealed, "added after the seal");
        if self.refuse.iter().any(|r| r == der) {
            return Err(std::io::Error::from_raw_os_error(74));
        }
        self.added.push(der.to_vec());
        Ok(())
    }
    fn seal(&mut self, keyring: i32) -> std::io::Result<()> {
        self.calls.push("seal");
        assert_eq!(Some(keyring), self.ring);
        if let Some(kind) = self.seal {
            return Err(kind.into());
        }
        self.sealed = true;
        Ok(())
    }
}

#[test]
fn a_system_signature_is_in_the_form_mica_signs_in() {
    for p7s in [ROOT, SUPPORT] {
        system_signature(p7s, &[]).unwrap();
        system_signature(p7s, &[OWNER_CA.to_vec(), b"not a certificate".to_vec()]).unwrap();
    }
    let error = system_signature(OWNER, &[]).unwrap_err().to_string();
    assert!(error.contains("carries no certificates"), "{error}");
    // The issuer and serial number the kernel looks the key up by.
    let (issuer, serial) = certificate_issuer_serial(MICA_CERT).unwrap();
    assert_eq!(signer(ROOT).unwrap().issuer_serial, Some((issuer, serial)));
    let error = system_signature(ROOT, &[MICA_CERT.to_vec()])
        .unwrap_err()
        .to_string();
    assert!(error.contains("enrolled owner certificate"), "{error}");
    for bad in [
        &ROOT[..ROOT.len() - 1],
        &ROOT[1..],
        b"",
        &[0x30, 0x80, 0, 0][..],
    ] {
        assert!(system_signature(bad, &[]).is_err());
    }
    // Trailing bytes after the signature are not a signature.
    let mut longer = ROOT.to_vec();
    longer.push(0);
    assert!(system_signature(&longer, &[]).is_err());
}

#[test]
fn the_system_maps_only_signatures_in_its_form() {
    let mut trust = BuiltinTrust::unchecked();
    trust.admit(ROOT).unwrap();
    trust.admit(SUPPORT).unwrap();
    assert!(trust.admit(OWNER).is_err());
    assert!(trust.admit(b"not a signature").is_err());
}

#[test]
fn every_enrolled_certificate_goes_in_and_the_keyring_is_sealed_last() {
    let mut keyring = Fake {
        ring: Some(7),
        refuse: vec![b"bad".to_vec()],
        ..Fake::default()
    };
    let mut trust = BuiltinTrust::unchecked();
    trust.admit(ROOT).unwrap();
    let sealed = enrol_and_seal(
        trust,
        &mut keyring,
        Enrolment::Load(vec![
            ("a".repeat(64), Ok(b"good".to_vec())),
            ("b".repeat(64), Ok(b"bad".to_vec())),
            ("c".repeat(64), Err(anyhow::anyhow!("unreadable"))),
        ]),
    );
    assert_eq!(keyring.calls, ["find", "add", "add", "seal"]);
    assert_eq!(keyring.added, [b"good".to_vec()]);
    assert!(keyring.sealed);
    let trust = sealed.trust;
    assert_eq!(trust.owner_keyring, KeyringState::Sealed);
    let states: Vec<_> = trust.owner_certs.iter().map(|c| c.state.clone()).collect();
    assert_eq!(
        states,
        [CertState::Loaded, CertState::Rejected, CertState::Rejected]
    );
    assert!(trust.owner_certs[0].reason.is_none());
    assert!(
        trust.owner_certs[1]
            .reason
            .as_deref()
            .unwrap()
            .contains("refused")
    );
    assert_eq!(trust.owner_certs[2].reason.as_deref(), Some("unreadable"));
}

#[test]
fn the_keyring_is_sealed_with_nothing_to_enrol_and_a_seal_in_place_counts() {
    let mut keyring = Fake {
        ring: Some(3),
        ..Fake::default()
    };
    let sealed = enrol_and_seal(BuiltinTrust::unchecked(), &mut keyring, Enrolment::None);
    assert_eq!(keyring.calls, ["find", "seal"]);
    assert_eq!(sealed.trust.owner_keyring, KeyringState::Sealed);
    assert!(sealed.trust.owner_certs.is_empty());

    let mut keyring = Fake {
        ring: Some(3),
        seal: Some(std::io::ErrorKind::AlreadyExists),
        ..Fake::default()
    };
    let sealed = enrol_and_seal(BuiltinTrust::unchecked(), &mut keyring, Enrolment::None);
    assert_eq!(sealed.trust.owner_keyring, KeyringState::Sealed);
}

#[test]
fn a_kernel_without_the_keyring_trusts_its_builtin_keys_alone() {
    let mut keyring = Fake::default();
    let sealed = enrol_and_seal(
        BuiltinTrust::unchecked(),
        &mut keyring,
        Enrolment::Load(vec![("a".repeat(64), Ok(b"good".to_vec()))]),
    );
    assert_eq!(keyring.calls, ["find"]);
    assert_eq!(sealed.trust.owner_keyring, KeyringState::Absent);
    assert_eq!(sealed.trust.owner_certs[0].state, CertState::Rejected);
}

#[test]
#[should_panic(expected = "the owner keyring cannot be sealed")]
fn a_keyring_that_cannot_be_sealed_stops_the_boot() {
    let mut keyring = Fake {
        ring: Some(3),
        seal: Some(std::io::ErrorKind::PermissionDenied),
        ..Fake::default()
    };
    enrol_and_seal(BuiltinTrust::unchecked(), &mut keyring, Enrolment::None);
}

#[test]
fn enrolled_certificates_are_read_by_their_digest_in_name_order() {
    let system = tempfile::tempdir().unwrap();
    assert!(
        owner_certs(system.path()).unwrap().is_empty(),
        "no directory"
    );
    let dir = system.path().join(OWNER_CERTS_DIR);
    std::fs::create_dir(&dir).unwrap();
    let good = b"certificate one".to_vec();
    let good_sha = crate::components::sha256(&good);
    std::fs::write(dir.join(format!("{good_sha}.der")), &good).unwrap();
    std::fs::write(dir.join(format!("{}.der", "0".repeat(64))), b"renamed").unwrap();
    std::fs::write(dir.join("notes.txt"), b"ignored").unwrap();
    std::fs::write(dir.join("ZZ.der"), b"ignored").unwrap();
    let listed = owner_certs(system.path()).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].0, "0".repeat(64));
    assert!(listed[0].1.is_err(), "its digest is not its name");
    let found = listed.iter().find(|(sha, _)| *sha == good_sha).unwrap();
    assert_eq!(found.1.as_ref().unwrap(), &good);

    for n in 0..MAX_OWNER_CERTS {
        std::fs::write(dir.join(format!("{n:064x}.der")), b"x").unwrap();
    }
    assert_eq!(owner_certs(system.path()).unwrap().len(), MAX_OWNER_CERTS);
}

#[test]
fn the_running_system_installs_only_signatures_in_the_systems_form() {
    let dir = tempfile::tempdir().unwrap();
    let (record, system) = (dir.path().join("trust.json"), dir.path().join("system"));
    std::fs::create_dir(&system).unwrap();
    // Without a record, from a runkit that enrols nothing, nothing is held.
    require_system_signature(&record, &system, OWNER).unwrap();
    let trust = Trust {
        owner_keyring: KeyringState::Sealed,
        owner_certs: Vec::new(),
    };
    std::fs::write(&record, serde_json::to_vec(&trust).unwrap()).unwrap();
    require_system_signature(&record, &system, ROOT).unwrap();
    assert!(require_system_signature(&record, &system, OWNER).is_err());
    let certs = system.join(OWNER_CERTS_DIR);
    std::fs::create_dir(&certs).unwrap();
    std::fs::write(
        certs.join(format!("{}.der", crate::components::sha256(MICA_CERT))),
        MICA_CERT,
    )
    .unwrap();
    let error = require_system_signature(&record, &system, ROOT)
        .unwrap_err()
        .to_string();
    assert!(error.contains("enrolled owner certificate"), "{error}");
    std::fs::write(&record, b"{").unwrap();
    assert!(require_system_signature(&record, &system, SUPPORT).is_err());
}

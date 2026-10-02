use std::collections::HashSet;

use tempfile::TempDir;

use super::*;

/// Characters an operator cannot reliably read off a printed label.
const AMBIGUOUS: &str = "0Oo1lI";

fn provision() -> (TempDir, Settings) {
    let dir = TempDir::new().expect("tempdir");
    let mut settings = Settings::default();
    let outcome = ensure_identity(dir.path(), &mut settings).expect("ensure_identity");
    assert_eq!(outcome, Outcome::Created);
    (dir, settings)
}

fn secret_bytes(state_dir: &Path, name: &str) -> Vec<u8> {
    fs::read(secrets_dir(state_dir).join(name)).expect("read secret file")
}

fn mode_of(path: &Path) -> u32 {
    fs::metadata(path).expect("stat").permissions().mode() & 0o7777
}

// A fresh STATE gets a complete identity, and the stored hash is really
// the hash of the plaintext that was written next to it.
#[test]
fn fresh_state_gets_identity_and_both_secrets() {
    let (dir, settings) = provision();

    let device_id = settings
        .provisioning
        .device_id
        .as_deref()
        .expect("device_id set");
    assert_eq!(device_id.len(), DEVICE_ID_BYTES * 2);
    assert!(
        device_id
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "device_id must be lowercase hex, got {device_id}"
    );

    let hash = settings
        .access
        .device
        .password_hash
        .as_deref()
        .expect("password_hash set");
    assert!(
        hash.starts_with("$argon2id$"),
        "not an argon2id PHC: {hash}"
    );
    assert_eq!(settings.access.device.generation, 1);

    let password = read_device_password(dir.path())
        .expect("read password")
        .expect("password present");
    let psk = read_ap_psk(dir.path())
        .expect("read psk")
        .expect("psk present");
    assert_eq!(password.len(), SECRET_LEN);
    assert_eq!(psk.len(), SECRET_LEN);
    assert_ne!(password, psk);

    assert!(
        verify_password(hash, &password),
        "stored hash does not verify against the stored plaintext"
    );

    // Untouched: declaring first boot finished is the caller's call.
    assert_eq!(
        settings.provisioning.state,
        micad_settings::ProvisioningState::Pending
    );
}

// A second call must not regenerate anything: a regenerated credential
// locks the operator out of a fielded device.
#[test]
fn second_call_is_a_genuine_no_op() {
    let (dir, first) = provision();
    let password_before = secret_bytes(dir.path(), DEVICE_PASSWORD_FILE);
    let psk_before = secret_bytes(dir.path(), AP_PSK_FILE);

    let mut second = first.clone();
    let outcome = ensure_identity(dir.path(), &mut second).expect("ensure_identity");

    assert_eq!(outcome, Outcome::AlreadyPresent);
    assert_eq!(second, first, "settings tree changed on a re-run");
    assert_eq!(
        secret_bytes(dir.path(), DEVICE_PASSWORD_FILE),
        password_before
    );
    assert_eq!(secret_bytes(dir.path(), AP_PSK_FILE), psk_before);
}

// Interrupted first boot that got as far as the device_id: only the
// credential half is completed.
#[test]
fn partial_state_with_device_id_only_completes_the_credential() {
    let dir = TempDir::new().expect("tempdir");
    let mut settings = Settings::default();
    settings.provisioning.device_id = Some("00112233445566778899aabbccddeeff".to_string());

    let outcome = ensure_identity(dir.path(), &mut settings).expect("ensure_identity");

    assert_eq!(outcome, Outcome::Created);
    assert_eq!(
        settings.provisioning.device_id.as_deref(),
        Some("00112233445566778899aabbccddeeff"),
        "the present half was regenerated"
    );
    let hash = settings
        .access
        .device
        .password_hash
        .as_deref()
        .expect("password_hash completed");
    let password = read_device_password(dir.path())
        .expect("read password")
        .expect("password present");
    assert!(verify_password(hash, &password));
    assert_eq!(settings.access.device.generation, 1);
    assert!(read_ap_psk(dir.path()).expect("read psk").is_some());
}

// The other direction: credential present, device_id missing. The
// credential must come through byte-identical, hash and plaintext alike.
#[test]
fn partial_state_with_credential_only_completes_the_device_id() {
    let (dir, first) = provision();
    let password_before = secret_bytes(dir.path(), DEVICE_PASSWORD_FILE);
    let psk_before = secret_bytes(dir.path(), AP_PSK_FILE);

    let mut settings = first.clone();
    settings.provisioning.device_id = None;
    let outcome = ensure_identity(dir.path(), &mut settings).expect("ensure_identity");

    assert_eq!(outcome, Outcome::Created);
    assert_eq!(
        settings.access.device, first.access.device,
        "the credential half was rewritten"
    );
    assert_eq!(
        secret_bytes(dir.path(), DEVICE_PASSWORD_FILE),
        password_before
    );
    assert_eq!(secret_bytes(dir.path(), AP_PSK_FILE), psk_before);
    let device_id = settings
        .provisioning
        .device_id
        .as_deref()
        .expect("device_id completed");
    assert_eq!(device_id.len(), DEVICE_ID_BYTES * 2);
    assert_ne!(
        Some(device_id),
        first.provisioning.device_id.as_deref(),
        "a fresh draw should not reproduce the old identifier"
    );
}

// The central guard: nothing generated here may be a fleet-wide constant,
// and the two secrets of one device must be independent draws rather than
// one string used twice.
#[test]
fn two_hundred_devices_share_no_secret() {
    const DEVICES: usize = 200;
    // Argon2id at apid's parameters is deliberately expensive, and an
    // unoptimised test build pays that 200 times; spread it over the
    // machine so the check gate stays usable.
    let lanes = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);

    let drawn: Vec<(String, String, String)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..lanes)
            .map(|lane| {
                let count = DEVICES / lanes + usize::from(lane < DEVICES % lanes);
                scope.spawn(move || {
                    (0..count)
                        .map(|_| {
                            let (dir, settings) = provision();
                            let password = read_device_password(dir.path())
                                .expect("read password")
                                .expect("password present");
                            let psk = read_ap_psk(dir.path())
                                .expect("read psk")
                                .expect("psk present");
                            let device_id = settings.provisioning.device_id.expect("device_id set");
                            (password, psk, device_id)
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("lane panicked"))
            .collect()
    });
    assert_eq!(drawn.len(), DEVICES);

    let mut passwords = HashSet::new();
    let mut psks = HashSet::new();
    let mut device_ids = HashSet::new();
    for (password, psk, device_id) in drawn {
        assert_ne!(
            password, psk,
            "the device password and the AP PSK are the same string"
        );
        passwords.insert(password);
        psks.insert(psk);
        device_ids.insert(device_id);
    }

    assert_eq!(passwords.len(), DEVICES, "device passwords repeat");
    assert_eq!(psks.len(), DEVICES, "AP PSKs repeat");
    assert_eq!(device_ids.len(), DEVICES, "device identifiers repeat");
    assert_eq!(
        passwords.union(&psks).count(),
        DEVICES * 2,
        "a password on one device is an AP PSK on another"
    );
}

// Read the bits back rather than trusting the create call: an inherited
// umask or a temp file left by an earlier run could widen either. The
// expected modes are written out as literals, not as the constants the
// implementation uses, so that widening a constant fails here instead of
// silently moving the goalposts.
#[test]
fn secret_files_are_0600_inside_a_0700_directory() {
    let (dir, _) = provision();
    let secrets = secrets_dir(dir.path());

    assert_eq!(mode_of(&secrets), 0o700);
    assert_eq!(mode_of(&secrets.join(DEVICE_PASSWORD_FILE)), 0o600);
    assert_eq!(mode_of(&secrets.join(AP_PSK_FILE)), 0o600);
}

// A pre-existing world-readable directory and a leftover temp file from an
// interrupted run must both be tightened, not inherited.
#[test]
fn a_lax_pre_existing_secrets_directory_is_tightened() {
    let dir = TempDir::new().expect("tempdir");
    let secrets = secrets_dir(dir.path());
    fs::create_dir_all(&secrets).expect("mkdir");
    fs::set_permissions(&secrets, Permissions::from_mode(0o777)).expect("chmod");
    let stale = secrets.join(format!(".{DEVICE_PASSWORD_FILE}.tmp"));
    fs::write(&stale, b"stale").expect("write stale temp");
    fs::set_permissions(&stale, Permissions::from_mode(0o666)).expect("chmod stale");

    let mut settings = Settings::default();
    ensure_identity(dir.path(), &mut settings).expect("ensure_identity");

    assert_eq!(mode_of(&secrets), 0o700);
    assert_eq!(mode_of(&secrets.join(DEVICE_PASSWORD_FILE)), 0o600);
    assert!(!stale.exists(), "temp file left behind after rename");
}

// Alphabet and length, over enough characters that a stray symbol would
// show up.
#[test]
fn generated_secrets_use_only_the_unambiguous_alphabet() {
    let rng = SystemRandom::new();
    let alphabet: HashSet<char> = ALPHABET.iter().map(|&b| char::from(b)).collect();
    assert_eq!(alphabet.len(), 32, "alphabet is not 32 distinct characters");
    for c in AMBIGUOUS.chars() {
        assert!(!alphabet.contains(&c), "ambiguous {c} is in the alphabet");
    }

    let mut seen = 0usize;
    for _ in 0..250 {
        let secret = generate_secret(&rng, SECRET_LEN).expect("generate");
        assert_eq!(secret.len(), SECRET_LEN);
        for c in secret.chars() {
            assert!(alphabet.contains(&c), "{c} is outside the alphabet");
            assert!(!AMBIGUOUS.contains(c), "ambiguous {c} was generated");
            seen += 1;
        }
    }
    assert_eq!(
        seen,
        250 * SECRET_LEN,
        "fewer characters checked than drawn"
    );
}

// A near miss is a miss.
#[test]
fn verify_password_rejects_a_one_character_miss() {
    let (dir, settings) = provision();
    let hash = settings
        .access
        .device
        .password_hash
        .expect("password_hash set");
    let password = read_device_password(dir.path())
        .expect("read password")
        .expect("password present");

    assert!(verify_password(&hash, &password));

    let mut near_miss = password.clone();
    let last = near_miss.pop().expect("non-empty");
    near_miss.push(if last == '2' { '3' } else { '2' });
    assert_ne!(near_miss, password);
    assert!(!verify_password(&hash, &near_miss), "near miss accepted");
    assert!(!verify_password("not a phc string", &password));
}

// Assert the invariant on the document that actually lands on STATE, rather
// than trusting the types to have kept the plaintext out.
#[test]
fn settings_document_contains_no_plaintext_secret() {
    let (dir, settings) = provision();
    let password = read_device_password(dir.path())
        .expect("read password")
        .expect("password present");
    let psk = read_ap_psk(dir.path())
        .expect("read psk")
        .expect("psk present");

    let document = toml::to_string(&settings).expect("serialize settings");

    assert!(
        !document.contains(&password),
        "the device password is in settings.toml:\n{document}"
    );
    assert!(
        !document.contains(&psk),
        "the AP PSK is in settings.toml:\n{document}"
    );
    assert!(
        document.contains("passwordHash"),
        "the hash should be there:\n{document}"
    );
}

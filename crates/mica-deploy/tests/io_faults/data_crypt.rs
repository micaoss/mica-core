//! The DATA LUKS2 header and keyslot writes interrupted at every IO boundary:
//! a cut leaves a volume every listed keyslot opens, or no header at all.

use mica_deploy::data_crypt::{Cipher, Kdf, NewKeyslot, Volume, has_header};
use serde_json::{Map, Value};
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

const TPM: &[u8] = b"a sealed secret, base64 in a real one";
const RECOVERY: &[u8] = b"AAAAA-BBBBB-CCCCC-DDDDD-EEEEE-FFFFF-GGGGG-HHHHH";
const STAGED: &[u8] = b"a staged secret";
/// The smallest argon2id costs, so a case stays fast.
const ARGON: Kdf = Kdf::Argon2id {
    time: 1,
    memory_kib: 64,
    cpus: 1,
};
const PBKDF2: Kdf = Kdf::Pbkdf2 { iterations: 1000 };

fn token() -> Map<String, Value> {
    let Value::Object(token) = serde_json::json!({ "type": "systemd-tpm2" }) else {
        unreachable!()
    };
    token
}

fn device(root: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("store/data.img"))
        .unwrap()
}

fn slot(secret: &'static [u8], kdf: Kdf, token: bool) -> NewKeyslot<'static> {
    NewKeyslot {
        secret,
        kdf,
        token: token.then(token_of),
    }
}

fn token_of() -> Map<String, Value> {
    token()
}

/// The keyslot that opens with `secret`, if one does.
fn opens(volume: &Volume, device: &File, secret: &[u8]) -> Option<usize> {
    volume
        .keyslots()
        .into_iter()
        .find(|slot| volume.unlock(device, *slot, secret).is_ok())
}

/// The device before `operation`: zeros for `format`; a volume with the TPM
/// and recovery slots for `add`; one with a staged slot as well for `kill`.
fn seed(root: &Path, operation: &str) {
    fs::create_dir_all(root.join("store")).unwrap();
    let file = File::create(root.join("store/data.img")).unwrap();
    file.set_len(20 * 1024 * 1024).unwrap();
    drop(file);
    if operation == "format" {
        return;
    }
    let device = device(root);
    let (mut volume, key) = Volume::format(
        &device,
        Cipher::AesXts,
        "0f0e0d0c-0b0a-4908-8706-050403020100",
        &[slot(TPM, PBKDF2, true), slot(RECOVERY, ARGON, false)],
    )
    .unwrap();
    if operation == "kill" {
        volume
            .add_keyslot(&device, &key, &slot(STAGED, PBKDF2, true))
            .unwrap();
    }
}

fn run_operation(root: &Path, operation: &str) {
    let device = device(root);
    match operation {
        "format" => {
            Volume::format(
                &device,
                Cipher::AesXts,
                "0f0e0d0c-0b0a-4908-8706-050403020100",
                &[slot(TPM, PBKDF2, true), slot(RECOVERY, ARGON, false)],
            )
            .unwrap();
        }
        "add" => {
            let mut volume = Volume::read(&device).unwrap();
            let key = volume.unlock_any(&device, RECOVERY).unwrap();
            volume
                .add_keyslot(&device, &key, &slot(STAGED, PBKDF2, true))
                .unwrap();
        }
        "kill" => {
            let mut volume = Volume::read(&device).unwrap();
            let staged = opens(&volume, &device, STAGED).unwrap();
            volume.kill_keyslot(&device, staged).unwrap();
        }
        other => panic!("unknown operation {other}"),
    }
}

/// What a cut may leave: no header (only before a format's first header
/// write), or a volume whose every keyslot opens with its secret, each
/// opening the same key, the TPM and recovery slots always among them.
fn validate(root: &Path, operation: &str) -> bool {
    let device = device(root);
    if !has_header(&device).unwrap() {
        assert_eq!(operation, "format", "a cut lost the header");
        return false;
    }
    let volume = Volume::read(&device).unwrap();
    let key = volume.unlock_any(&device, RECOVERY).unwrap();
    assert_eq!(*volume.unlock_any(&device, TPM).unwrap(), *key);
    for slot in volume.keyslots() {
        let opened = [TPM, RECOVERY, STAGED]
            .into_iter()
            .find_map(|secret| volume.unlock(&device, slot, secret).ok());
        assert_eq!(
            opened.as_deref().map(Vec::as_slice),
            Some(&key[..]),
            "keyslot {slot} listed but its area does not open"
        );
    }
    assert!(
        volume.tokens().iter().all(|(_, slots)| !slots.is_empty()),
        "a token names no keyslot"
    );
    true
}

/// After the restarted operation: the end state.
fn validate_done(root: &Path, operation: &str) {
    assert!(validate(root, operation));
    let device = device(root);
    let volume = Volume::read(&device).unwrap();
    let staged = opens(&volume, &device, STAGED);
    match operation {
        "format" => assert_eq!(volume.keyslots().len(), 2),
        "add" => assert!(staged.is_some() && volume.keyslots().len() == 3),
        _ => assert!(staged.is_none() && volume.keyslots().len() == 2),
    }
}

#[test]
#[ignore = "requires tests/file-ab-faults/run.sh and its isolated IO fault shim"]
pub(super) fn luks_writes_survive_each_boundary() {
    use std::os::unix::process::ExitStatusExt;
    if let Ok(root) = std::env::var("MICA_FAULT_WORK") {
        run_operation(
            Path::new(&root),
            &std::env::var("MICA_FAULT_OPERATION").unwrap(),
        );
        return;
    }
    let shim = std::env::var("MICA_TEST_FAULT_SHIM").expect("run tests/file-ab-faults/run.sh");
    let evidence = PathBuf::from(std::env::var("MICA_TEST_FAULT_EVIDENCE").unwrap());
    let mut summaries = Vec::new();
    for operation in ["format", "add", "kill"] {
        let invoke = |root: &Path, at: usize, phase: &str, enospc: bool| {
            let mut cmd = Command::new(std::env::current_exe().unwrap());
            cmd.args([
                "--ignored",
                "--exact",
                "data_crypt::luks_writes_survive_each_boundary",
                "--test-threads=1",
            ])
            .env("LD_PRELOAD", &shim)
            .env("MICA_FAULT_WORK", root)
            .env("MICA_FAULT_OPERATION", operation)
            .env("MICA_FAULT_PREFIX", root.join("store"))
            .env("MICA_FAULT_LOG", root.join("trace"))
            .env("MICA_FAULT_AT", at.to_string())
            .env("MICA_FAULT_WHEN", phase);
            if enospc {
                cmd.env("MICA_FAULT_ENOSPC", "1");
            }
            cmd.output().unwrap()
        };
        let baseline = TempDir::new_in(&evidence).unwrap();
        seed(baseline.path(), operation);
        let result = invoke(baseline.path(), 0, "before", false);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        validate_done(baseline.path(), operation);
        let trace = fs::read_to_string(baseline.path().join("trace")).unwrap();
        assert!(
            trace.contains("pwrite") && trace.contains("fdatasync"),
            "{trace}"
        );
        let count = trace.lines().count();
        fs::write(evidence.join(format!("luks-{operation}.trace")), trace).unwrap();
        for at in 1..=count {
            for (phase, enospc) in [("before", false), ("after", false), ("before", true)] {
                let case = TempDir::new_in(&evidence).unwrap();
                seed(case.path(), operation);
                let result = invoke(case.path(), at, phase, enospc);
                let check = std::panic::catch_unwind(|| {
                    if enospc {
                        assert!(result.status.success() || result.status.code() == Some(101));
                    } else {
                        assert_eq!(
                            result.status.signal(),
                            Some(9),
                            "boundary {at}/{count}: {phase} {operation}"
                        );
                    }
                    let headed = validate(case.path(), operation);
                    // Restart: a format cut before its header starts again on
                    // a fresh disk; an add or kill is retried if it did not land.
                    let device = device(case.path());
                    let landed = headed && {
                        let volume = Volume::read(&device).unwrap();
                        let staged = opens(&volume, &device, STAGED).is_some();
                        match operation {
                            "format" => true,
                            "add" => staged,
                            _ => !staged,
                        }
                    };
                    if !landed {
                        run_operation(case.path(), operation);
                    }
                    validate_done(case.path(), operation);
                });
                if let Err(error) = check {
                    eprintln!("failed fixture retained: {}", case.keep().display());
                    std::panic::resume_unwind(error);
                }
            }
        }
        let summary = format!(
            "LUKS2 {operation}: {count} IO boundaries, {} interrupted/error cases",
            count * 3
        );
        println!("{summary}");
        summaries.push(summary);
    }
    fs::write(evidence.join("luks-results.txt"), summaries.join("\n")).unwrap();
}

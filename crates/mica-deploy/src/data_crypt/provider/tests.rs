use std::cell::RefCell;
use std::fs::{File, OpenOptions};
use std::rc::Rc;

use anyhow::bail;
use serde_json::json;

use super::*;

type Log = Rc<RefCell<Vec<String>>>;

/// A provider that keeps its secret in memory and logs every call.
struct Fake {
    kind: &'static str,
    ready: bool,
    enrolls: bool,
    unlocks: bool,
    seals_off: bool,
    log: Log,
}

impl Fake {
    fn new(kind: &'static str, log: &Log) -> Self {
        Self {
            kind,
            ready: true,
            enrolls: true,
            unlocks: true,
            seals_off: true,
            log: Rc::clone(log),
        }
    }

    fn secret(&self) -> Secret {
        Zeroizing::new(format!("secret of {}", self.kind).into_bytes())
    }

    fn note(&self, call: &str) {
        self.log.borrow_mut().push(format!("{} {call}", self.kind));
    }
}

impl KeyProvider for Fake {
    fn token_type(&self) -> &'static str {
        self.kind
    }

    fn available(&mut self) -> Availability {
        self.note("available");
        if self.ready {
            Availability::Ready
        } else {
            Availability::Unavailable("no device".into())
        }
    }

    fn enroll(&mut self) -> Result<(Secret, Map<String, Value>)> {
        self.note("enroll");
        if !self.enrolls {
            bail!("seal failed");
        }
        let token = json!({ "type": self.kind, "keyslots": [] });
        Ok((self.secret(), token.as_object().unwrap().clone()))
    }

    fn unlock(&mut self, token: &Map<String, Value>) -> Result<Secret> {
        self.note("unlock");
        assert_eq!(token["type"], self.kind, "only its own tokens");
        if !self.unlocks {
            bail!("policy failed");
        }
        Ok(self.secret())
    }

    fn seal_off(&mut self) -> Result<()> {
        self.note("seal_off");
        if !self.seals_off {
            bail!("extend failed");
        }
        Ok(())
    }
}

fn device() -> (tempfile::TempDir, File) {
    let dir = tempfile::tempdir().unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.path().join("data.img"))
        .unwrap();
    file.set_len(20 * 1024 * 1024).unwrap();
    (dir, file)
}

const FORMAT: Format = Format {
    cipher: Cipher::AesXts,
    no_provider: NoProvider::Refuse,
    recovery: Kdf::Argon2id {
        time: 1,
        memory_kib: 64,
        cpus: 1,
    },
};

fn calls(log: &Log) -> Vec<String> {
    std::mem::take(&mut *log.borrow_mut())
}

/// Format with providers `a` and `b`, returning the recovery passphrase.
fn formatted(device: &File, log: &Log) -> Secret {
    let (mut a, mut b) = (Fake::new("a", log), Fake::new("b", log));
    let outcome = open(device, &mut [&mut a, &mut b], Some(&FORMAT));
    let Ok(Opened::Formatted { recovery, .. }) = outcome.opened else {
        panic!("not formatted");
    };
    calls(log);
    recovery
}

#[test]
fn plain_data_touches_no_provider_but_still_seals_off() {
    let (_dir, device) = device();
    let log = Log::default();
    let (mut a, mut b) = (Fake::new("a", &log), Fake::new("b", &log));
    let outcome = open(&device, &mut [&mut a, &mut b], None);
    assert!(matches!(outcome.opened, Ok(Opened::Plain)));
    assert_eq!(calls(&log), ["a seal_off", "b seal_off"]);
    assert!(!has_header(&device).unwrap());
}

#[test]
fn a_first_format_enrols_every_ready_provider_in_order_plus_recovery() {
    let (_dir, device) = device();
    let log = Log::default();
    let (mut a, mut b, mut c) = (
        Fake::new("a", &log),
        Fake::new("b", &log),
        Fake::new("c", &log),
    );
    b.ready = false;
    let outcome = open(&device, &mut [&mut a, &mut b, &mut c], Some(&FORMAT));
    let Ok(Opened::Formatted {
        volume,
        key,
        enrolled,
        recovery,
        failures,
    }) = outcome.opened
    else {
        panic!("not formatted");
    };
    assert_eq!(enrolled, ["a", "c"]);
    assert_eq!(failures, [("b", "no device".to_string())]);
    assert_eq!(
        calls(&log),
        [
            "a available",
            "a enroll",
            "b available",
            "c available",
            "c enroll",
            "a seal_off",
            "b seal_off",
            "c seal_off"
        ]
    );
    assert_eq!(volume.keyslots().len(), 3, "a, c and recovery");
    assert_eq!(volume.cipher(), Cipher::AesXts);
    let read = Volume::read(&device).unwrap();
    assert_eq!(*read.unlock_any(&device, &recovery).unwrap(), *key);
    let tokens: Vec<_> = read
        .tokens()
        .into_iter()
        .map(|(t, _)| t["type"].clone())
        .collect();
    assert_eq!(tokens, ["a", "c"], "recovery has no token");
}

#[test]
fn the_recovery_passphrase_is_eight_groups_of_five_base32() {
    let passphrase = recovery_passphrase().unwrap();
    let text = std::str::from_utf8(&passphrase).unwrap();
    let groups: Vec<_> = text.split('-').collect();
    assert_eq!(groups.len(), 8);
    assert!(
        groups
            .iter()
            .all(|g| g.len() == 5 && g.bytes().all(|b| BASE32.contains(&b)))
    );
    assert_ne!(*recovery_passphrase().unwrap(), *passphrase);
}

#[test]
fn no_provider_refuse_leaves_data_untouched() {
    let (_dir, device) = device();
    let log = Log::default();
    let (mut a, mut b) = (Fake::new("a", &log), Fake::new("b", &log));
    a.ready = false;
    b.enrolls = false;
    let outcome = open(&device, &mut [&mut a, &mut b], Some(&FORMAT));
    let Ok(Opened::NoProvider { failures }) = outcome.opened else {
        panic!("not refused");
    };
    assert_eq!(
        failures,
        [
            ("a", "no device".to_string()),
            ("b", "seal failed".to_string())
        ]
    );
    assert!(!has_header(&device).unwrap());
    assert_eq!(
        calls(&log)
            .iter()
            .filter(|c| c.ends_with("seal_off"))
            .count(),
        2
    );
}

#[test]
fn no_provider_plain_falls_back_without_a_header() {
    let (_dir, device) = device();
    let log = Log::default();
    let mut a = Fake::new("a", &log);
    a.ready = false;
    let format = Format {
        no_provider: NoProvider::Plain,
        ..FORMAT
    };
    let outcome = open(&device, &mut [&mut a], Some(&format));
    assert!(matches!(
        outcome.opened,
        Ok(Opened::PlainFallback { ref failures }) if failures.len() == 1
    ));
    assert!(!has_header(&device).unwrap());
    assert_eq!(calls(&log), ["a available", "a seal_off"]);
}

#[test]
fn unlock_follows_the_product_order_and_the_first_wins() {
    let (_dir, device) = device();
    let log = Log::default();
    formatted(&device, &log);

    let (mut a, mut b) = (Fake::new("a", &log), Fake::new("b", &log));
    let outcome = open(&device, &mut [&mut b, &mut a], Some(&FORMAT));
    assert!(matches!(
        outcome.opened,
        Ok(Opened::Unlocked { provider: "b", .. })
    ));
    assert_eq!(
        calls(&log),
        ["b available", "b unlock", "b seal_off", "a seal_off"],
        "a is never asked: b came first and unlocked"
    );
}

#[test]
fn a_failing_provider_falls_through_to_the_next() {
    let (_dir, device) = device();
    let log = Log::default();
    formatted(&device, &log);

    let (mut a, mut b) = (Fake::new("a", &log), Fake::new("b", &log));
    a.unlocks = false;
    let outcome = open(&device, &mut [&mut a, &mut b], None);
    assert!(matches!(
        outcome.opened,
        Ok(Opened::Unlocked { provider: "b", .. })
    ));
    assert_eq!(
        calls(&log),
        [
            "a available",
            "a unlock",
            "b available",
            "b unlock",
            "a seal_off",
            "b seal_off"
        ]
    );
}

#[test]
fn a_header_with_no_provider_unlocking_is_locked_never_reformatted() {
    let (_dir, device) = device();
    let log = Log::default();
    let recovery = formatted(&device, &log);

    let (mut a, mut b) = (Fake::new("a", &log), Fake::new("b", &log));
    a.unlocks = false;
    b.ready = false;
    let outcome = open(&device, &mut [&mut a, &mut b], Some(&FORMAT));
    let Ok(Opened::Locked { failures }) = outcome.opened else {
        panic!("not locked");
    };
    assert_eq!(
        failures,
        [
            ("a", "policy failed".to_string()),
            ("b", "no device".to_string())
        ]
    );
    assert!(!calls(&log).iter().any(|c| c.ends_with("enroll")));
    let volume = Volume::read(&device).unwrap();
    assert!(
        volume.unlock_any(&device, &recovery).is_ok(),
        "the header is kept"
    );
}

#[test]
fn a_header_is_honoured_with_encryption_off() {
    let (_dir, device) = device();
    let log = Log::default();
    formatted(&device, &log);

    let mut a = Fake::new("a", &log);
    let outcome = open(&device, &mut [&mut a], None);
    assert!(matches!(
        outcome.opened,
        Ok(Opened::Unlocked { provider: "a", .. })
    ));
    let outcome = open(&device, &mut [], None);
    assert!(matches!(outcome.opened, Ok(Opened::Locked { ref failures }) if failures.is_empty()));
}

#[test]
fn seal_off_runs_on_an_error_path_and_its_failure_is_reported() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.img");
    File::create(&path)
        .unwrap()
        .set_len(20 * 1024 * 1024)
        .unwrap();
    let read_only = File::open(&path).unwrap();
    let log = Log::default();
    let (mut a, mut b) = (Fake::new("a", &log), Fake::new("b", &log));
    b.seals_off = false;
    let outcome = open(&read_only, &mut [&mut a, &mut b], Some(&FORMAT));
    assert!(outcome.opened.is_err(), "the header write fails");
    assert_eq!(
        outcome.seal_off_failures,
        [("b", "extend failed".to_string())]
    );
    let calls = calls(&log);
    assert_eq!(calls[calls.len() - 2..], ["a seal_off", "b seal_off"]);
}

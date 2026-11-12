use std::io::{Read, Write};
use std::net::TcpStream;

use super::client::*;
use super::wire::{Res, W};
use super::*;

fn unhex_ok(text: &str) -> Vec<u8> {
    unhex(text).unwrap()
}

// The reference values come from a separate implementation (Python hmac and
// hashlib; OpenSSL `enc -aes-128-cfb`).

#[test]
fn kdfa_and_kdfe_match_the_reference() {
    let key: Vec<u8> = (0..32).collect();
    let u: Vec<u8> = (32..64).collect();
    let v: Vec<u8> = (64..96).collect();
    assert_eq!(
        *kdfa(&key, "CFB", &u, &v, 256),
        unhex_ok("b089d48db92e134831ed3c9792c191c2e45cc7011de0b5f0fbb259f88d0668ea")
    );
    assert_eq!(
        *kdfa(&key, "ATH", &u, &v, 320),
        unhex_ok(
            "9d5861460d9b45a2bdfdac77300f3c2000e25fd7eec856b04bad9e7f17ed62be468c6770dd62c37b"
        )
    );
    assert_eq!(
        *kdfe(&key, "SECRET", &u, &v, 256),
        unhex_ok("62cf85ca3599cd915a71a6330c3885f986918e12037ba2e39578f72686417cdb")
    );
}

#[test]
fn the_pcr_policy_digest_matches_the_reference() {
    assert_eq!(
        policy_pcr_digest(&[vec![0x11; 32], vec![0; 32]]),
        unhex_ok("41d9554948646697522f1958ea299d53b8c11acc88abdb9f5ab1bd35d6fb2253")
    );
}

#[test]
fn cfb_matches_openssl_both_ways() {
    let key_iv: Vec<u8> = (0..32).collect();
    let plain: Vec<u8> = (0..37).map(|i| (i % 251) as u8).collect();
    let mut data = plain.clone();
    cfb(&key_iv, &mut data, true).unwrap();
    assert_eq!(
        data,
        unhex_ok("07ffed77e5d005699807e41a82999c9c66dc9d6bd70ba905d01b6805b1ed0e9b3102d940ac")
    );
    cfb(&key_iv, &mut data, false).unwrap();
    assert_eq!(data, plain);
}

/// A TPM that answers only CreatePrimary (with a P-256 key whose name
/// matches its area unless `bad_name`) and FlushContext, and records the
/// command codes it was sent.
struct Fake {
    sent: Vec<u32>,
    bad_name: bool,
}

fn ecc_area(fill: u8) -> Vec<u8> {
    let mut area = W::default();
    area.u16(0x23)
        .u16(0x0B)
        .u32(0x0003_0472)
        .b(&[])
        .u16(0x06)
        .u16(128)
        .u16(0x43)
        .u16(0x10)
        .u16(0x03)
        .u16(0x10)
        .b(&[fill; 32])
        .b(&[fill; 32]);
    area.0
}

fn name_of(area: &[u8]) -> Vec<u8> {
    [&[0x00, 0x0B][..], &sha256(&[area])].concat()
}

fn response(tag: u16, handles: &[u32], params: &[u8]) -> Vec<u8> {
    let mut body = W::default();
    for h in handles {
        body.u32(*h);
    }
    if tag == 0x8002 {
        body.u32(params.len() as u32).raw(params);
        body.b(&[]).u8(0).b(&[]);
    } else {
        body.raw(params);
    }
    let mut out = W::default();
    out.u16(tag)
        .u32(10 + body.0.len() as u32)
        .u32(0)
        .raw(&body.0);
    out.0
}

fn create_primary_response(fill: u8, bad_name: bool) -> Vec<u8> {
    let area = ecc_area(fill);
    let mut name = name_of(&area);
    if bad_name {
        name[5] ^= 1;
    }
    let mut params = W::default();
    params
        .b(&area)
        .b(&[])
        .b(&[])
        .u16(0x8021)
        .u32(0x4000_0001)
        .b(&[])
        .b(&name);
    response(0x8002, &[0x8000_0000], &params.0)
}

impl Transport for Fake {
    fn transmit(&mut self, command: &[u8]) -> Res<Vec<u8>> {
        let cc = u32::from_be_bytes(command[6..10].try_into().unwrap());
        self.sent.push(cc);
        match cc {
            CC_CREATE_PRIMARY => Ok(create_primary_response(0x42, self.bad_name)),
            CC_FLUSH_CONTEXT => Ok(response(0x8001, &[], &[])),
            other => Err(format!("the fake TPM has no command 0x{other:x}")),
        }
    }
}

fn token_for(srk_name: &[u8]) -> Map<String, Value> {
    let mut blob = W::default();
    blob.b(&[1, 2, 3]).b(&[4, 5, 6]);
    let token = json!({
        "type": TOKEN,
        "keyslots": ["0"],
        "tpm2-blob": B64.encode(&blob.0),
        "mica-srk-name": hex(srk_name),
    });
    token.as_object().unwrap().clone()
}

#[test]
fn a_storage_key_other_than_the_sealed_one_is_refused_before_salting() {
    let mut tpm = Tpm2::new(Fake {
        sent: Vec::new(),
        bad_name: false,
    });
    let other = name_of(&ecc_area(0x43));
    let err = tpm.unlock(&token_for(&other)).unwrap_err();
    assert!(
        format!("{err:#}").contains("not the one sealed to"),
        "{err:#}"
    );
    let sent = &tpm
        .client
        .as_ref()
        .unwrap_or_else(|_| unreachable!())
        .transport()
        .sent;
    assert_eq!(
        sent,
        &[CC_CREATE_PRIMARY, CC_FLUSH_CONTEXT],
        "no StartAuthSession"
    );
}

#[test]
fn a_primary_whose_name_does_not_match_its_area_is_refused() {
    let mut tpm = Tpm2::new(Fake {
        sent: Vec::new(),
        bad_name: true,
    });
    let name = name_of(&ecc_area(0x42));
    let err = tpm.unlock(&token_for(&name)).unwrap_err();
    assert!(
        format!("{err:#}").contains("does not match its public area"),
        "{err:#}"
    );
}

#[test]
fn a_token_without_its_fields_is_refused_without_a_tpm_command() {
    let mut tpm = Tpm2::new(Fake {
        sent: Vec::new(),
        bad_name: false,
    });
    let mut token = token_for(&[0; 34]);
    token.remove("mica-srk-name");
    assert!(tpm.unlock(&token).is_err());
    let mut token = token_for(&[0; 34]);
    token.insert("tpm2-blob".into(), json!(B64.encode([0, 9, 1])));
    assert!(tpm.unlock(&token).is_err());
    assert!(
        tpm.client
            .as_ref()
            .unwrap_or_else(|_| unreachable!())
            .transport()
            .sent
            .is_empty()
    );
}

#[test]
fn a_missing_device_makes_the_provider_unavailable() {
    let mut tpm = Tpm2::open(Path::new("/nonexistent/tpmrm0"));
    assert!(matches!(tpm.available(), Availability::Unavailable(_)));
    assert!(tpm.enroll().is_err());
    assert!(tpm.seal_off().is_err(), "reported, never silent");
}

/// Mutated responses never panic a parser (the full corpus run is kept
/// out of CI, see the step report).
#[test]
fn response_parsers_survive_mutation() {
    let seeds = [
        create_primary_response(0x42, false),
        response(0x8001, &[], &[]),
        response(
            0x8001,
            &[],
            &[
                0, 0, 0, 1, 0, 0, 0, 1, 0, 0x0b, 3, 0x80, 0x80, 0, 0, 0, 0, 1, 0, 2, 7, 7,
            ],
        ),
    ];
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    for _ in 0..20_000 {
        let mut b = seeds[(next() % seeds.len() as u64) as usize].clone();
        for _ in 0..=(next() % 4) {
            if b.is_empty() {
                b.push(0);
            }
            let at = (next() % b.len() as u64) as usize;
            match next() % 4 {
                0 => b[at] ^= 1 << (next() % 8),
                1 => b[at] = next() as u8,
                2 => b.truncate(at),
                _ => {
                    if b.len() >= 6 {
                        let size = (b.len() as u32).to_be_bytes();
                        b[2..6].copy_from_slice(&size);
                    }
                }
            }
        }
        for handles in 0..=2 {
            if let Ok(r) = parse_response(&b, handles) {
                let _ = parse_create_primary(&r.params).map(|(area, _)| parse_ecc_public(&area));
                let _ = parse_create(&r.params);
                let _ = parse_pcr_read(&r.params);
            }
        }
    }
}

/// Every response recorded from swtpm in step 0 (`MICA_TPM_CORPUS`),
/// mutated `MICA_TPM_FUZZ` times (2,000,000 by default) through every
/// parser: none may panic. Not in CI.
#[test]
#[ignore = "needs the step 0 response corpus at MICA_TPM_CORPUS"]
fn the_recorded_corpus_never_panics_a_parser() {
    let dir = std::env::var("MICA_TPM_CORPUS").expect("MICA_TPM_CORPUS");
    let iterations: u64 = std::env::var("MICA_TPM_FUZZ").map_or(2_000_000, |n| n.parse().unwrap());
    let corpus: Vec<Vec<u8>> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
        .collect();
    assert!(!corpus.is_empty());
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut parsed = 0u64;
    for _ in 0..iterations {
        let mut b = corpus[(next() % corpus.len() as u64) as usize].clone();
        for _ in 0..=(next() % 4) {
            if b.is_empty() {
                b.push(0);
            }
            let at = (next() % b.len() as u64) as usize;
            match next() % 6 {
                0 => b[at] ^= 1 << (next() % 8),
                1 => b[at] = next() as u8,
                2 => b.truncate(at),
                3 => b.extend((0..next() % 64).map(|_| next() as u8)),
                4 if b.len() >= 6 => {
                    let size = (b.len() as u32).to_be_bytes();
                    b[2..6].copy_from_slice(&size);
                }
                _ => {
                    if at + 1 < b.len() {
                        let v: u16 = if next() % 2 == 0 { 0xFFFF } else { 0 };
                        b[at..at + 2].copy_from_slice(&v.to_be_bytes());
                    }
                }
            }
        }
        for handles in 0..=2 {
            if let Ok(r) = parse_response(&b, handles) {
                parsed += 1;
                let _ = parse_create_primary(&r.params).map(|(area, _)| parse_ecc_public(&area));
                let _ = parse_create(&r.params);
                let _ = parse_pcr_read(&r.params);
                let _ = split_blob(&r.params);
            }
        }
    }
    println!(
        "TPM corpus: {iterations} mutated responses from {} seeds, 0 panics, {parsed} parsed as a response",
        corpus.len()
    );
}

/// swtpm over TCP, with every byte kept and one response byte flipped on
/// demand.
struct Swtpm {
    stream: TcpStream,
    wire: Vec<u8>,
    tamper: Option<(u32, usize)>,
}

impl Swtpm {
    fn connect() -> Self {
        let addr = std::env::var("MICA_SWTPM").expect("MICA_SWTPM=host:port");
        Self {
            stream: TcpStream::connect(addr).unwrap(),
            wire: Vec::new(),
            tamper: None,
        }
    }
}

impl Transport for Swtpm {
    fn transmit(&mut self, command: &[u8]) -> Res<Vec<u8>> {
        self.stream.write_all(command).map_err(|e| e.to_string())?;
        self.wire.extend_from_slice(command);
        let mut out = vec![0u8; 10];
        self.stream
            .read_exact(&mut out)
            .map_err(|e| e.to_string())?;
        let size = u32::from_be_bytes(out[2..6].try_into().unwrap()) as usize;
        out.resize(size.max(10), 0);
        self.stream
            .read_exact(&mut out[10..])
            .map_err(|e| e.to_string())?;
        self.wire.extend_from_slice(&out);
        let cc = u32::from_be_bytes(command[6..10].try_into().unwrap());
        if let Some((target, at)) = self.tamper
            && target == cc
        {
            let at = at.min(out.len() - 1);
            out[at] ^= 1;
            self.tamper = None;
        }
        Ok(out)
    }
}

fn transport(tpm: &mut Tpm2<Swtpm>) -> &mut Swtpm {
    tpm.client
        .as_mut()
        .unwrap_or_else(|_| unreachable!())
        .transport_mut()
}

fn in_clear(wire: &[u8], secret: &[u8]) -> bool {
    secret.windows(16).any(|w| wire.windows(16).any(|x| x == w))
}

/// Against a fresh swtpm (PCR 15 zero): enrol, unlock, the negatives, then
/// seal-off. Run by the step's swtpm driver, not in CI.
#[test]
#[ignore = "needs swtpm at MICA_SWTPM"]
fn swtpm_enrol_unlock_and_seal_off() {
    let mut tpm = Tpm2::new(Swtpm::connect());
    assert_eq!(tpm.available(), Availability::Ready);
    let (secret, token) = tpm.enroll().unwrap();
    assert_eq!(token["tpm2-pcrs"], json!([7, 15]));
    let raw = B64.decode(&*secret).unwrap();
    assert_eq!(raw.len(), 32);
    assert_eq!(*tpm.unlock(&token).unwrap(), *secret);

    assert!(
        !in_clear(&transport(&mut tpm).wire, &raw),
        "the secret crossed the wire in clear"
    );

    // A flipped byte in the encrypted answer or in its HMAC: refused here.
    for at in [20, 200] {
        transport(&mut tpm).tamper = Some((CC_UNSEAL, at));
        let err = tpm.unlock(&token).unwrap_err();
        assert!(
            format!("{err:#}").contains("HMAC does not verify"),
            "{err:#}"
        );
    }
    // A flipped nonceTPM: the TPM refuses the command HMAC.
    transport(&mut tpm).tamper = Some((CC_START_AUTH_SESSION, 16));
    let err = tpm.unlock(&token).unwrap_err();
    assert!(format!("{err:#}").contains("TPM rc"), "{err:#}");
    // Another storage key name: refused before a session is salted.
    let mut wrong = token.clone();
    wrong.insert("mica-srk-name".into(), json!(hex(&name_of(&ecc_area(1)))));
    let before = transport(&mut tpm).wire.len();
    assert!(tpm.unlock(&wrong).is_err());
    let after = &transport(&mut tpm).wire[before..];
    assert!(
        !after
            .windows(4)
            .any(|w| w == CC_START_AUTH_SESSION.to_be_bytes())
    );

    assert_eq!(
        *tpm.unlock(&token).unwrap(),
        *secret,
        "still unseals before seal-off"
    );
    tpm.seal_off().unwrap();
    let err = tpm.unlock(&token).unwrap_err();
    assert!(format!("{err:#}").contains("POLICY_FAIL"), "{err:#}");
}

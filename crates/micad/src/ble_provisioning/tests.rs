//! The RFC 9383 vector and the vectors of `docs/ble-provisioning.md`.

mod engine;
mod store;
mod window;

use std::num::NonZeroU32;

use aws_lc_rs::agreement;
use p256::ProjectivePoint;
use p256::elliptic_curve::PrimeField;
use p256::elliptic_curve::group::Group;

use super::channel::*;
use super::spake2plus::*;
use super::*;

fn h(text: &str) -> Vec<u8> {
    hex::decode(text).expect("hex")
}

fn h32(text: &str) -> [u8; 32] {
    h(text).try_into().expect("32 bytes")
}

/// RFC 9383 Appendix C, P256-SHA256-HKDF-SHA256-HMAC-SHA256.
#[test]
fn the_rfc_9383_vector_holds() {
    let t = Transcript {
        context: b"SPAKE2+-P256-SHA256-HKDF-SHA256-HMAC-SHA256 Test Vectors",
        id_prover: b"client",
        id_verifier: b"server",
    };
    let w0 = scalar(&h32(
        "bb8e1bbcf3c48f62c08db243652ae55d3e5586053fca77102994f23ad95491b3",
    ));
    let w1 = scalar(&h32(
        "7e945f34d78785b8a3ef44d0df5a1a97d6b3b460409a345ca7830387a74b1dba",
    ));
    let x = scalar(&h32(
        "d1232c8e8693d02368976c174e2088851b8365d0d79a9eee709c6a05a2fad539",
    ));
    let y = scalar(&h32(
        "717a72348a182085109c8d3917d6c43d59b224dc6a7fc4f0483232fa6516d8b3",
    ));
    let record = Record {
        salt: Vec::new(),
        iterations: 1,
        w0: w0.to_repr().into(),
        l: encode(&(ProjectivePoint::generator() * w1)),
    };
    assert_eq!(
        hex::encode(record.l),
        "04eb7c9db3d9a9eb1f8adab81b5794c1f13ae3e225efbe91ea487425854c7fc00f00bfedcbd09b2400142d40a14f2064ef31dfaa903b91d1faea7093d835966efd"
    );
    let share_p = prover::share(&w0, &x);
    assert_eq!(
        hex::encode(share_p),
        "04ef3bd051bf78a2234ec0df197f7828060fe9856503579bb1733009042c15c0c1de127727f418b5966afadfdd95a6e4591d171056b333dab97a79c7193e341727"
    );
    let answer = answer_with(&record, &t, &share_p, y).unwrap();
    assert_eq!(
        hex::encode(answer.share_v),
        "04c0f65da0d11927bdf5d560c69e1d7d939a05b0e88291887d679fcadea75810fb5cc1ca7494db39e82ff2f50665255d76173e09986ab46742c798a9a68437b048"
    );
    assert_eq!(
        hex::encode(answer.keys.confirm_p),
        "926cc713504b9b4d76c9162ded04b5493e89109f6d89462cd33adc46fda27527"
    );
    assert_eq!(
        hex::encode(answer.keys.confirm_v),
        "9747bcc4f8fe9f63defee53ac9b07876d907d55047e6ff2def2e7529089d3e68"
    );
    assert_eq!(
        hex::encode(answer.keys.shared),
        "0c5f8ccd1413423a54f6c1fb26ff01534a87f893779c6e68666d772bfd91f3e7"
    );
    let prover = prover::finish(&t, &w0, &w1, &x, &answer.share_v).unwrap();
    assert_eq!(prover, answer.keys);
}

/// The document's `password` and `temp-code` vectors, from the password on.
#[test]
fn the_document_s_spake2plus_vectors_hold() {
    let cases = [
        (
            CONTEXT_PASSWORD,
            "harbour-pw-1",
            "000102030405060708090a0b0c0d0e0f",
            "947235c6a07ca203da652ef5f3654578849ab4e5f1ae01698be30fae331fc46f",
            "041602a15fdc3635da663b56f662817f13c2492eef661baa61f9c6a83cab56ec78301d4c8b53a704b52a92cea7bd66b6ca0efa10879e3455112d0cd054843fedbd",
            "50ec4dec9740817c464fbd565f6a4ce13d6759645b17410600f88cb76b015cde",
            "2b5b9dd3a7a0090b90a2c46652b653f8ed7315b45d10bebdb3e88f1cbba1b28e",
            "04e0f08c16f158714530c1ff822a6b6c2c46a608c58ba1c616a2d42994a9cbdae2199796a5269bfc0922512c25aac4a0063a117f657b7d0dccc4fb5aaabefc3c79",
            "f13f912ed43d74d959acfda95a39fa255235fadaa444b6005a50db4f06c9e6ef",
            "920c9a30aacc3a02db9a39841a9d5fcca698c5e0c8b524e2417c94cf05f8ea5c",
            "bd808aa6a273ae78a8fb53434d5cf43320d2b9f3a34edb6de4afd62d243747f6",
        ),
        (
            CONTEXT_TEMP_CODE,
            "40718263",
            "f0e1d2c3b4a5968778695a4b3c2d1e0f",
            "1b91176fc9da89e1238ccc240b5a22c55fc6410f398b0f3381f75658dc2df0cf",
            "04b43659a2162ec5eb40cb28007f7d5c60aeda02f1d6e040c3fdd562fc111f2306d4a67f03ca8c1ceb0cd3d329f77379ee03688c034353ad5f6e295552118fe65c",
            "dc299dff456bea2a25517e4eda26c6352c886a4c04ac502e847fb636e1a6a16d",
            "bed8d6582ea4915d95be2bc9a8dd3d446a8ff6ec725453672c1cd024960831ad",
            "049416ad76ad99039d5a9096f8c3ce645b4ed308f40f063214f7381bb2f83d1d43ab8381df8488828feb60c71e60a04f4d3a29cc997e9909b7424fc467ac157aef",
            "7b4db62eedf3261819e6585f8b9332d1858e4f2aff720b7d9e4208b55058f91c",
            "29582fa90d5017750f2b65b0d9c0386eaf85de569a59cdbb97ba24c451ffde9d",
            "6306c5ec03561990cfc20c5fe35f58fc5989cc2e6f0d7c832e389b14fa4ff9c8",
        ),
    ];
    let iterations = NonZeroU32::new(600_000).unwrap();
    for (context, pw, salt, w0, l, x, y, share_v, confirm_v, confirm_p, k_p2d) in cases {
        let record = register(pw.as_bytes(), &h(salt), iterations, ID_PROVER, ID_VERIFIER);
        assert_eq!(hex::encode(record.w0), w0);
        assert_eq!(hex::encode(record.l), l);
        let t = Transcript {
            context,
            id_prover: ID_PROVER,
            id_verifier: ID_VERIFIER,
        };
        let (pw0, pw1) = derive(pw.as_bytes(), &h(salt), iterations, ID_PROVER, ID_VERIFIER);
        let x = scalar(&h32(x));
        let answer = answer_with(&record, &t, &prover::share(&pw0, &x), scalar(&h32(y))).unwrap();
        assert_eq!(hex::encode(answer.share_v), share_v);
        assert_eq!(hex::encode(answer.keys.confirm_v), confirm_v);
        assert_eq!(hex::encode(answer.keys.confirm_p), confirm_p);
        let keys = session_keys(&[], &answer.keys.shared).unwrap();
        assert_eq!(hex::encode(keys.phone_to_device), k_p2d);
        assert_eq!(
            prover::finish(&t, &pw0, &pw1, &x, &answer.share_v).unwrap(),
            answer.keys
        );
    }
}

/// A random exchange agrees, and a wrong password does not.
#[test]
fn a_wrong_password_does_not_confirm() {
    let iterations = NonZeroU32::new(1000).unwrap();
    let record = register(
        b"right-pw-1",
        b"salt-salt-salt-1",
        iterations,
        ID_PROVER,
        ID_VERIFIER,
    );
    let t = Transcript {
        context: CONTEXT_PASSWORD,
        id_prover: ID_PROVER,
        id_verifier: ID_VERIFIER,
    };
    for (pw, agrees) in [("right-pw-1", true), ("wrong-pw-1", false)] {
        let (w0, w1) = derive(
            pw.as_bytes(),
            b"salt-salt-salt-1",
            iterations,
            ID_PROVER,
            ID_VERIFIER,
        );
        let x = scalar(&h32(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        ));
        let answer = answer(&record, &t, &prover::share(&w0, &x)).unwrap();
        let prover = prover::finish(&t, &w0, &w1, &x, &answer.share_v).unwrap();
        assert_eq!(prover == answer.keys, agrees, "{pw}");
    }
}

#[test]
fn a_share_that_is_not_a_point_is_refused() {
    let record = register(
        b"right-pw-1",
        b"salt",
        NonZeroU32::new(1).unwrap(),
        ID_PROVER,
        ID_VERIFIER,
    );
    let t = Transcript {
        context: CONTEXT_PASSWORD,
        id_prover: ID_PROVER,
        id_verifier: ID_VERIFIER,
    };
    let mut off_curve = [0u8; 65];
    off_curve[0] = 4;
    off_curve[64] = 1;
    assert!(answer(&record, &t, &off_curve).is_err());
    assert!(answer(&record, &t, &[0]).is_err());
    assert!(answer(&record, &t, &record.l[..33]).is_err());
}

/// RFC 7748 section 6.1's keys: the phone is Alice, the device Bob.
#[test]
fn the_document_s_none_vector_holds() {
    let device = agreement::PrivateKey::from_private_key(
        &agreement::X25519,
        &h("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb"),
    )
    .unwrap();
    let phone_public = h("8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
    let (public, keys) = none_exchange(&device, &phone_public).unwrap();
    assert_eq!(
        hex::encode(public),
        "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"
    );
    assert_eq!(
        hex::encode(keys.phone_to_device),
        "6adbb833f44b333ea6f2b1a438110f6f65af3176a3769bf3d0c77d04166ed39b"
    );
    assert_eq!(
        hex::encode(keys.device_to_phone),
        "3f0fec7695564ebbc285bd66124daf461b74a1136fc4e67eb98d086140285f8c"
    );
    // A low-order point makes the shared secret zero.
    assert!(none_exchange(&device, &[0u8; 32]).is_err());
    assert!(none_exchange(&device, &[1u8; 31]).is_err());
}

/// The records and fragments of the document, under the `password` keys.
#[test]
fn the_document_s_records_and_fragments_hold() {
    let p2d = h32("bd808aa6a273ae78a8fb53434d5cf43320d2b9f3a34edb6de4afd62d243747f6");
    let d2p = h32("794e4eceb4a026ee4ac2b6441dda6895ace89e4d7e4f34052daf56fca4e0773c");
    let first =
        h("a9b30b71223021559fb962ba91e96de295db688d93a2a506effcd08978e73840643c7973b4ed164e8d");
    let second =
        h("2587a01eef7e85a702b0c774d68abd435dcec37814e9c80ce74a5b30023adce54fbe34bb23c755c2");

    let mut phone = Records::new(&p2d).unwrap();
    assert_eq!(phone.seal(br#"{"id":1,"op":"getStatus"}"#).unwrap(), first);
    let mut device = Records::new(&p2d).unwrap();
    assert_eq!(
        device.open(&first).unwrap(),
        br#"{"id":1,"op":"getStatus"}"#
    );
    assert_eq!(
        device.open(&second).unwrap(),
        br#"{"id":2,"op":"scanWifi"}"#
    );

    // A replayed record is under the wrong counter.
    let mut device = Records::new(&p2d).unwrap();
    device.open(&first).unwrap();
    assert!(device.open(&first).is_err());

    let mut events = Records::new(&d2p).unwrap();
    assert_eq!(
        hex::encode(events.seal(br#"{"id":1,"event":"status","wifiMode":"ap","connected":false,"ssid":null,"ip":null}"#).unwrap()),
        "dae2e332138cc8c0b253e5c0718ce3cf6387f53731b49b2105dff0ea4e074019966babd560829e8f74ccff39310ea6fc57841635a5f3f7bd29df8f84794ca4cad0f320bdfd5e023e659406893a6fcc5386a0fcb6cbbf890254a11aef28bccb2520"
    );

    let cut = fragments(&first, 20);
    assert_eq!(
        cut.iter().map(hex::encode).collect::<Vec<_>>(),
        [
            "000000a9b30b71223021559fb962ba91e96de295",
            "000100db688d93a2a506effcd08978e73840643c",
            "0002017973b4ed164e8d",
        ]
    );
    let mut join = Reassembly::default();
    assert_eq!(join.push(&cut[0]).unwrap(), None);
    assert_eq!(join.push(&cut[1]).unwrap(), None);
    assert_eq!(join.push(&cut[2]).unwrap(), Some(first.clone()));
}

#[test]
fn fragments_out_of_order_or_too_long_are_dropped() {
    let mut join = Reassembly::default();
    assert!(join.push(&[0, 1, 1, 7]).is_err());
    // The partial message is gone: a fresh first fragment starts over.
    assert_eq!(join.push(&[0, 0, 1, 7]).unwrap(), Some(vec![7]));
    assert!(join.push(&[0, 0, 2]).is_err());
    assert!(join.push(&[0, 0]).is_err());

    let big = vec![0u8; MAX_MESSAGE + 1];
    let mut join = Reassembly::default();
    let outcome = fragments(&big, MAX_FRAGMENT)
        .iter()
        .map(|fragment| join.push(fragment))
        .find(Result::is_err);
    assert!(outcome.is_some());
    assert_eq!(fragments(&[], 20), vec![vec![0, 0, 1]]);
}

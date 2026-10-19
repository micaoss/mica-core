use super::*;

/// Build a structurally valid blob for `key_type`: the four-byte length,
/// the algorithm name, then filler out to `total` decoded bytes.
///
/// Padded to a four-character boundary because the parser demands a length
/// that is a multiple of 4, which is what a real OpenSSH key always is.
fn blob_for(key_type: &str, total: usize) -> String {
    let mut bytes = Vec::new();
    let name = key_type.as_bytes();
    bytes.extend_from_slice(&u32::try_from(name.len()).unwrap().to_be_bytes());
    bytes.extend_from_slice(name);
    // Filler is position-dependent so two blobs of the same length for
    // different types are not accidentally equal.
    while bytes.len() < total {
        let index = u8::try_from(bytes.len() % 251).unwrap();
        bytes.push(index.wrapping_mul(7).wrapping_add(3));
    }
    pad(&encode_base64_nopad(&bytes))
}

/// Append `=` until the encoding lands on a four-character boundary.
fn pad(encoded: &str) -> String {
    let mut padded = encoded.to_string();
    while !padded.len().is_multiple_of(4) {
        padded.push('=');
    }
    padded
}

fn valid_line(key_type: &str) -> String {
    format!("{key_type} {}", blob_for(key_type, 64))
}

fn message_of(err: &SettingsError) -> String {
    match err {
        SettingsError::Validation { path, message } => {
            assert_eq!(path, KEYS_PATH);
            message.clone()
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

// --- Positive: every accepted type ------------------------------------

#[test]
fn every_accepted_type_parses_with_and_without_a_comment() {
    for key_type in ACCEPTED_TYPES {
        let blob = blob_for(key_type, 64);
        let bare = format!("{key_type} {blob}");

        let parsed = parse_authorized_key(&bare)
            .unwrap_or_else(|err| panic!("{key_type} should parse: {err}"));
        assert_eq!(parsed.key, bare, "{key_type} canonical form");
        assert_eq!(parsed.comment, None, "{key_type} has no comment");

        let labelled = format!("{key_type} {blob} alice@workstation");
        let parsed = parse_authorized_key(&labelled)
            .unwrap_or_else(|err| panic!("{key_type} with a comment should parse: {err}"));
        // The comment is stripped from `key`, not kept in it.
        assert_eq!(parsed.key, bare, "{key_type} strips the comment from `key`");
        assert_eq!(parsed.comment.as_deref(), Some("alice@workstation"));

        // Re-parsing the canonical form is a fixed point.
        assert_eq!(parse_authorized_key(&parsed.key).unwrap().key, bare);
    }
    assert_eq!(ACCEPTED_TYPES.len(), 7);
}

#[test]
fn a_type_outside_the_whitelist_is_rejected() {
    // Each line carries a blob that agrees with its declared type, so the
    // whitelist is the only guard that can reject it; a mismatched blob
    // would let this test pass on the blob-vs-type check instead.
    for key_type in [
        "ssh-dss",
        "ssh-ed25519-cert-v01@openssh.com",
        "SSH-ED25519",
        "rsa-sha2-256",
        "webauthn-sk-ecdsa-sha2-nistp256@openssh.com",
    ] {
        let line = format!("{key_type} {}", blob_for(key_type, 96));
        let err = parse_authorized_key(&line).unwrap_err();
        assert!(
            message_of(&err).contains("key type"),
            "{key_type} must be rejected by the whitelist, got: {}",
            message_of(&err)
        );
    }
}

// --- Per-character hostile input --------------------------------------

/// Characters that must be rejected wherever they appear, comment
/// included.
const CONTROL_CHARS: [(char, &str); 7] = [
    ('\0', "NUL"),
    ('\n', "line feed"),
    ('\r', "carriage return"),
    ('\t', "tab"),
    ('\u{b}', "vertical tab"),
    ('\u{c}', "form feed"),
    ('\u{7f}', "delete"),
];

/// Characters a shell would treat as special, which this parser must not:
/// they are rejected in the type and the blob because they are not in
/// those alphabets, and accepted verbatim in a comment.
const SHELL_METACHARS: [char; 6] = ['\\', '"', '\'', '`', '$', '#'];

#[test]
fn control_characters_are_rejected_in_the_type_field() {
    let blob = blob_for("ssh-ed25519", 64);
    for (ch, name) in CONTROL_CHARS {
        let line = format!("ssh-ed{ch}25519 {blob}");
        assert!(
            parse_authorized_key(&line).is_err(),
            "a {name} in the type field must be rejected"
        );
    }
}

#[test]
fn control_characters_are_rejected_in_the_blob() {
    let blob = blob_for("ssh-ed25519", 64);
    for (ch, name) in CONTROL_CHARS {
        let mangled = format!("{}{ch}{}", &blob[..8], &blob[9..]);
        let line = format!("ssh-ed25519 {mangled}");
        assert!(
            parse_authorized_key(&line).is_err(),
            "a {name} in the blob must be rejected"
        );
    }
}

#[test]
fn control_characters_are_rejected_in_the_comment() {
    let blob = blob_for("ssh-ed25519", 64);
    for (ch, name) in CONTROL_CHARS {
        let line = format!("ssh-ed25519 {blob} alice{ch}bob");
        assert!(
            parse_authorized_key(&line).is_err(),
            "a {name} in the comment must be rejected"
        );
    }
}

#[test]
fn shell_metacharacters_are_rejected_in_the_type_and_the_blob() {
    let blob = blob_for("ssh-ed25519", 64);
    for ch in SHELL_METACHARS {
        let typed = format!("ssh-ed{ch}25519 {blob}");
        assert!(
            parse_authorized_key(&typed).is_err(),
            "`{ch}` in the type field must be rejected"
        );

        let mangled = format!("{}{ch}{}", &blob[..8], &blob[9..]);
        let blobbed = format!("ssh-ed25519 {mangled}");
        let err = parse_authorized_key(&blobbed).unwrap_err();
        assert!(
            message_of(&err).contains("base64 blob"),
            "`{ch}` in the blob must be rejected as a blob error, got {}",
            message_of(&err)
        );
    }
}

#[test]
fn shell_metacharacters_are_accepted_verbatim_inside_a_comment() {
    // The comment reaches no shell. Rejecting these would be security
    // theatre that stops an operator writing `dev$box` as a label, so the
    // acceptance is asserted rather than left to chance.
    let blob = blob_for("ssh-ed25519", 64);
    for ch in SHELL_METACHARS {
        let comment = format!("alice{ch}bob");
        let line = format!("ssh-ed25519 {blob} {comment}");
        let parsed = parse_authorized_key(&line)
            .unwrap_or_else(|err| panic!("`{ch}` in a comment must be accepted: {err}"));
        assert_eq!(parsed.comment.as_deref(), Some(comment.as_str()));
        assert_eq!(parsed.key, format!("ssh-ed25519 {blob}"));
    }
}

#[test]
fn a_comment_that_starts_with_a_hash_is_still_a_comment() {
    let blob = blob_for("ssh-ed25519", 64);
    let parsed = parse_authorized_key(&format!("ssh-ed25519 {blob} #1 laptop")).unwrap();
    assert_eq!(parsed.comment.as_deref(), Some("#1 laptop"));
}

#[test]
fn non_ascii_utf8_in_a_comment_is_accepted() {
    let blob = blob_for("ssh-ed25519", 64);
    let parsed = parse_authorized_key(&format!("ssh-ed25519 {blob} Ada Lovelace (café)"))
        .expect("a non-ASCII label must be accepted");
    assert_eq!(parsed.comment.as_deref(), Some("Ada Lovelace (café)"));
}

#[test]
fn space_runs_and_edge_spaces_are_rejected_between_fields_but_kept_in_a_comment() {
    let blob = blob_for("ssh-ed25519", 64);

    // Two spaces between the type and the blob.
    let err = parse_authorized_key(&format!("ssh-ed25519  {blob}")).unwrap_err();
    assert!(message_of(&err).contains("base64 blob is empty"));

    // A leading space, and a trailing one (which is also the empty-comment
    // case).
    let err = parse_authorized_key(&format!(" ssh-ed25519 {blob}")).unwrap_err();
    assert!(message_of(&err).contains("starts with a space"));
    let err = parse_authorized_key(&format!("ssh-ed25519 {blob} ")).unwrap_err();
    assert!(message_of(&err).contains("ends with a space"));

    // A run inside the comment is fine: everything after the second space
    // is the comment, verbatim.
    let parsed = parse_authorized_key(&format!("ssh-ed25519 {blob}  spaced  out")).unwrap();
    assert_eq!(parsed.comment.as_deref(), Some(" spaced  out"));
}

#[test]
fn an_empty_comment_is_a_rejection_not_a_none() {
    let blob = blob_for("ssh-ed25519", 64);
    let err = parse_authorized_key(&format!("ssh-ed25519 {blob} ")).unwrap_err();
    assert!(message_of(&err).contains("space"));
    // The same line without the trailing space is the `None` case.
    assert_eq!(
        parse_authorized_key(&format!("ssh-ed25519 {blob}"))
            .unwrap()
            .comment,
        None
    );
}

#[test]
fn a_comment_is_bounded_at_256_bytes() {
    let blob = blob_for("ssh-ed25519", 64);
    let at_limit = "c".repeat(256);
    assert_eq!(
        parse_authorized_key(&format!("ssh-ed25519 {blob} {at_limit}"))
            .unwrap()
            .comment
            .as_deref(),
        Some(at_limit.as_str())
    );

    let over = "c".repeat(257);
    let err = parse_authorized_key(&format!("ssh-ed25519 {blob} {over}")).unwrap_err();
    assert!(
        message_of(&err).contains("257 bytes"),
        "{}",
        message_of(&err)
    );
}

// --- Blob semantics ---------------------------------------------------

#[test]
fn the_blob_must_declare_the_type_the_line_declares() {
    // Matching: accepted.
    let matching = format!("ssh-ed25519 {}", blob_for("ssh-ed25519", 64));
    assert!(parse_authorized_key(&matching).is_ok());

    // Mismatched: an RSA blob presented as an Ed25519 key. Every other
    // rule here passes; only the embedded algorithm name disagrees.
    let mismatched = format!("ssh-ed25519 {}", blob_for("ssh-rsa", 64));
    let err = parse_authorized_key(&mismatched).unwrap_err();
    assert!(
        message_of(&err).contains("does not match the declared key type"),
        "{}",
        message_of(&err)
    );

    // And the other direction, so the test is not passing on a length
    // coincidence between the two names.
    let swapped = format!("ssh-rsa {}", blob_for("ssh-ed25519", 64));
    assert!(parse_authorized_key(&swapped).is_err());

    // Every accepted type mismatches every other one.
    for declared in ACCEPTED_TYPES {
        for embedded in ACCEPTED_TYPES {
            let line = format!("{declared} {}", blob_for(embedded, 96));
            assert_eq!(
                parse_authorized_key(&line).is_ok(),
                declared == embedded,
                "declared {declared}, blob says {embedded}"
            );
        }
    }
}

#[test]
fn a_blob_below_thirty_two_decoded_bytes_is_rejected_and_exactly_thirty_two_is_accepted() {
    // Literals rather than MIN_BLOB_BYTES: widening the constant must fail
    // a test, not silently move the expectation with it.
    let exactly = blob_for("ssh-ed25519", 32);
    assert_eq!(decode_base64(&exactly).unwrap().len(), 32);
    assert!(
        parse_authorized_key(&format!("ssh-ed25519 {exactly}")).is_ok(),
        "a blob of exactly 32 bytes must be accepted"
    );

    let short = blob_for("ssh-ed25519", 31);
    assert_eq!(decode_base64(&short).unwrap().len(), 31);
    let err = parse_authorized_key(&format!("ssh-ed25519 {short}")).unwrap_err();
    assert!(
        message_of(&err).contains("31 bytes"),
        "{}",
        message_of(&err)
    );

    // 16 bytes is a truncated paste rather than an off-by-one.
    let truncated = blob_for("ssh-ed25519", 16);
    assert!(parse_authorized_key(&format!("ssh-ed25519 {truncated}")).is_err());
}

#[test]
fn a_blob_whose_length_is_not_a_multiple_of_four_is_rejected() {
    let blob = blob_for("ssh-ed25519", 64);
    let trimmed = &blob[..blob.len() - 1];
    let err = parse_authorized_key(&format!("ssh-ed25519 {trimmed}")).unwrap_err();
    assert!(
        message_of(&err).contains("not a multiple of 4"),
        "{}",
        message_of(&err)
    );
}

#[test]
fn three_or_more_padding_characters_are_rejected() {
    let blob = blob_for("ssh-ed25519", 64);
    let overpadded = format!("{blob}====");
    let err = parse_authorized_key(&format!("ssh-ed25519 {overpadded}")).unwrap_err();
    assert!(message_of(&err).contains("padding"), "{}", message_of(&err));
}

// --- Lines that are not keys at all -----------------------------------

#[test]
fn options_hash_comments_and_empty_lines_are_rejected() {
    let blob = blob_for("ssh-ed25519", 64);
    let rejected = [
        format!("command=\"x\" ssh-ed25519 {blob}"),
        format!("no-pty ssh-ed25519 {blob}"),
        format!("restrict ssh-ed25519 {blob}"),
        format!("environment=\"A=b\" ssh-ed25519 {blob}"),
        format!("no-pty,command=\"x\" ssh-ed25519 {blob}"),
        format!("# ssh-ed25519 {blob}"),
        format!("#ssh-ed25519 {blob}"),
        String::new(),
        " ".to_string(),
        "ssh-ed25519".to_string(),
    ];
    for line in rejected {
        assert!(
            parse_authorized_key(&line).is_err(),
            "must be rejected: {} bytes starting `{}`",
            line.len(),
            line.chars().take(12).collect::<String>()
        );
    }
}

// --- base64 codec -----------------------------------------------------

/// Deterministic pseudo-random bytes; a fixed LCG keeps the test
/// reproducible without a dependency.
fn pseudo_random(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            u8::try_from((state >> 16) & 0xff).unwrap()
        })
        .collect()
}

#[test]
fn base64_round_trips_every_length_up_to_sixty_four() {
    for len in 0..=64usize {
        let bytes = pseudo_random(len);
        let encoded = encode_base64_nopad(&bytes);
        assert!(
            !encoded.contains('='),
            "the no-pad encoder must not emit padding at length {len}"
        );
        assert_eq!(
            decode_base64(&encoded).unwrap(),
            bytes,
            "unpadded round trip at length {len}"
        );
        assert_eq!(
            decode_base64(&pad(&encoded)).unwrap(),
            bytes,
            "padded round trip at length {len}"
        );
    }
}

#[test]
fn padding_length_follows_the_input_length_modulo_three() {
    // len % 3 == 0 -> no padding, == 1 -> two `=`, == 2 -> one `=`.
    for (len, expected_padding, expected_unpadded_len) in
        [(3usize, 0usize, 4usize), (4, 2, 6), (5, 1, 7)]
    {
        let bytes = pseudo_random(len);
        let encoded = encode_base64_nopad(&bytes);
        assert_eq!(encoded.len(), expected_unpadded_len, "length {len}");
        let padded = pad(&encoded);
        assert_eq!(
            padded.len() - encoded.len(),
            expected_padding,
            "padding for length {len}"
        );
        assert_eq!(decode_base64(&padded).unwrap(), bytes);
    }
    assert_eq!(encode_base64_nopad(b""), "");
    assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
}

#[test]
fn the_decoder_rejects_bad_alphabets_and_bad_padding() {
    // Out-of-alphabet bytes, including the URL-safe alphabet, which is a
    // different encoding wearing the same shape.
    for bad in ["AAA-", "AAA_", "AA A", "AA\nA", "AAA\u{e9}", "AA*A", "AA.A"] {
        assert_eq!(decode_base64(bad), None, "must reject `{bad}`");
    }
    // Body lengths no byte string can produce, and padding that is either
    // too long, misplaced, or not on a four-character boundary.
    for bad in [
        "A", "AAAAA", "A===", "AAAA====", "AA=A", "=AAA", "AAAAA=", "AAA=A",
    ] {
        assert_eq!(decode_base64(bad), None, "must reject `{bad}`");
    }
    // The well-formed neighbours of those inputs still decode, so the test
    // proves a boundary rather than a blanket refusal.
    assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
    assert_eq!(decode_base64("AA").unwrap(), vec![0]);
    assert_eq!(decode_base64("AAA").unwrap(), vec![0, 0]);
    assert_eq!(decode_base64("AAAA").unwrap(), vec![0, 0, 0]);
    assert_eq!(decode_base64("AA==").unwrap(), vec![0]);
    assert_eq!(decode_base64("AAA=").unwrap(), vec![0, 0]);
    assert_eq!(decode_base64("AAAAAA==").unwrap(), vec![0, 0, 0, 0]);
    assert_eq!(decode_base64("/w==").unwrap(), vec![0xff]);
    assert_eq!(decode_base64("+w==").unwrap(), vec![0xfb]);
}

// --- validate_authorized_keys ------------------------------------------

fn entry(index: usize, comment: Option<&str>) -> AuthorizedKey {
    AuthorizedKey {
        key: format!("ssh-ed25519 {}", blob_for("ssh-ed25519", 64 + index * 3)),
        comment: comment.map(str::to_string),
    }
}

#[test]
fn a_valid_list_is_accepted() {
    let keys = vec![
        entry(0, None),
        entry(1, Some("alice@workstation")),
        entry(2, Some("build box #2")),
    ];
    validate_authorized_keys(&keys).unwrap();
}

#[test]
fn a_duplicate_key_is_rejected_and_names_its_index() {
    let mut keys = vec![entry(0, None), entry(1, None), entry(2, None)];
    keys[2].key = keys[0].key.clone();
    // The comments differ, so this is exactly the case the canonical `key`
    // exists to catch.
    keys[2].comment = Some("looks different, grants the same access".to_string());

    let err = validate_authorized_keys(&keys).unwrap_err();
    let message = message_of(&err);
    assert!(message.contains("entry 2"), "{message}");
    assert!(message.contains("entry 0"), "{message}");
}

#[test]
fn the_list_is_bounded_at_thirty_two_entries() {
    let full: Vec<_> = (0..32).map(|index| entry(index, None)).collect();
    assert_eq!(full.len(), 32);
    validate_authorized_keys(&full).unwrap();

    let mut over = full;
    over.push(entry(32, None));
    let err = validate_authorized_keys(&over).unwrap_err();
    assert!(message_of(&err).contains("33 keys"), "{}", message_of(&err));
}

#[test]
fn an_entry_whose_key_carries_a_comment_is_rejected() {
    let keys = vec![AuthorizedKey {
        key: valid_line("ssh-ed25519") + " smuggled",
        comment: None,
    }];
    let err = validate_authorized_keys(&keys).unwrap_err();
    assert!(
        message_of(&err).contains("carries a comment"),
        "{}",
        message_of(&err)
    );
}

#[test]
fn an_entry_with_a_malformed_key_is_rejected() {
    for key in [
        "ssh-ed25519".to_string(),
        format!("ssh-dss {}", blob_for("ssh-dss", 64)),
        format!("ssh-ed25519 {}", blob_for("ssh-rsa", 64)),
        format!("command=\"x\" {}", valid_line("ssh-ed25519")),
        format!("ssh-ed25519 {}", blob_for("ssh-ed25519", 16)),
        String::new(),
    ] {
        let keys = vec![AuthorizedKey { key, comment: None }];
        let err = validate_authorized_keys(&keys).unwrap_err();
        assert!(message_of(&err).contains("entry 0"), "{}", message_of(&err));
    }
}

#[test]
fn an_entry_with_a_bad_comment_field_is_rejected() {
    for comment in [
        "line\nfeed".to_string(),
        "tab\there".to_string(),
        "\u{7f}".to_string(),
        String::new(),
        "c".repeat(257),
    ] {
        let keys = vec![entry(0, Some(&comment))];
        let err = validate_authorized_keys(&keys).unwrap_err();
        assert!(message_of(&err).contains("entry 0"), "{}", message_of(&err));
    }
    // A shell metacharacter in the struct field is still fine.
    validate_authorized_keys(&[entry(0, Some("dev$box `n1` \"x\""))]).unwrap();
}

#[test]
fn an_empty_list_is_accepted() {
    validate_authorized_keys(&[]).unwrap();
}

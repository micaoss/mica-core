//! The wpa_supplicant configuration: golden file, order, security and SSIDs.

use super::*;

#[test]
pub(super) fn multi_network_render_matches_the_golden_file() {
    assert_eq!(
        render_config(&multi_network_client()).unwrap(),
        GOLDEN_MULTI
    );
}

#[test]
pub(super) fn render_is_deterministic() {
    let client = multi_network_client();

    assert_eq!(
        render_config(&client).unwrap(),
        render_config(&client.clone()).unwrap()
    );
}

#[test]
pub(super) fn networks_are_ordered_by_priority_with_an_explicit_priority_line() {
    let rendered = render_config(&client(
        true,
        vec![
            network("low", None, false, -5),
            network("high", None, false, 100),
            network("mid", None, false, 0),
        ],
    ))
    .unwrap();

    let order: Vec<&str> = rendered
        .lines()
        .filter_map(|line| line.strip_prefix("\tssid="))
        .collect();
    assert_eq!(order, vec!["\"high\"", "\"mid\"", "\"low\""]);
    let priorities: Vec<&str> = rendered
        .lines()
        .filter_map(|line| line.strip_prefix("\tpriority="))
        .collect();
    assert_eq!(
        priorities,
        vec!["100", "0", "-5"],
        "every block must state its own priority so wpa_supplicant's \
         selection matches the settings order"
    );
}

#[test]
pub(super) fn equal_priorities_keep_their_settings_order() {
    let rendered = render_config(&client(
        true,
        vec![
            network("first", None, false, 7),
            network("second", None, false, 7),
            network("third", None, false, 7),
        ],
    ))
    .unwrap();

    let order: Vec<&str> = rendered
        .lines()
        .filter_map(|line| line.strip_prefix("\tssid="))
        .collect();
    assert_eq!(order, vec!["\"first\"", "\"second\"", "\"third\""]);
}

#[test]
pub(super) fn a_passphrase_outside_wpa2_bounds_is_rejected() {
    // Seven characters: one short of IEEE 802.11i's minimum. Rendering it
    // would make wpa_supplicant refuse the whole configuration file.
    let err = encode_psk("short07").unwrap_err();
    assert!(err.to_string().contains("8 to 63"), "{err}");
    // Sixty-four non-hex characters: too long for a passphrase and not a
    // PMK either.
    assert!(encode_psk(&"x".repeat(64)).is_err());
    // Sixty-four hex digits ARE a raw PMK; the passphrase bounds must not
    // apply to it.
    assert!(encode_psk(&"a".repeat(64)).is_ok());
    assert!(encode_psk("exactly8").is_ok());
}

/// The bound the renderer enforces is the one `micad-settings` states, and
/// there is no second copy of it left here (the lift for the
/// length band, the for the quotable predicate).
#[test]
pub(super) fn the_lifted_psk_bound_is_the_one_the_renderer_enforces() {
    for psk in [
        "",
        "short07",
        "exactly8",
        &"x".repeat(63),
        &"x".repeat(64),
        &"a".repeat(64),
        &"A".repeat(64),
        &"x".repeat(200),
        // the quotable half of the agreement. Each of these
        // is inside the length band and was accepted by the lifted rule
        // while the renderer refused it -- accepted at a write surface,
        // dead at render time.
        "has\"quote1",
        "has\\backslash",
        "two\nlines1",
        "tab\there1",
        "caf\u{e9}-latte",
    ] {
        assert_eq!(
            micad_settings::validate_wifi_psk(psk).is_ok(),
            encode_psk(psk).is_ok(),
            "the renderer and the lifted rule disagree about a {}-character key",
            psk.len()
        );
    }
    // And the sentence is the lifted one, verbatim: a caller that reads it
    // from either side reads the same words.
    let lifted = micad_settings::validate_wifi_psk("short07").unwrap_err();
    assert_eq!(encode_psk("short07").unwrap_err().to_string(), lifted);
    // Which still never names the length observed.
    assert!(!lifted.contains('7'), "{lifted}");
}

#[test]
pub(super) fn an_open_network_emits_key_mgmt_none_and_no_psk() {
    let rendered = render_config(&client(true, vec![network("cafe", None, false, 0)])).unwrap();

    assert!(
        rendered.contains("\tkey_mgmt=NONE\n"),
        "an open network must say so: {rendered}"
    );
    assert!(
        !rendered.contains("psk="),
        "an open network must not carry a key line: {rendered}"
    );
}

#[test]
pub(super) fn a_psk_network_emits_a_key_and_never_key_mgmt_none() {
    let rendered = render_config(&client(
        true,
        vec![network("office", Some("s3cretpass"), false, 0)],
    ))
    .unwrap();

    assert!(rendered.contains("\tpsk=\"s3cretpass\"\n"), "{rendered}");
    assert!(rendered.contains("\tkey_mgmt=WPA-PSK SAE\n"), "{rendered}");
    assert!(
        !rendered.contains("key_mgmt=NONE"),
        "a protected network must never be downgraded to open: {rendered}"
    );
}

#[test]
pub(super) fn hidden_is_the_only_thing_that_emits_scan_ssid() {
    let hidden = render_config(&client(true, vec![network("lab", None, true, 0)])).unwrap();
    let visible = render_config(&client(true, vec![network("lab", None, false, 0)])).unwrap();

    assert!(hidden.contains("\tscan_ssid=1\n"), "{hidden}");
    assert!(!visible.contains("scan_ssid"), "{visible}");
}

#[test]
pub(super) fn a_hostile_ssid_cannot_inject_configuration() {
    // A quote to close the string, a brace to close the block, and a
    // newline to start a directive of its own — the whole escape.
    let hostile = "evil\"\n}\nnetwork={\n\tssid=\"pwned\"\n\tkey_mgmt=NONE\n}\n#";

    let rendered = render_config(&client(true, vec![network(hostile, None, false, 3)])).unwrap();

    assert_eq!(
        rendered.matches("network={").count(),
        1,
        "the hostile SSID opened a second block: {rendered}"
    );
    assert!(
        !rendered.contains("pwned"),
        "hostile content reached the file verbatim: {rendered}"
    );
    assert!(
        rendered.contains(&format!("\tssid={}\n", hex::encode(hostile.as_bytes()))),
        "an SSID that cannot be quoted must be emitted as hex: {rendered}"
    );
    assert_eq!(
        rendered,
        format!(
            "{GOLDEN_EMPTY}\nnetwork={{\n\tssid={}\n\tpriority=3\n\tkey_mgmt=NONE\n}}\n",
            hex::encode(hostile.as_bytes())
        ),
        "the render must be exactly one well-formed block"
    );
}

#[test]
pub(super) fn every_unquotable_ssid_shape_goes_to_hex_and_plain_ones_stay_quoted() {
    for hostile in [
        "has\"quote",
        "has\\backslash",
        "has\nnewline",
        "has\ttab",
        "has\0nul",
        "caf\u{e9}",
    ] {
        let rendered =
            render_config(&client(true, vec![network(hostile, None, false, 0)])).unwrap();
        assert!(
            rendered.contains(&format!("\tssid={}\n", hex::encode(hostile.as_bytes()))),
            "{hostile:?} must be hex-encoded: {rendered}"
        );
    }
    for plain in [
        "plain",
        "with space",
        "with#hash",
        "with'quote",
        "a-b_c.d:e",
    ] {
        let rendered = render_config(&client(true, vec![network(plain, None, false, 0)])).unwrap();
        assert!(
            rendered.contains(&format!("\tssid=\"{plain}\"\n")),
            "{plain:?} must stay quoted and readable: {rendered}"
        );
    }
}

#[test]
pub(super) fn a_sixty_four_character_hex_key_is_emitted_as_a_raw_pmk() {
    let pmk = "0123456789abcdef".repeat(4);
    assert_eq!(pmk.len(), 64);

    let rendered =
        render_config(&client(true, vec![network("office", Some(&pmk), false, 0)])).unwrap();

    assert!(
        rendered.contains(&format!("\tpsk={pmk}\n")),
        "a raw PMK must not be quoted, or wpa_supplicant reads it as an \
         over-long passphrase and rejects the file: {rendered}"
    );
    assert!(!rendered.contains(&format!("psk=\"{pmk}\"")), "{rendered}");
    // SAE needs the password itself, so a PMK network is WPA2 only and
    // offers no SAE a WPA3 access point would then fail.
    assert!(rendered.contains("\tkey_mgmt=WPA-PSK\n"), "{rendered}");
    assert!(!rendered.contains("SAE"), "{rendered}");
}

/// A passphrase network joins WPA2, WPA3-only and transition-mode access
/// points from one block, with PMF offered (SAE requires it), and the
/// global section admits hash-to-element, which WPA3 on 6 GHz requires.
#[test]
pub(super) fn a_passphrase_network_joins_wpa2_and_wpa3_and_6_ghz() {
    let rendered = render_config(&client(
        true,
        vec![network("office", Some("officepass"), false, 0)],
    ))
    .unwrap();
    assert!(rendered.contains("\nsae_pwe=2\n"), "{rendered}");
    assert!(
        rendered.contains("\tkey_mgmt=WPA-PSK SAE\n\tieee80211w=1\n\tpsk=\"officepass\"\n"),
        "{rendered}"
    );
    let open = render_config(&client(true, vec![network("guest", None, false, 0)])).unwrap();
    assert!(open.contains("\tkey_mgmt=NONE\n"), "{open}");
    assert!(
        !open.contains("ieee80211w"),
        "an open network asks for no PMF: {open}"
    );
}

#[test]
pub(super) fn a_passphrase_that_cannot_be_quoted_is_an_error_that_does_not_name_it() {
    let err = render_config(&client(
        true,
        vec![network("office", Some("bad\"key\nMORE"), false, 0)],
    ))
    .unwrap_err();

    let chain = format!("{err:#}");
    assert!(chain.contains("pre-shared key"), "{chain}");
    assert!(chain.contains("\"office\""), "{chain}");
    assert!(
        !chain.contains("bad") && !chain.contains("MORE"),
        "the key leaked into the error: {chain}"
    );
}

// ---- what apply writes ------------------------------------------------

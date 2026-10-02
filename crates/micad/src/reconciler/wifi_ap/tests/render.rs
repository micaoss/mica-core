//! The hostapd configuration: golden file, security, country, channel, SSID and key.

use super::*;

#[test]
pub(super) fn the_render_matches_the_golden_file() {
    assert_eq!(
        render_config(&lab_ap(), "mica-lab", "labsecret1").unwrap(),
        GOLDEN_CONFIG
    );
}

#[test]
pub(super) fn the_render_is_deterministic() {
    let ap = lab_ap();
    assert_eq!(
        render_config(&ap, "mica-lab", "labsecret1").unwrap(),
        render_config(&ap.clone(), "mica-lab", "labsecret1").unwrap()
    );
}

#[test]
pub(super) fn the_render_is_wpa2_psk_and_never_open() {
    let rendered = render_config(&lab_ap(), "mica-lab", "labsecret1").unwrap();

    for expected in [
        "wpa=2\n",
        "wpa_key_mgmt=WPA-PSK\n",
        "rsn_pairwise=CCMP\n",
        "auth_algs=1\n",
    ] {
        assert!(
            rendered.contains(expected),
            "{expected:?} missing, so the access point would not be WPA2-PSK: {rendered}"
        );
    }
    assert!(
        !rendered.contains("wpa=0") && !rendered.contains("wpa=1"),
        "an access point carrying a device credential must not be open or WPA1: {rendered}"
    );
}

#[test]
pub(super) fn the_country_code_is_emitted_and_advertised() {
    let rendered = render_config(&lab_ap(), "mica-lab", "labsecret1").unwrap();

    assert!(rendered.contains("country_code=DE\n"), "{rendered}");
    assert!(
        rendered.contains("ieee80211d=1\n"),
        "without ieee80211d the regulatory domain is carried and not advertised: {rendered}"
    );
}

#[test]
pub(super) fn a_country_code_that_is_not_a_regulatory_domain_is_rejected() {
    for bad in ["", "U", "USA", "U1", "US\nssid=pwned", "u s"] {
        assert!(
            validate_country_code(bad).is_err(),
            "{bad:?} was accepted as a regulatory domain"
        );
    }
    for good in ["US", "DE", "JP", "gb"] {
        assert!(
            validate_country_code(good).is_ok(),
            "{good:?} was rejected as a regulatory domain"
        );
    }
}

#[test]
pub(super) fn a_channel_outside_the_two_point_four_gigahertz_band_is_rejected() {
    for bad in [0u8, 15, 36, 255] {
        let ap = WifiApSettings {
            channel: bad,
            ..lab_ap()
        };
        assert!(
            render_config(&ap, "mica-lab", "labsecret1").is_err(),
            "channel {bad} was accepted"
        );
    }
    for good in [1u8, 6, 11, 14] {
        let ap = WifiApSettings {
            channel: good,
            ..lab_ap()
        };
        let rendered = render_config(&ap, "mica-lab", "labsecret1").unwrap();
        assert!(
            rendered.contains(&format!("channel={good}\n")),
            "{rendered}"
        );
    }
}

#[test]
pub(super) fn a_hostile_ssid_cannot_inject_configuration() {
    // A newline to end the directive and a whole second directive after it
    // — the entire escape, because hostapd has no quoting to break out of.
    let hostile = "evil\nssid=pwned\nwpa=0\n#";

    let rendered = render_config(&lab_ap(), hostile, "labsecret1").unwrap();

    assert!(
        !rendered.contains("pwned"),
        "hostile content reached the file verbatim: {rendered}"
    );
    assert!(
        !rendered.contains("wpa=0"),
        "a hostile SSID turned the access point open: {rendered}"
    );
    assert_eq!(
        rendered
            .lines()
            .filter(|line| line.contains("ssid"))
            .count(),
        2,
        "exactly one SSID directive and one ignore_broadcast_ssid: {rendered}"
    );
    assert!(
        rendered.contains(&format!("ssid2={}\n", hex::encode(hostile.as_bytes()))),
        "an SSID that cannot be carried raw must be emitted as hex: {rendered}"
    );
    assert_eq!(
        rendered,
        GOLDEN_CONFIG.replace(
            "ssid=mica-lab\n",
            &format!("ssid2={}\n", hex::encode(hostile.as_bytes()))
        ),
        "the render must be the golden file with only the SSID directive changed"
    );
}

#[test]
pub(super) fn every_unrepresentable_ssid_goes_to_hex_and_plain_ones_stay_readable() {
    for hostile in [
        "has\nnewline",
        "has\ttab",
        "has\0nul",
        "has\"quote",
        "has\\backslash",
        "caf\u{e9}",
        " leading",
        "trailing ",
    ] {
        let rendered = render_config(&lab_ap(), hostile, "labsecret1").unwrap();
        assert!(
            rendered.contains(&format!("ssid2={}\n", hex::encode(hostile.as_bytes()))),
            "{hostile:?} must be hex-encoded: {rendered}"
        );
        assert!(
            !rendered.contains(&format!("ssid={hostile}")),
            "{hostile:?} reached the file raw: {rendered}"
        );
    }
    for plain in [
        "mica-lab",
        "with space",
        "with#hash",
        "with'quote",
        "a.b:c_d",
    ] {
        let rendered = render_config(&lab_ap(), plain, "labsecret1").unwrap();
        assert!(
            rendered.contains(&format!("ssid={plain}\n")),
            "{plain:?} must stay readable: {rendered}"
        );
        assert!(!rendered.contains("ssid2="), "{rendered}");
    }
}

#[test]
pub(super) fn an_ssid_ieee_802_11_cannot_carry_is_rejected() {
    assert!(validate_ssid("").is_err(), "an empty SSID was accepted");
    assert!(validate_ssid(&"a".repeat(33)).is_err(), "33 bytes accepted");
    assert!(validate_ssid(&"a".repeat(32)).is_ok(), "32 bytes rejected");
    assert!(validate_ssid("mica-lab").is_ok());
    assert!(
        render_config(&lab_ap(), &"a".repeat(33), "labsecret1").is_err(),
        "hostapd rejects the whole file on an over-long SSID"
    );
}

#[test]
pub(super) fn a_sixty_four_character_hex_key_is_emitted_as_a_raw_pmk() {
    let pmk = "0123456789abcdef".repeat(4);
    assert_eq!(pmk.len(), 64);

    let rendered = render_config(&lab_ap(), "mica-lab", &pmk).unwrap();

    assert!(
        rendered.contains(&format!("wpa_psk={pmk}\n")),
        "a raw PMK must use wpa_psk; wpa_passphrase would be an over-long \
         passphrase and hostapd would reject the file: {rendered}"
    );
    assert!(!rendered.contains("wpa_passphrase="), "{rendered}");
}

#[test]
pub(super) fn a_key_hostapd_cannot_carry_is_an_error_that_does_not_name_it() {
    for bad in ["short7X", &"x".repeat(64), "has\nnewlineXX", "trailingX "] {
        let err = render_config(&lab_ap(), "mica-lab", bad).unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("pre-shared key"), "{bad:?} produced {chain}");
        assert!(
            !chain.contains(bad),
            "the key leaked into the error: {chain}"
        );
    }
    // The boundaries themselves are accepted.
    for good in ["8charsXX", &"y".repeat(63)] {
        assert!(
            render_config(&lab_ap(), "mica-lab", good).is_ok(),
            "{good:?} is a legal WPA2 passphrase and was rejected"
        );
    }
}

#[test]
pub(super) fn the_length_refusal_is_the_same_sentence_for_every_length() {
    // Interpolating any property of the secret — its length included —
    // would make the sentence vary with the input; an identical refusal
    // for a short key and a long one proves the message names only the
    // rule. The refusal reaches an API client through the live-state tree
    // and the apply-task record, so a length in it is a disclosed fact
    // about a secret.
    let short = psk_directive("short7X").unwrap_err().to_string();
    let long = psk_directive(&"y".repeat(70)).unwrap_err().to_string();
    assert_eq!(
        short, long,
        "the refusal varies with the key, so it names a property of the secret"
    );
    assert!(
        !short.contains('7'),
        "the rejected key's length leaked into the refusal: {short}"
    );
    assert!(
        !long.contains("70"),
        "the rejected key's length leaked into the refusal: {long}"
    );
}

// ---- derived SSID -----------------------------------------------------

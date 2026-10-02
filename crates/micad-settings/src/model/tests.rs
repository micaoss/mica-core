use super::*;

#[test]
fn default_roundtrips_via_toml() {
    let settings = Settings::default();
    let text = toml::to_string(&settings).unwrap();
    let parsed: Settings = toml::from_str(&text).unwrap();
    assert_eq!(parsed, settings);
    assert_eq!(parsed.hostname, "mica");
    assert!(parsed.network.is_empty());
    assert!(parsed.access.web_admin.is_none());
    assert_eq!(parsed.access.ssh, SshSettings::default());
    assert_eq!(parsed.access.console, ConsoleSettings::default());
    assert_eq!(parsed.access.device, DeviceCredentialSettings::default());
    assert!(parsed.access.api_tokens.is_empty());
    assert_eq!(parsed.provisioning, ProvisioningSettings::default());
    assert_eq!(parsed.wifi, WifiSettings::default());
    assert_eq!(parsed.container, ContainerSettings::default());
    assert_eq!(parsed.mqtt, MqttSettings::default());
    assert_eq!(parsed.time, TimeSettings::default());
}

/// The time defaults, spelled out: no managed servers (the image fallback
/// pool applies) and the UTC presentation zone the contract starts from.
#[test]
fn time_defaults_are_no_servers_and_utc() {
    let settings = Settings::default();
    assert!(settings.time.ntp.servers.is_empty());
    assert_eq!(settings.time.timezone, "UTC");

    // There is deliberately no switch to find here: timesyncd is an
    // always-running base service, and a field named like one appearing
    // in this subtree is the regression this pins against.
    let time = toml::to_string(&settings.time).unwrap();
    assert!(!time.contains("enabled"), "{time}");
    assert!(!time.contains("Poll"), "{time}");
}

/// The renderer writes `NTP=` space-separated into an ini drop-in, so
/// everything that could end the assignment or smuggle another is refused
/// at the write surface rather than stored and dead at render time.
#[test]
fn an_ntp_server_the_renderer_cannot_carry_is_refused() {
    for server in [
        "",
        "pool one.example",
        "pool\tone",
        "two\nlines",
        "a=b",
        "#comment",
        "host_name.example",
        "höst.example",
    ] {
        assert!(
            validate_ntp_servers(&[server.to_string()]).is_err(),
            "{server:?} must be refused"
        );
    }

    assert!(
        validate_ntp_servers(&[
            "0.debian.pool.ntp.org".to_string(),
            "time.example-corp.com".to_string(),
            "192.0.2.7".to_string(),
            "2001:db8::123".to_string(),
        ])
        .is_ok()
    );
}

#[test]
fn the_ntp_server_list_is_bounded_and_duplicate_free() {
    let too_many: Vec<String> = (0..=MAX_NTP_SERVERS)
        .map(|index| format!("ntp{index}.example"))
        .collect();
    let err = validate_ntp_servers(&too_many).unwrap_err();
    assert!(err.contains(&MAX_NTP_SERVERS.to_string()), "{err}");

    let twice = vec!["ntp.example".to_string(), "ntp.example".to_string()];
    let err = validate_ntp_servers(&twice).unwrap_err();
    assert!(err.contains("twice"), "{err}");

    let long = "a".repeat(254);
    assert!(validate_ntp_servers(&[long]).is_err());
}

/// Deterministic on every host: the rule is the tzdata name grammar and
/// never a lookup against the machine's own zoneinfo tree.
#[test]
fn a_timezone_is_validated_by_grammar_not_by_the_host_tzdata() {
    for zone in [
        "UTC",
        "Etc/GMT+8",
        "Europe/Berlin",
        "America/Argentina/Buenos_Aires",
        "America/Port-au-Prince",
        // Grammatically fine and almost certainly not a real zone: the
        // existence check belongs to reconcile time, not to this rule.
        "Atlantis/Made_Up",
    ] {
        assert!(validate_timezone_name(zone).is_ok(), "{zone:?}");
    }
    for zone in [
        "",
        "/Etc/UTC",
        "Etc/",
        "Etc//UTC",
        "../etc/shadow",
        "Europe/..",
        "Europe/Ber lin",
        "Europe/Berlin\n",
        "Europe/Bërlin",
        &"Z/".repeat(40),
    ] {
        assert!(validate_timezone_name(zone).is_err(), "{zone:?}");
    }
}

/// The write surface enforces the two `time` predicates through the tree
/// itself, so no caller of `Settings::set` can store what the reconciler
/// cannot render — and an unrelated write leaves a hand-edited `time`
/// subtree alone, the same property the network keys have.
#[test]
fn a_time_write_is_validated_and_an_unrelated_write_is_not() {
    let mut settings = Settings::default();
    settings
        .set("time.timezone", Value::from("Europe/Berlin"))
        .unwrap();
    assert_eq!(settings.time.timezone, "Europe/Berlin");

    let err = settings
        .set("time.timezone", Value::from("Europe/Ber lin"))
        .unwrap_err();
    assert!(matches!(err, SettingsError::Validation { .. }), "{err:?}");
    assert_eq!(settings.time.timezone, "Europe/Berlin");

    settings
        .set(
            "time.ntp.servers",
            serde_json::json!(["0.pool.ntp.org", "192.0.2.7"]),
        )
        .unwrap();
    assert_eq!(settings.time.ntp.servers.len(), 2);
    let err = settings
        .set("time.ntp.servers", serde_json::json!(["bad server"]))
        .unwrap_err();
    assert!(matches!(err, SettingsError::Validation { .. }), "{err:?}");
    assert_eq!(settings.time.ntp.servers.len(), 2);

    // An unrelated write over a tree whose `time` subtree would no longer
    // validate must still land: the rule is about the write, not the tree.
    let mut hand_edited: Settings = settings.clone();
    hand_edited.time.timezone = "not a zone!".to_string();
    hand_edited.set("hostname", Value::from("edge-42")).unwrap();
    assert_eq!(hand_edited.hostname, "edge-42");
    assert_eq!(hand_edited.time.timezone, "not a zone!");
}

/// The MQTT defaults, spelled out: off, loopback, and no auth. The switch
/// is what an operator turns on; the listener is what the broker is
/// rendered from, and neither constrains the other.
#[test]
fn mqtt_defaults_are_off_and_loopback() {
    let settings = Settings::default();
    assert!(!settings.mqtt.enabled);
    assert_eq!(settings.mqtt.listen.address, "127.0.0.1");
    assert_eq!(settings.mqtt.listen.port, 1883);
    assert!(!settings.mqtt.auth.enabled);

    // No credential field exists in this management subtree; the broker's
    // accounts live in a mode-restricted STATE file instead.
    let mqtt = toml::to_string(&settings.mqtt).unwrap();
    assert!(!mqtt.contains("password"), "{mqtt}");
    assert!(!mqtt.contains("username"), "{mqtt}");
}

#[test]
fn no_secret_is_present_in_a_freshly_built_tree() {
    // A non-None default here would be a fleet-wide shared secret baked
    // into a byte-identical signed rootfs.
    let settings = Settings::default();
    assert_eq!(settings.access.device.password_hash, None);
    assert_eq!(settings.access.device.generation, 0);
    assert_eq!(settings.access.web_admin, None);
    assert!(settings.access.api_tokens.is_empty());
    assert_eq!(settings.wifi.ap.psk, None);
    assert_eq!(settings.wifi.ap.ssid, None);
    assert!(settings.wifi.client.networks.is_empty());
    assert_eq!(settings.provisioning.device_id, None);

    let text = toml::to_string(&settings).unwrap();
    assert!(
        !text.contains("psk"),
        "serialized tree must hold no key: {text}"
    );
    assert!(
        !text.contains("passwordHash"),
        "serialized tree must hold no credential: {text}"
    );
    assert!(
        !text.contains("apiTokens"),
        "serialized tree must hold no credential: {text}"
    );
}

/// The empty list is not written out, and that is what makes the v7 -> v8
/// bump additive: a device that never minted a token has a v8 document
/// whose only difference from its v7 form is the version integer.
#[test]
fn an_empty_token_list_is_not_serialized() {
    let text = toml::to_string(&Settings::default()).unwrap();
    assert!(!text.contains("apiTokens"), "{text}");

    let mut with_token = Settings::default();
    with_token.access.api_tokens.push(sample_token());
    let text = toml::to_string(&with_token).unwrap();
    assert!(text.contains("[[access.apiTokens]]"), "{text}");
}

/// The wire names are the tree's camelCase convention, and the entry
/// carries the four fields and no fifth.
#[test]
fn a_token_round_trips_through_toml_under_its_camel_case_name() {
    let mut settings = Settings::default();
    settings.access.api_tokens.push(sample_token());

    let text = toml::to_string(&settings).unwrap();
    let parsed: Settings = toml::from_str(&text).unwrap();
    assert_eq!(parsed, settings);

    // Through the dot-path API the JSON shape is the same one apid reads
    // out of `GetSettings("access")`.
    let value = settings.get("access.apiTokens").unwrap();
    let entry = &value.as_array().unwrap()[0];
    let fields: Vec<&str> = entry
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(fields, ["created", "hash", "id", "name"]);
    assert_eq!(entry["id"], Value::String("3f2a9c41".to_string()));
    assert_eq!(entry["created"], Value::from(1_700_000_000_u64));
}

/// The path syntax has no array indexing, so mint and revoke are
/// read-modify-write of the whole list. This pins that the whole-array
/// write works and that the indexed one does not silently appear to.
#[test]
fn the_token_list_is_written_whole_and_not_by_index() {
    let mut settings = Settings::default();
    let one = serde_json::to_value([sample_token()]).unwrap();
    settings.set("access.apiTokens", one).unwrap();
    assert_eq!(settings.access.api_tokens.len(), 1);

    // An index is not a path segment; a write through one must not land.
    assert!(
        settings
            .set("access.apiTokens.0.name", Value::from("x"))
            .is_err()
    );
    assert_eq!(settings.access.api_tokens[0].name, "ci-deploy");

    settings
        .set("access.apiTokens", Value::Array(Vec::new()))
        .unwrap();
    assert!(settings.access.api_tokens.is_empty());
}

/// A passphrase the station renderer cannot carry is refused here, so it
/// cannot be accepted at a write surface and then die at render time.
#[test]
fn a_passphrase_the_renderer_cannot_quote_is_refused() {
    for psk in [
        "has\"quote1",
        "has\\backslash",
        "two\nlines1",
        "tab\there1",
        "cafe\u{301}-latte",
        "caf\u{e9}-latte",
    ] {
        let Err(err) = validate_wifi_psk(psk) else {
            panic!("{psk:?} must be refused");
        };
        assert!(err.contains("pre-shared key"), "{psk:?}: {err}");
        // A refusal never echoes the value; a key is a secret and this
        // sentence reaches an HTTP client.
        assert!(!err.contains(psk), "the refusal echoed the key: {err}");
    }

    // Everything IEEE 802.11i's own passphrase alphabet allows and the
    // renderer can quote still passes, and so does a raw PMK.
    assert!(validate_wifi_psk("hunter2hunter2").is_ok());
    assert!(validate_wifi_psk("p@ssw0rd!#$%^&*()_+-=[]{};:',.<>/? ~`").is_ok());
    assert!(validate_wifi_psk(&"a".repeat(RAW_PMK_LEN)).is_ok());
}

/// One well-formed entry, spelled the way the store spells it.
fn sample_token() -> ApiToken {
    ApiToken {
        id: "3f2a9c41".to_string(),
        name: "ci-deploy".to_string(),
        hash: "9".repeat(64),
        created: 1_700_000_000,
    }
}

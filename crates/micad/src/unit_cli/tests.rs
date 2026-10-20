use super::*;

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_string).collect()
}

#[test]
fn every_command_parses_and_a_misuse_is_named() {
    assert_eq!(
        parse(&words("install /srv/x/tailscaled.service")),
        Ok(Command::Install(Install {
            file: "/srv/x/tailscaled.service".into(),
            ..Install::default()
        }))
    );
    assert_eq!(
        parse(&words("install - --name tailscaled --conf t.conf --enable")),
        Ok(Command::Install(Install {
            file: "-".into(),
            name: Some("tailscaled".into()),
            conf: Some("t.conf".into()),
            enable: true,
        }))
    );
    assert_eq!(
        parse(&words("enable x")),
        Ok(Command::Enable {
            name: "x".into(),
            enabled: true
        })
    );
    assert_eq!(
        parse(&words("disable x")),
        Ok(Command::Enable {
            name: "x".into(),
            enabled: false
        })
    );
    assert_eq!(parse(&words("remove x")), Ok(Command::Remove("x".into())));
    assert_eq!(
        parse(&words("restart x")),
        Ok(Command::Verb {
            verb: Verb::Restart,
            name: "x".into()
        })
    );
    assert_eq!(parse(&words("list")), Ok(Command::List));
    assert_eq!(
        parse(&words("logs x --tail 20")),
        Ok(Command::Logs {
            name: "x".into(),
            tail: 20
        })
    );
    assert_eq!(parse(&words("")), Ok(Command::Help));

    for (line, says) in [
        ("install", "needs a file"),
        ("install -", "needs --name"),
        ("install a b", "unexpected argument: b"),
        ("install a --name", "--name needs a value"),
        ("install a --name x --name y", "given twice"),
        ("enable", "unknown command"),
        ("logs x --tail 0", "1 to 200"),
        ("logs x --tail 201", "1 to 200"),
        ("enable --now x", "unknown command"),
    ] {
        let err = parse(&words(line)).unwrap_err();
        assert!(err.contains(says), "{line}: {err}");
    }
}

#[test]
fn a_systemd_name_without_a_type_is_a_service() {
    assert_eq!(unit_name("systemd", "tailscaled"), "tailscaled.service");
    assert_eq!(unit_name("systemd", "x.timer"), "x.timer");
    assert_eq!(unit_name("openrc", "tailscaled"), "tailscaled");
    assert_eq!(
        unit_path("tailscaled.service"),
        "services.units.\"tailscaled.service\""
    );
    assert_eq!(unit_path("tailscaled"), "services.units.tailscaled");
}

#[test]
fn the_list_names_who_manages_each_service() {
    let units = json!({
        "init": "systemd",
        "available": true,
        "units": [
            { "name": "hand.service", "managed": false, "enabled": false, "active": "inactive" },
            { "name": "x.service", "managed": true, "enabled": true, "active": "active" },
            { "name": "y", "managed": false, "enabled": false, "refused": "for openrc; this device runs systemd" },
        ],
    });
    assert_eq!(
        render_list(&units),
        "\
NAME          ENABLED  ACTIVE    MANAGED
hand.service  no       inactive  by hand
x.service     yes      active    micad
y             no       -         refused: for openrc; this device runs systemd
"
    );
    let none = json!({ "init": "systemd", "available": false, "units": [] });
    assert!(render_list(&none).starts_with("this root has no mica-unit"));
}

#[test]
fn a_log_is_its_newest_lines_or_the_reason_it_is_not() {
    let log = json!({ "available": true, "lines": ["a", "b", "c"], "truncated": false });
    assert_eq!(render_log(&log, 2).unwrap(), "b\nc\n");
    assert_eq!(render_log(&log, 200).unwrap(), "a\nb\nc\n");
    let absent = micad_settings::absent("the log could not be read: no journal");
    assert!(
        render_log(&absent, 2)
            .unwrap_err()
            .to_string()
            .contains("no journal")
    );
}

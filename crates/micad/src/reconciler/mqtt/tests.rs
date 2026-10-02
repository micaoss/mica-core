use std::path::Path;

use micad_settings::{MqttAuthSettings, MqttListenSettings};

use super::super::systemd::mock::MockUnitControl;
use super::*;

/// What `apply` renders for default `mqtt` settings.
const GOLDEN_DEFAULTS: &str =
    "listen_address = \"127.0.0.1\"\nlisten_port = 1883\nauth_enabled = false\n";
const GOLDEN_IDENTITY: &str = "MICA_MQTT_DEVICE_ID=00112233445566778899aabbccddeeff\n";

fn mqtt_settings(enabled: bool, address: &str, port: u16, auth: bool) -> MqttSettings {
    MqttSettings {
        enabled,
        listen: MqttListenSettings {
            address: address.to_string(),
            port,
        },
        auth: MqttAuthSettings { enabled: auth },
    }
}

fn settings_with(mqtt: MqttSettings) -> Settings {
    let mut settings = Settings {
        mqtt,
        ..Settings::default()
    };
    settings.provisioning.device_id = Some("00112233445566778899aabbccddeeff".to_string());
    settings
}

/// The two above composed, because every unit test names all four values
/// and nothing else in the tree.
fn settings(enabled: bool, address: &str, port: u16, auth: bool) -> Settings {
    settings_with(mqtt_settings(enabled, address, port, auth))
}

/// Reconciler rendering into a directory that does not exist yet, so every
/// test also proves the parent is created, and driving both units through
/// a mock starting at `active`/`file_state`.
fn fixture(
    dir: &Path,
    active: &str,
    file_state: &str,
) -> (MqttReconciler<MockUnitControl>, PathBuf) {
    let config = dir.join("mica").join("mqtt-broker.toml");
    (
        MqttReconciler::new(config.clone(), MockUnitControl::new(active, file_state)),
        config,
    )
}

#[tokio::test]
async fn disabled_to_enabled_starts_the_broker_before_the_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings(true, "127.0.0.1", 1883, false))
        .await
        .unwrap();

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable mica-mqtt-broker.service".to_string(),
            "start mica-mqtt-broker.service".to_string(),
            "enable mica-mqttd.service".to_string(),
            "start mica-mqttd.service".to_string(),
        ]
    );
}

#[tokio::test]
async fn enabled_to_disabled_stops_the_bridge_before_the_broker() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "active", "enabled");

    reconciler
        .apply(&settings(false, "127.0.0.1", 1883, false))
        .await
        .unwrap();

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "stop mica-mqttd.service".to_string(),
            "disable mica-mqttd.service".to_string(),
            "stop mica-mqtt-broker.service".to_string(),
            "disable mica-mqtt-broker.service".to_string(),
        ]
    );
}

#[tokio::test]
async fn a_second_apply_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "disabled");
    let unchanged = settings(true, "127.0.0.1", 1883, false);

    reconciler.apply(&unchanged).await.unwrap();
    let after_first = reconciler.control.calls();
    reconciler.apply(&unchanged).await.unwrap();

    assert_eq!(
        reconciler.control.calls(),
        after_first,
        "a repeated apply against an unchanged system must issue no calls"
    );
}

#[tokio::test]
async fn a_changed_listen_config_restarts_the_running_broker() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings(true, "127.0.0.1", 1883, false))
        .await
        .unwrap();
    let after_first = reconciler.control.calls().len();
    reconciler
        .apply(&settings(true, "127.0.0.1", 1884, false))
        .await
        .unwrap();

    // Restarted, not reloaded: the unit carries no ExecReload. And only the
    // broker -- the bridge does not read this config.
    assert_eq!(
        reconciler.control.calls()[after_first..],
        ["restart mica-mqtt-broker.service".to_string()]
    );
}

#[tokio::test]
async fn a_changed_device_identity_restarts_only_the_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "disabled");
    let initial = settings(true, "127.0.0.1", 1883, false);

    reconciler.apply(&initial).await.unwrap();
    let after_first = reconciler.control.calls().len();

    let mut changed = initial;
    changed.provisioning.device_id = Some("ffeeddccbbaa99887766554433221100".to_string());
    reconciler.apply(&changed).await.unwrap();

    assert_eq!(
        reconciler.control.calls()[after_first..],
        ["restart mica-mqttd.service".to_string()]
    );
}

/// There is deliberately no gate here -- nothing refuses to start a
/// broker bound off-host with authentication disabled. `mqtt.enabled` is a
/// master switch and nothing else; `listen` and `auth` are a separate
/// configuration that nothing may refuse to start on. The warning in
/// `apply` is the whole of the response. Do not delete this test in order
/// to add the gate.
#[tokio::test]
async fn off_host_without_auth_still_starts() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings(true, "0.0.0.0", 1883, false))
        .await
        .expect("an off-host bind without auth warns; it must never fail");

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable mica-mqtt-broker.service".to_string(),
            "start mica-mqtt-broker.service".to_string(),
            "enable mica-mqttd.service".to_string(),
            "start mica-mqttd.service".to_string(),
        ]
    );
    assert_eq!(state["enabled"], serde_json::json!(true));
}

/// The other half of the StartLimit amendment. `mica-mqtt-broker.service`
/// carries `StartLimitIntervalSec=60` / `StartLimitBurst=5`, and inside
/// that window systemd REFUSES start jobs on a unit that has exhausted its
/// burst. Converging by "not active, so start it" therefore issues a start
/// that cannot succeed, and the operator who has just fixed the address
/// would have to save again after the minute expired -- with nothing
/// telling them so.
#[tokio::test]
async fn a_failed_broker_is_reset_before_it_is_started() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "enabled");
    reconciler.control.set_active_state(BROKER_UNIT, "failed");

    reconciler
        .apply(&settings(true, "127.0.0.1", 1883, false))
        .await
        .unwrap();

    // ORDER is the assertion: a reset AFTER the start would clear the
    // failure and leave the broker still down.
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "reset-failed mica-mqtt-broker.service".to_string(),
            "start mica-mqtt-broker.service".to_string(),
            "start mica-mqttd.service".to_string(),
        ]
    );
}

/// No wasted call on the healthy path, and -- more to the point -- a call
/// log in which `reset-failed` means something. Issued on every apply it
/// would stop distinguishing the broker that needed rescuing from the one
/// that never failed.
#[tokio::test]
async fn a_broker_that_is_not_failed_is_not_reset() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings(true, "127.0.0.1", 1883, false))
        .await
        .unwrap();

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable mica-mqtt-broker.service".to_string(),
            "start mica-mqtt-broker.service".to_string(),
            "enable mica-mqttd.service".to_string(),
            "start mica-mqttd.service".to_string(),
        ],
        "a broker that never failed must not be reset-failed"
    );
}

/// A start systemd REFUSES must not fail the reconcile. `apply` covers the
/// whole `mqtt` subtree, so an `Err` here fails `mqtt.enabled` itself over
/// a unit in start-limit cool-off -- the same coupling that
/// `an_address_that_cannot_parse_still_starts_both_units` exists to
/// forbid, arriving one step later. Worse, reconcilers run together: an
/// unrelated hostname or WiFi write would fail on a broker in cool-off that
/// has nothing to do with it. Do not "fix" this into an `Err`.
#[tokio::test]
async fn a_refused_broker_start_does_not_fail_the_reconcile() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "enabled");
    reconciler.control.refuse_start(BROKER_UNIT);

    let state = reconciler
        .apply(&settings(true, "127.0.0.1", 1883, false))
        .await
        .expect("a refused start warns; it must never fail the reconcile");

    // The bridge is still driven: the broker's refusal must not abort the
    // rest of the subtree's convergence either.
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "start mica-mqtt-broker.service".to_string(),
            "start mica-mqttd.service".to_string(),
        ]
    );
    // And the failure is REPORTED rather than hidden. Both units are still
    // in the published state, and the broker's `activeState` is what it
    // really is -- which is what the apid pane renders and points at
    // `journalctl -u mica-mqtt-broker`.
    assert_eq!(
        state["units"],
        json!([
            {
                "unit": "mica-mqtt-broker.service",
                "activeState": "inactive",
                "unitFileState": "enabled",
            },
            {
                "unit": "mica-mqttd.service",
                "activeState": "active",
                "unitFileState": "enabled",
            },
        ])
    );
}

/// The bridge gets the same treatment as the broker, for the same reason:
/// `mqtt.enabled` is one switch over two units, and neither of them being
/// startable is a reason to fail the switch.
#[tokio::test]
async fn a_refused_bridge_start_does_not_fail_the_reconcile_either() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "enabled");
    reconciler.control.refuse_start(BRIDGE_UNIT);

    let state = reconciler
        .apply(&settings(true, "127.0.0.1", 1883, false))
        .await
        .expect("a refused bridge start warns; it must never fail the reconcile");

    assert_eq!(state["enabled"], json!(true));
    assert_eq!(state["units"][0]["activeState"], json!("active"));
    assert_eq!(state["units"][1]["activeState"], json!("inactive"));
}

/// Stop and disable keep propagating. A start limit cannot cause them, and
/// `mqtt.enabled = false` that quietly left a broker listening is a failure
/// worth failing on -- the leniency above is scoped to the start path and
/// must not spread.
#[tokio::test]
async fn turning_the_switch_off_still_propagates_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "active", "enabled");
    // Refusing `start` is the only injectable failure, and the off path
    // must never reach it: what this asserts is that turning the switch off
    // issues stops and disables and no start at all, so the `?` on those
    // calls is still the code that runs.
    reconciler.control.refuse_start(BROKER_UNIT);
    reconciler.control.refuse_start(BRIDGE_UNIT);

    reconciler
        .apply(&settings(false, "127.0.0.1", 1883, false))
        .await
        .unwrap();

    let calls = reconciler.control.calls();
    assert!(
        calls.iter().all(|call| !call.starts_with("start ")),
        "the off path must issue no start: {calls:?}"
    );
    assert_eq!(
        calls,
        vec![
            "stop mica-mqttd.service".to_string(),
            "disable mica-mqttd.service".to_string(),
            "stop mica-mqtt-broker.service".to_string(),
            "disable mica-mqtt-broker.service".to_string(),
        ]
    );
}

#[tokio::test]
async fn apply_writes_the_golden_config_creating_its_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, config) = fixture(dir.path(), "inactive", "disabled");
    assert!(!config.parent().unwrap().exists());

    reconciler
        .apply(&settings_with(MqttSettings::default()))
        .await
        .unwrap();

    assert!(config.parent().unwrap().is_dir());
    assert_eq!(std::fs::read_to_string(&config).unwrap(), GOLDEN_DEFAULTS);
    assert_eq!(
        std::fs::read_to_string(config.parent().unwrap().join(IDENTITY_FILE_NAME)).unwrap(),
        GOLDEN_IDENTITY
    );
}

/// An identity the environment grammar cannot carry withholds the bridge
/// and nothing else. The broker does not read the identity, so it is
/// driven as usual, and the reconcile of `mqtt.enabled` succeeds: the
/// master switch is not coupled to the provisioning subtree any more than
/// it is to `listen`.
#[tokio::test]
async fn an_unsafe_device_identity_withholds_only_the_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "disabled");
    let mut invalid = settings(true, "127.0.0.1", 1883, false);
    invalid.provisioning.device_id = Some("device id$injected".to_string());

    let state = reconciler
        .apply(&invalid)
        .await
        .expect("an unsafe identity warns; it must not fail the reconcile");

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable mica-mqtt-broker.service".to_string(),
            "start mica-mqtt-broker.service".to_string(),
        ],
        "the bridge must not start against an invalid identity; the broker still does"
    );
    assert_eq!(state["units"][1]["activeState"], json!("inactive"));
}

/// The off path never depends on the identity render. A device whose
/// identity fails validation must still be able to stop both units.
#[tokio::test]
async fn an_unsafe_device_identity_does_not_block_the_off_path() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "active", "enabled");
    let mut invalid = settings(false, "127.0.0.1", 1883, false);
    invalid.provisioning.device_id = Some("device id$injected".to_string());

    reconciler
        .apply(&invalid)
        .await
        .expect("turning the switch off must not depend on the identity");

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "stop mica-mqttd.service".to_string(),
            "disable mica-mqttd.service".to_string(),
            "stop mica-mqtt-broker.service".to_string(),
            "disable mica-mqtt-broker.service".to_string(),
        ]
    );
}

#[tokio::test]
async fn the_config_is_rendered_even_while_the_switch_is_off() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, config) = fixture(dir.path(), "inactive", "disabled");

    reconciler
        .apply(&settings(false, "10.0.0.5", 8883, true))
        .await
        .unwrap();

    // So turning the switch on does not have to wait for a second
    // reconcile to get a config that matches the settings.
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "listen_address = \"10.0.0.5\"\nlisten_port = 8883\nauth_enabled = true\n"
    );
}

#[test]
fn the_render_is_deterministic() {
    let mqtt = mqtt_settings(true, "10.0.0.5", 8883, true);
    assert_eq!(render_config(&mqtt), render_config(&mqtt));
}

#[test]
fn a_listen_address_is_loopback_off_host_or_not_an_address() {
    use ListenAddress::{Loopback, OffHost, Unparseable};

    assert_eq!(classify_listen_address("127.0.0.1"), Loopback);
    assert_eq!(classify_listen_address("127.0.0.2"), Loopback);
    assert_eq!(classify_listen_address("::1"), Loopback);
    assert_eq!(classify_listen_address("0.0.0.0"), OffHost);
    assert_eq!(classify_listen_address("10.0.0.5"), OffHost);
    assert_eq!(classify_listen_address("::"), OffHost);
    // A name, not an address. The broker does not resolve names, so this
    // is NOT an off-host bind -- it is not a bind at all, and it gets its
    // own warning rather than one claiming the network can reach it.
    assert_eq!(classify_listen_address("localhost"), Unparseable);
    assert_eq!(classify_listen_address(""), Unparseable);
    assert_eq!(classify_listen_address("127.0.0.1:1883"), Unparseable);
}

/// An unparseable address must NOT fail the reconcile. `apply` covers the
/// whole `mqtt` subtree, so an `Err` raised over `listen` would fail the
/// reconcile of `mqtt.enabled` itself -- making the master switch depend on
/// `listen` being valid, which is exactly the coupling this reconciler
/// refuses. The broker is started, exits with its own parse error naming
/// the file and the value, and lands in `failed` where live state reports
/// it. Do not "fix" this into an `Err`; that introduces the coupling.
#[tokio::test]
async fn an_address_that_cannot_parse_still_starts_both_units() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, config) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings(true, "localhost", 1883, true))
        .await
        .expect("an unparseable listen address warns; it must never fail the reconcile");

    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable mica-mqtt-broker.service".to_string(),
            "start mica-mqtt-broker.service".to_string(),
            "enable mica-mqttd.service".to_string(),
            "start mica-mqttd.service".to_string(),
        ]
    );
    // Rendered verbatim: no default substituted, no value corrected.
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        "listen_address = \"localhost\"\nlisten_port = 1883\nauth_enabled = true\n"
    );
    assert_eq!(state["listen"]["address"], json!("localhost"));
}

/// The keys of a JSON object, sorted, for an exact-set assertion.
fn key_set(value: &serde_json::Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("published live state is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

/// The exact published shape, asserted key by key, because the consumer
/// is in another crate and cannot be seen from this file.
#[tokio::test]
async fn the_published_shape_is_the_contract_with_the_apid_pane() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _config) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings(true, "10.0.0.5", 8883, true))
        .await
        .unwrap();

    assert_eq!(
        key_set(&state),
        ["auth", "configPath", "enabled", "listen", "units"],
        "the pane reads these top-level keys by name"
    );
    assert_eq!(
        key_set(&state["listen"]),
        ["address", "port"],
        "the pane reads `listen.address` and `listen.port`"
    );
    assert_eq!(
        key_set(&state["auth"]),
        ["enabled"],
        "the pane reads `auth.enabled`; it is what the open-listener warning is gated on"
    );

    let units = state["units"]
        .as_array()
        .expect("`units` is an array -- the pane iterates it");
    assert_eq!(units.len(), 2);
    for unit in units {
        assert_eq!(
            key_set(unit),
            ["activeState", "unit", "unitFileState"],
            "the pane selects a unit by its `unit` field and shows its `activeState`"
        );
    }
    // Both names must be here. The pane selects by name and not by index,
    // so the ORDER is deliberately not part of this assertion -- but the
    // names are, and dropping one would leave that half of the switch
    // reading "unknown" on the page forever.
    let names: Vec<&str> = units
        .iter()
        .map(|unit| unit["unit"].as_str().expect("`unit` is a string"))
        .collect();
    assert!(names.contains(&BROKER_UNIT), "{names:?}");
    assert!(names.contains(&BRIDGE_UNIT), "{names:?}");
}

#[tokio::test]
async fn live_state_names_both_units_and_the_config_path() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, config) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler
        .apply(&settings(true, "127.0.0.1", 1883, false))
        .await
        .unwrap();

    assert_eq!(state["configPath"], json!(config.display().to_string()));
    assert_eq!(state["listen"]["address"], json!("127.0.0.1"));
    assert_eq!(state["listen"]["port"], json!(1883));
    assert_eq!(state["auth"]["enabled"], json!(false));
    // Read after the transition, so both report what they now are.
    assert_eq!(
        state["units"],
        json!([
            {
                "unit": "mica-mqtt-broker.service",
                "activeState": "active",
                "unitFileState": "enabled-runtime",
            },
            {
                "unit": "mica-mqttd.service",
                "activeState": "active",
                "unitFileState": "enabled-runtime",
            },
        ])
    );
}

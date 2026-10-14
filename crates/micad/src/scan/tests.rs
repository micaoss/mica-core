use super::{Conformance, Entry, Registry, as_i64, name_owner_changed_rule};
use zbus::zvariant::Value;

/// The rule the BUS is given, in the form it is given it: the filtering
/// happens on the daemon's side of the socket, not on ours, and the only
/// way to see that is the rule string `AddMatch` carries.
#[test]
fn the_bus_filters_on_the_namespace() {
    let rule = name_owner_changed_rule()
        .expect("a valid match rule")
        .to_string();
    assert!(
        rule.contains("arg0namespace='com.mica'"),
        "the bus must do the namespace filtering: {rule}"
    );
    assert!(rule.contains("member='NameOwnerChanged'"), "{rule}");
    assert!(rule.contains("interface='org.freedesktop.DBus'"), "{rule}");
    assert!(rule.contains("type='signal'"), "{rule}");
}

fn entry(class: Option<&str>, instance: i64, connected: bool) -> Entry {
    Entry {
        class: class.map(str::to_string),
        connected,
        instance,
        conformance: Conformance::default(),
    }
}

/// A conforming service carries an EMPTY conformance object — the shape an
/// operator reads as "nothing missing".
#[test]
fn a_clean_conformance_is_an_empty_object() {
    let clean = Conformance::default();
    assert!(clean.is_clean());
    assert_eq!(clean.to_json(), serde_json::json!({}));
}

/// Every gap has a name, and the names are the ones the bus contract uses.
#[test]
fn every_gap_names_itself() {
    let gaps = Conformance {
        no_item1: true,
        no_device_instance: true,
        missing_paths: vec!["/DeviceInstance", "/ProductId"],
    };
    assert!(!gaps.is_clean());
    assert_eq!(
        gaps.to_json(),
        serde_json::json!({
            "item1": false,
            "device_instance": false,
            "missing_paths": ["/DeviceInstance", "/ProductId"],
        })
    );
}

/// Two connected services of one class on one instance: BOTH marked, and
/// both still there.
#[test]
fn a_shared_instance_marks_both_sides() {
    let registry = Registry::new();
    registry.record("com.mica.sensor.one", entry(Some("sensor"), 0, true));
    let snapshot = registry.record("com.mica.sensor.two", entry(Some("sensor"), 0, true));

    assert_eq!(snapshot["com.mica.sensor.one"]["instance_collision"], true);
    assert_eq!(snapshot["com.mica.sensor.two"]["instance_collision"], true);
}

/// The neighbouring cases: a different class, a different instance, and a
/// service that is no longer connected, none of which collide.
#[test]
fn what_does_not_collide() {
    let registry = Registry::new();
    registry.record("com.mica.sensor.one", entry(Some("sensor"), 0, true));
    registry.record("com.mica.meter.two", entry(Some("meter"), 0, true));
    registry.record("com.mica.sensor.three", entry(Some("sensor"), 1, true));
    let snapshot = registry.record("com.mica.sensor.gone", entry(Some("sensor"), 0, false));

    for name in [
        "com.mica.sensor.one",
        "com.mica.meter.two",
        "com.mica.sensor.three",
        "com.mica.sensor.gone",
    ] {
        assert_eq!(
            snapshot[name]["instance_collision"], false,
            "{name} does not collide with anything"
        );
    }
}

/// Retention and removal: a vanished service stays, `ForgetService`'s
/// backing call drops it, and a connected one is refused.
#[test]
fn forget_takes_a_disconnected_entry_and_refuses_a_connected_one() {
    let registry = Registry::new();
    registry.record("com.mica.sensor.fake", entry(Some("sensor"), 3, true));

    let refused = registry
        .forget("com.mica.sensor.fake")
        .expect_err("a connected service must not be forgettable");
    assert!(
        refused.to_string().contains("still connected"),
        "the refusal must say why: {refused}"
    );

    let snapshot = registry
        .disconnect("com.mica.sensor.fake")
        .expect("the entry is there");
    assert_eq!(snapshot["com.mica.sensor.fake"]["connected"], false);

    let snapshot = registry
        .forget("com.mica.sensor.fake")
        .expect("a disconnected entry can be forgotten");
    assert_eq!(snapshot, serde_json::json!({}));

    assert!(
        registry.forget("com.mica.sensor.fake").is_err(),
        "forgetting what is already gone is an error, not a silent no-op"
    );
}

/// A name the registry never saw is not silently accepted.
#[test]
fn forgetting_an_unknown_name_is_refused() {
    let registry = Registry::new();
    let err = registry
        .forget("com.mica.sensor.never")
        .expect_err("unknown name");
    assert!(
        err.to_string().contains("no service registry entry"),
        "{err}"
    );
}

/// An instance is an integer whatever variant it arrived in, and is not
/// invented from something that is not one.
#[test]
fn an_instance_is_read_out_of_any_integer_variant() {
    assert_eq!(as_i64(&Value::from(7i32)), Some(7));
    assert_eq!(as_i64(&Value::from(7u32)), Some(7));
    assert_eq!(as_i64(&Value::from(7i64)), Some(7));
    assert_eq!(as_i64(&Value::from(7u8)), Some(7));
    assert_eq!(as_i64(&Value::from(7.0f64)), Some(7));
    // A variant inside a variant, which is what an item whose `value`
    // attribute was built by hand rather than by zvariant can arrive as.
    assert_eq!(as_i64(&Value::Value(Box::new(Value::from(7i32)))), Some(7));

    assert_eq!(as_i64(&Value::from(7.5f64)), None);
    assert_eq!(as_i64(&Value::from("7")), None);
    assert_eq!(as_i64(&Value::from(true)), None);
}

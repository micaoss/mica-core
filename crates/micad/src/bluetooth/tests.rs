use super::*;

mod agent;

#[tokio::test]
async fn a_board_with_no_adapter_refuses_rather_than_answering_empty() {
    let error = NoAdapter.devices().await.expect_err("no adapter");

    assert!(format!("{error}").contains("no Bluetooth adapter"));
    assert!(NoAdapter.pair("AA:BB:CC:DD:EE:FF").await.is_err());
    assert!(NoAdapter.set_powered(true).await.is_err());
}

#[test]
fn the_two_sides_are_named_apart_and_never_merged() {
    let mut declared = BTreeMap::new();
    declared.insert(
        "AA:BB:CC:DD:EE:01".to_string(),
        micad_settings::PairedDevice {
            name: "phone".to_string(),
            trusted: true,
            blocked: false,
        },
    );

    let value = observed_json(
        &declared,
        Ok(Adapter {
            address: "11:22:33:44:55:66".to_string(),
            alias: "mica".to_string(),
            powered: true,
            discoverable: false,
            discovering: true,
        }),
        Ok(vec![Device {
            address: "AA:BB:CC:DD:EE:02".to_string(),
            name: "headset".to_string(),
            rssi: Some(-60),
            ..Device::default()
        }]),
        "4211",
    );

    // Declared and in range are different lists: the phone is declared and
    // not in range, the headset is in range and not declared.
    assert_eq!(
        value["declared"]["AA:BB:CC:DD:EE:01"]["name"],
        json!("phone")
    );
    assert_eq!(
        value["devices"]["entries"][0]["address"],
        json!("AA:BB:CC:DD:EE:02")
    );
    assert_eq!(value["devices"]["entries"][0]["rssi"], json!(-60));
    assert_eq!(value["adapter"]["discovering"], json!(true));
    assert_eq!(value["pin"], json!("4211"));
}

#[test]
fn an_adapter_that_did_not_answer_is_absent_and_never_empty() {
    let value = observed_json(
        &BTreeMap::new(),
        Err("bluetoothd is not running".to_string()),
        Err("bluetoothd is not running".to_string()),
        "0000",
    );

    assert_eq!(value["adapter"]["available"], json!(false));
    assert_eq!(value["devices"]["available"], json!(false));
    assert!(value["adapter"]["detail"].is_string());
}

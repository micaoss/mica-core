use std::sync::Arc;

use super::*;
use crate::openrc::FakeCommands;

fn units(fake: &Arc<FakeCommands>) -> OpenrcUnits {
    OpenrcUnits::new(Arc::clone(fake) as Arc<dyn Commands>)
}

#[test]
fn a_service_unit_maps_onto_its_script_and_a_template_onto_a_multiplexed_one() {
    assert_eq!(service_name("mica-sshd.service").unwrap(), "mica-sshd");
    assert_eq!(
        service_name("hostapd@wlan0.service").unwrap(),
        "hostapd.wlan0"
    );
    assert!(service_name("mica.timer").is_err());
    assert!(service_name(".service").is_err());
    assert!(service_name("a b.service").is_err());
}

#[tokio::test]
async fn states_are_systemd_s_words_for_openrc_s_exit_codes() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer("rc-service mica-sshd status", 0, "");
    fake.answer("rc-service mica-mqttd status", 32, "");
    fake.answer("rc-service mica-mqtt-broker status", 3, "");
    let units = units(&fake);
    assert_eq!(
        units.active_state("mica-sshd.service").await.unwrap(),
        "active"
    );
    assert_eq!(
        units.active_state("mica-mqttd.service").await.unwrap(),
        "failed"
    );
    assert_eq!(
        units
            .active_state("mica-mqtt-broker.service")
            .await
            .unwrap(),
        "inactive"
    );
    assert_eq!(
        units.unit_file_state("mica-sshd.service").await.unwrap(),
        "static"
    );
}

#[tokio::test]
async fn verbs_run_rc_service_and_enablement_runs_nothing() {
    let fake = Arc::new(FakeCommands::default());
    let units = units(&fake);
    units.start("mica-sshd.service").await.unwrap();
    units.restart("mica-sshd.service").await.unwrap();
    units.stop("mica-sshd.service").await.unwrap();
    units.enable("mica-sshd.service").await.unwrap();
    units.disable("mica-sshd.service").await.unwrap();
    assert_eq!(
        fake.calls(),
        [
            "rc-service mica-sshd start",
            "rc-service mica-sshd restart",
            "rc-service mica-sshd stop"
        ]
    );
}

#[tokio::test]
async fn a_refused_verb_is_an_error() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer("rc-service mica-sshd start", 1, "");
    assert!(units(&fake).start("mica-sshd.service").await.is_err());
}

#[tokio::test]
async fn only_a_crashed_service_is_zapped() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer("rc-service mica-sshd status", 32, "");
    fake.answer("rc-service mica-mqttd status", 0, "");
    let units = units(&fake);
    units.reset_failed("mica-sshd.service").await.unwrap();
    units.reset_failed("mica-mqttd.service").await.unwrap();
    assert_eq!(
        fake.calls(),
        [
            "rc-service mica-sshd status",
            "rc-service mica-sshd zap",
            "rc-service mica-mqttd status"
        ]
    );
}

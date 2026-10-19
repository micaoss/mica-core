use super::*;
use crate::openrc::FakeCommands;

#[test]
fn only_allowlisted_sources_of_carried_features_are_readable() {
    let all = micad_settings::Features::all();
    assert_eq!(source("micad", &all).unwrap().unit, "micad.service");
    assert!(source("kernel", &all).is_err());
    assert!(source("../etc/shadow", &all).is_err());
    let bare = micad_settings::Features::only(&[]);
    assert!(source("apid", &bare).is_ok());
    let error = source("ssh", &bare).unwrap_err().to_string();
    assert!(error.contains("ssh"), "{error}");
}

#[tokio::test]
async fn systemd_reads_the_unit_s_journal_of_this_boot() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer(
        "journalctl -b -q --no-pager --no-hostname -o short-iso -u micad.service -n 200",
        0,
        "2026-09-28T10:00:00+00:00 micad[1]: serving\n",
    );
    let reader = HostLogs::new(Init::Systemd, Arc::clone(&fake) as Arc<dyn Commands>);
    let value = log_json(&reader, &LOG_SOURCES[0]).await;
    assert_eq!(value["available"], true);
    assert_eq!(value["source"], "micad");
    assert_eq!(
        value["lines"][0],
        "2026-09-28T10:00:00+00:00 micad[1]: serving"
    );
}

#[tokio::test]
async fn openrc_reads_the_tag_s_lines_without_the_host() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer(
        "logread",
        0,
        "Sep 28 10:00:00 site daemon.info micad[10]: serving\n\
         Sep 28 10:00:01 site daemon.info mica-apid[11]: listening\n\
         Sep 28 10:00:02 site daemon.warn micad: a warning\n",
    );
    let reader = HostLogs::new(Init::Openrc, Arc::clone(&fake) as Arc<dyn Commands>);
    let value = log_json(&reader, &LOG_SOURCES[0]).await;
    assert_eq!(
        value["lines"],
        json!([
            "Sep 28 10:00:00 daemon.info micad[10]: serving",
            "Sep 28 10:00:02 daemon.warn micad: a warning"
        ])
    );
    assert!(!value.to_string().contains("site"));
}

#[tokio::test]
async fn a_failed_read_is_absent_with_the_reason() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer("logread", 1, "");
    let reader = HostLogs::new(Init::Openrc, Arc::clone(&fake) as Arc<dyn Commands>);
    let value = log_json(&reader, &LOG_SOURCES[1]).await;
    assert_eq!(value["available"], false);
    assert_eq!(value["source"], "apid");
}

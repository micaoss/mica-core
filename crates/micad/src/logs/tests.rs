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
        "logread -b 0",
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
    fake.answer("logread -b 0", 1, "");
    fake.answer("logread", 1, "");
    let reader = HostLogs::new(Init::Openrc, Arc::clone(&fake) as Arc<dyn Commands>);
    let value = log_json(&reader, &LOG_SOURCES[1]).await;
    assert_eq!(value["available"], false);
    assert_eq!(value["source"], "apid");
}

/// Either SSH server logs as `mica-sshd`: the unit on systemd, the syslog tag
/// on OpenRC.
#[tokio::test]
async fn the_ssh_log_is_mica_sshd_whichever_server_runs() {
    let ssh = source("ssh", &micad_settings::Features::all()).unwrap();
    assert_eq!(ssh.unit, "mica-sshd.service");
    assert_eq!(ssh.tag, "mica-sshd");

    let fake = Arc::new(FakeCommands::default());
    fake.answer(
        "logread -b 0",
        0,
        "Sep 28 10:00:00 site auth.info mica-sshd[20]: Accepted publickey for root\n\
         Sep 28 10:00:01 site auth.info dropbear[21]: a line of another tag\n",
    );
    let reader = HostLogs::new(Init::Openrc, Arc::clone(&fake) as Arc<dyn Commands>);
    let value = log_json(&reader, ssh).await;
    assert_eq!(
        value["lines"],
        json!(["Sep 28 10:00:00 auth.info mica-sshd[20]: Accepted publickey for root"])
    );
}

#[test]
fn a_boot_is_current_or_previous() {
    assert_eq!(Boot::parse("current").unwrap(), Boot::Current);
    assert_eq!(Boot::parse("previous").unwrap(), Boot::Previous);
    assert!(Boot::parse("-1").is_err());
    assert!(Boot::parse("").is_err());
}

#[tokio::test]
async fn systemd_reads_the_previous_boot_only_when_the_journal_lists_one() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer(
        "journalctl --list-boots -q --no-pager",
        0,
        "IDX BOOT ID                          FIRST ENTRY LAST ENTRY\n\
         -1 0f1e2d3c4b5a69788796a5b4c3d2e1f0 Mon 2026-11-09 08:00:00 UTC Mon 2026-11-09 09:00:00 UTC\n\
          0 1f1e2d3c4b5a69788796a5b4c3d2e1f0 Mon 2026-11-09 09:01:00 UTC Mon 2026-11-09 10:00:00 UTC\n",
    );
    fake.answer(
        "journalctl -b -1 -q --no-pager --no-hostname -o short-iso -u micad.service -n 200",
        0,
        "2026-11-09T08:59:00+00:00 micad[1]: stopping\n",
    );
    let reader = HostLogs::new(Init::Systemd, Arc::clone(&fake) as Arc<dyn Commands>);
    let value = log_of_boot_json(&reader, &LOG_SOURCES[0], Boot::Previous).await;
    assert_eq!(value["available"], true);
    assert_eq!(
        value["lines"],
        json!(["2026-11-09T08:59:00+00:00 micad[1]: stopping"])
    );

    let volatile = Arc::new(FakeCommands::default());
    volatile.answer(
        "journalctl --list-boots -q --no-pager",
        0,
        "0 1f1e2d3c4b5a69788796a5b4c3d2e1f0 Mon 2026-11-09 09:01:00 UTC Mon 2026-11-09 10:00:00 UTC\n",
    );
    let reader = HostLogs::new(Init::Systemd, Arc::clone(&volatile) as Arc<dyn Commands>);
    let value = log_of_boot_json(&reader, &LOG_SOURCES[0], Boot::Previous).await;
    assert_eq!(value["available"], false);
    assert_eq!(value["source"], "micad");
    assert_eq!(value["detail"], NoPreviousBoot.to_string());
    assert!(
        volatile.calls().iter().all(|call| !call.contains("-b -1")),
        "{:?}",
        volatile.calls()
    );
}

#[tokio::test]
async fn openrc_reads_the_previous_boot_from_base_s_markers() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer(
        "logread -b -1",
        0,
        "Nov  9 08:00:00 site syslog.info mica-syslogd: mica-boot: boot\n\
         Nov  9 08:59:00 site daemon.info micad[10]: stopping\n",
    );
    let reader = HostLogs::new(Init::Openrc, Arc::clone(&fake) as Arc<dyn Commands>);
    let value = log_of_boot_json(&reader, &LOG_SOURCES[0], Boot::Previous).await;
    assert_eq!(
        value["lines"],
        json!(["Nov  9 08:59:00 daemon.info micad[10]: stopping"])
    );

    let ram = Arc::new(FakeCommands::default());
    ram.answer("logread -b -1", 0, "");
    let reader = HostLogs::new(Init::Openrc, Arc::clone(&ram) as Arc<dyn Commands>);
    let value = log_of_boot_json(&reader, &LOG_SOURCES[0], Boot::Previous).await;
    assert_eq!(value["available"], false);
    assert_eq!(value["detail"], NoPreviousBoot.to_string());
}

/// busybox's own `logread`, on a Base without boot markers, refuses `-b`:
/// this boot is then its whole RAM log.
#[tokio::test]
async fn an_older_base_s_logread_is_read_whole_for_this_boot() {
    let fake = Arc::new(FakeCommands::default());
    fake.answer("logread -b 0", 1, "");
    fake.answer(
        "logread",
        0,
        "Sep 28 10:00:00 site daemon.info micad[10]: serving\n",
    );
    let reader = HostLogs::new(Init::Openrc, Arc::clone(&fake) as Arc<dyn Commands>);
    let value = log_json(&reader, &LOG_SOURCES[0]).await;
    assert_eq!(
        value["lines"],
        json!(["Sep 28 10:00:00 daemon.info micad[10]: serving"])
    );
    assert_eq!(fake.calls(), ["logread -b 0", "logread"]);
}

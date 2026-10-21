//! Channel 0 under ACS, and the fallback when hostapd cannot choose.

use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

use super::*;

/// [`lab_ap`] on channel 0.
fn automatic() -> Settings {
    settings_with(WifiApSettings {
        channel: 0,
        ..lab_ap()
    })
}

/// A hostapd control socket for `wlan0` under `dir` answering every
/// `STATUS` with `reply`, until the test ends.
fn fake_hostapd(dir: &Path, reply: &'static str) {
    std::fs::create_dir_all(dir).unwrap();
    let server = UnixDatagram::bind(dir.join("wlan0")).unwrap();
    std::thread::spawn(move || {
        let mut buffer = [0u8; 64];
        while let Ok((_, peer)) = server.recv_from(&mut buffer) {
            if let Some(path) = peer.as_pathname() {
                let _ = server.send_to(reply.as_bytes(), path);
            }
        }
    });
}

#[tokio::test]
pub(super) async fn a_channel_hostapd_chose_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    let control = dir.path().join("ctl");
    fake_hostapd(&control, "state=ENABLED\nchannel=6\n");
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let reconciler = reconciler.with_acs(true).with_acs_watch(
        control,
        Duration::from_secs(5),
        Duration::from_millis(10),
    );

    let state = reconciler.apply(&automatic()).await.unwrap();

    let config = std::fs::read_to_string(&paths.config).unwrap();
    assert!(config.contains("channel=acs_survey\n"), "{config}");
    assert_eq!(state["acsFallback"], serde_json::Value::Null);
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable hostapd@wlan0.service",
            "start hostapd@wlan0.service"
        ]
    );
}

/// A hostapd that never reports a channel -- it exits on the failure and is
/// restarted into the same survey -- is restarted on the country's first
/// channel, and stays there without another wait.
#[tokio::test]
pub(super) async fn a_selection_that_never_finishes_falls_back_to_the_first_channel() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let reconciler = reconciler.with_acs(true).with_acs_watch(
        dir.path().join("ctl"),
        Duration::from_millis(50),
        Duration::from_millis(10),
    );

    let state = reconciler.apply(&automatic()).await.unwrap();

    let config = std::fs::read_to_string(&paths.config).unwrap();
    assert!(config.contains("channel=1\n"), "{config}");
    assert_eq!(state["acsFallback"]["channel"], json!(1));
    let detail = state["acsFallback"]["detail"].as_str().unwrap();
    assert!(detail.contains("did not choose a channel"), "{detail}");
    assert_eq!(
        reconciler.control.calls(),
        vec![
            "enable hostapd@wlan0.service",
            "start hostapd@wlan0.service",
            "restart hostapd@wlan0.service",
        ]
    );

    let again = reconciler.apply(&automatic()).await.unwrap();
    assert_eq!(again["accessPoint"], json!("unchanged"));
    assert_eq!(again["acsFallback"]["channel"], json!(1));
    assert_eq!(reconciler.control.calls().len(), 3);

    // An explicit channel reports no fallback.
    let fixed = reconciler.apply(&settings_with(lab_ap())).await.unwrap();
    assert_eq!(fixed["acsFallback"], serde_json::Value::Null);
}

/// `state=DISABLED` is hostapd giving up: no need to wait out the rest.
#[tokio::test]
pub(super) async fn hostapd_giving_up_falls_back_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let control = dir.path().join("ctl");
    fake_hostapd(&control, "state=DISABLED\n");
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let reconciler = reconciler.with_acs(true).with_acs_watch(
        control,
        Duration::from_secs(30),
        Duration::from_millis(10),
    );

    let started = Instant::now();
    let state = reconciler.apply(&automatic()).await.unwrap();

    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains("channel=1\n")
    );
    assert_eq!(
        state["acsFallback"]["detail"],
        json!("hostapd gave up choosing a channel (state DISABLED)")
    );
}

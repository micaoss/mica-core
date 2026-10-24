//! The engine against a phone that speaks protocol 1, with a fake device,
//! radio and clock.

use std::num::NonZeroU32;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use aws_lc_rs::{agreement, digest};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use micad_settings::{WebMode, WebSettings};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::super::channel::{self, Reassembly, Records};
use super::super::engine::*;
use super::super::spake2plus::{self, Transcript, prover, scalar};
use super::super::store::{Lockout, Store, make_record};
use super::super::*;

const PHONE: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_01";
const OTHER: &str = "/org/bluez/hci0/dev_AA_BB_CC_DD_EE_02";
const PASSWORD: &str = "harbour-pw-1";

#[derive(Default)]
struct FakeDevice {
    facts: Mutex<Facts>,
    calls: Mutex<Vec<String>>,
    /// `await_wifi`'s answer; `None` waits until released.
    wifi: Mutex<Option<Result<String, &'static str>>>,
    release: tokio::sync::Notify,
    /// `scan`'s answer; `ap_active` when unset.
    scanned: Mutex<Option<Scanned>>,
    /// The `rescan` of each scan asked for.
    rescans: Mutex<Vec<bool>>,
    /// `await_network`'s failure, if it fails.
    network: Mutex<Option<&'static str>>,
    /// How many connections began a scan.
    warmed: AtomicU64,
}

#[async_trait::async_trait]
impl Device for FakeDevice {
    async fn facts(&self) -> Facts {
        self.facts.lock().unwrap().clone()
    }
    async fn status(&self) -> Value {
        let joined = self
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.starts_with("join"));
        let mode = if joined { "client" } else { "ap" };
        let mut status = json!({ "wifiMode": mode, "connected": false, "ssid": null, "ip": null, "interfaces": [
            { "name": "eth0", "kind": "ethernet", "carrier": true, "address": "10.0.0.5/24" },
            { "name": "wlan0", "kind": "wifi", "carrier": false, "address": "192.168.1.40/24" },
        ] });
        if !joined {
            status["ap"] = json!({ "on": true, "ssid": "mica-789Z", "psk": "U8A5TU7RT6A6E8TV" });
        }
        status
    }
    async fn scan(&self, rescan: bool) -> Result<Scanned, &'static str> {
        self.rescans.lock().unwrap().push(rescan);
        self.scanned.lock().unwrap().clone().ok_or("ap_active")
    }
    async fn wifi_snapshot(&self) -> Value {
        let joined = self
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call.starts_with("join"));
        json!({ "mode": if joined { "client" } else { "ap" } })
    }
    async fn join_wifi(&self, ssid: &str, _psk: &str, _hidden: bool) -> Result<bool, &'static str> {
        self.calls.lock().unwrap().push(format!("join {ssid}"));
        Ok(true)
    }
    async fn await_wifi(&self, _ssid: &str) -> Result<String, &'static str> {
        loop {
            let answer = self.wifi.lock().unwrap().clone();
            if let Some(answer) = answer {
                return answer;
            }
            self.release.notified().await;
        }
    }
    async fn set_network(&self, interface: &str, entry: &Value) -> Result<Value, &'static str> {
        if interface != "eth0" {
            return Err("unknown_interface");
        }
        self.calls
            .lock()
            .unwrap()
            .push(format!("network {interface} {entry}"));
        Ok(json!({ "dhcp": true }))
    }
    async fn await_network(
        &self,
        _interface: &str,
        entry: &Value,
    ) -> Result<Option<String>, &'static str> {
        if let Some(reason) = *self.network.lock().unwrap() {
            return Err(reason);
        }
        let address = entry["static"]["address"]
            .as_str()
            .unwrap_or("192.168.1.41/24");
        Ok(Some(address.to_string()))
    }
    async fn restore_network(&self, previous: Vec<(String, Value)>) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("restore network {previous:?}"));
    }
    async fn warm(&self) {
        self.warmed.fetch_add(1, Ordering::SeqCst);
    }
    async fn restore_wifi(&self, snapshot: Value, remove: Vec<String>) {
        self.calls
            .lock()
            .unwrap()
            .push(format!("restore {snapshot} remove {remove:?}"));
    }
}

#[derive(Default)]
struct FakeRadio {
    notified: Mutex<Vec<(Notify, Vec<u8>)>>,
    advertised: Mutex<Vec<Option<String>>>,
    disconnected: Mutex<Vec<String>>,
    held: AtomicBool,
}

#[async_trait::async_trait]
impl Radio for FakeRadio {
    async fn advertise(&self, name: Option<&str>) {
        self.advertised
            .lock()
            .unwrap()
            .push(name.map(str::to_string));
    }
    async fn hold(&self, on: bool) {
        self.held.store(on, Ordering::SeqCst);
    }
    async fn notify(&self, characteristic: Notify, fragment: Vec<u8>) {
        self.notified
            .lock()
            .unwrap()
            .push((characteristic, fragment));
    }
    async fn disconnect(&self, device: &str) {
        self.disconnected.lock().unwrap().push(device.to_string());
    }
}

#[derive(Default)]
struct FakeClock {
    uptime: AtomicU64,
    unix: AtomicU64,
}

impl Clock for FakeClock {
    fn uptime(&self) -> u64 {
        self.uptime.load(Ordering::SeqCst)
    }
    fn unix(&self) -> u64 {
        self.unix.load(Ordering::SeqCst)
    }
}

/// The phone's half of a session.
#[derive(Default)]
struct Phone {
    session_in: Reassembly,
    event_in: Reassembly,
    to_device: Option<Records>,
    from_device: Option<Records>,
    next_id: u64,
}

struct Bench {
    engine: Engine,
    rx: mpsc::UnboundedReceiver<Input>,
    device: Arc<FakeDevice>,
    radio: Arc<FakeRadio>,
    clock: Arc<FakeClock>,
    store: Store,
    audit: Arc<Mutex<Vec<String>>>,
    wifi_locked: Arc<AtomicBool>,
    phone: Phone,
    /// The device's last `share`.
    share: Value,
    _dir: tempfile::TempDir,
}

impl Bench {
    async fn new(claimed: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::new(dir.path());
        let facts = Facts {
            claimed,
            hostname: "mica-a4e2e69e".into(),
            model: "mica cx3576".into(),
            auto_when_offline: true,
            has_wifi: true,
            web: WebSettings::default(),
        };
        let device = Arc::new(FakeDevice::default());
        *device.facts.lock().unwrap() = facts.clone();
        let radio = Arc::new(FakeRadio::default());
        let clock = Arc::new(FakeClock::default());
        clock.uptime.store(5, Ordering::SeqCst);
        clock.unix.store(1_800_000_000, Ordering::SeqCst);
        let audit = Arc::new(Mutex::new(Vec::new()));
        let lines = Arc::clone(&audit);
        let wifi_locked = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::unbounded_channel();
        let parts = Parts {
            device: device.clone(),
            radio: radio.clone(),
            clock: clock.clone(),
            store: store.clone(),
            audit: Arc::new(move |event: &str, outcome: &str| {
                lines.lock().unwrap().push(format!("{event} {outcome}"));
            }),
            wifi_locked: Arc::clone(&wifi_locked),
        };
        let mut bench = Self {
            engine: Engine::new(parts, facts, tx),
            rx,
            device,
            radio,
            clock,
            store,
            audit,
            wifi_locked,
            phone: Phone::default(),
            share: Value::Null,
            _dir: dir,
        };
        bench.engine.handle(Input::Tick).await;
        bench
    }

    fn advance(&self, seconds: u64) {
        self.clock.uptime.fetch_add(seconds, Ordering::SeqCst);
        self.clock.unix.fetch_add(seconds, Ordering::SeqCst);
    }

    async fn tick(&mut self, seconds: u64) {
        self.advance(seconds);
        self.engine.handle(Input::Tick).await;
    }

    /// Let spawned operations finish and feed back what they sent.
    async fn pump(&mut self) {
        for _ in 0..20 {
            tokio::task::yield_now().await;
            while let Ok(input) = self.rx.try_recv() {
                self.engine.handle(input).await;
            }
        }
    }

    async fn read_info(&mut self, phone: &str) -> Result<Value, Refused> {
        let (reply, answer) = oneshot::channel();
        self.engine
            .handle(Input::Read {
                device: phone.into(),
                mtu: Some(185),
                reply,
            })
            .await;
        answer
            .await
            .unwrap()
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
    }

    async fn write(
        &mut self,
        phone: &str,
        characteristic: Written,
        message: &[u8],
    ) -> Result<(), Refused> {
        for fragment in channel::fragments(message, 182) {
            let (reply, answer) = oneshot::channel();
            self.engine
                .handle(Input::Write {
                    device: phone.into(),
                    mtu: Some(185),
                    characteristic,
                    value: fragment,
                    reply,
                })
                .await;
            answer.await.unwrap()?;
        }
        Ok(())
    }

    /// Every message notified since the last call, on `characteristic`,
    /// joined.
    fn take(&mut self, characteristic: Notify) -> Vec<Vec<u8>> {
        let notified: Vec<_> = std::mem::take(&mut *self.radio.notified.lock().unwrap());
        let mut messages = Vec::new();
        for (on, fragment) in notified {
            assert!(fragment.len() <= 182, "a fragment fits the MTU");
            let joiner = match on {
                Notify::Session => &mut self.phone.session_in,
                Notify::Event => &mut self.phone.event_in,
            };
            if let Some(message) = joiner.push(&fragment).unwrap()
                && on == characteristic
            {
                messages.push(message);
            }
        }
        messages
    }

    async fn session(&mut self, message: Value) -> Value {
        self.write(PHONE, Written::Session, message.to_string().as_bytes())
            .await
            .unwrap();
        let mut replies = self.take(Notify::Session);
        assert_eq!(replies.len(), 1, "one answer");
        serde_json::from_slice(&replies.remove(0)).unwrap()
    }

    /// Send `command` and answer the events it produced.
    async fn command(&mut self, mut command: Value) -> Vec<Value> {
        self.phone.next_id += 1;
        command["id"] = json!(self.phone.next_id);
        let record = self
            .phone
            .to_device
            .as_mut()
            .unwrap()
            .seal(command.to_string().as_bytes())
            .unwrap();
        self.write(PHONE, Written::Command, &record).await.unwrap();
        self.pump().await;
        self.events()
    }

    fn events(&mut self) -> Vec<Value> {
        let records = self.take(Notify::Event);
        records
            .iter()
            .map(|record| {
                let plaintext = self
                    .phone
                    .from_device
                    .as_mut()
                    .unwrap()
                    .open(record)
                    .unwrap();
                serde_json::from_slice(&plaintext).unwrap()
            })
            .collect()
    }

    /// Authenticate with `none`.
    async fn start_none(&mut self) {
        self.start_none_with(json!({})).await;
    }

    /// Authenticate with `none`, with `extra` members in `start`.
    async fn start_none_with(&mut self, extra: Value) {
        let key = agreement::PrivateKey::generate(&agreement::X25519).unwrap();
        let public = key.compute_public_key().unwrap();
        let mut start =
            json!({ "t": "start", "auth": "none", "pub": BASE64.encode(public.as_ref()) });
        for (name, value) in extra.as_object().unwrap() {
            start[name] = value.clone();
        }
        let ready = self.session(start).await;
        assert_eq!(ready.get("status"), extra.get("status"), "{ready}");
        assert_eq!(ready["t"], "ready", "{ready}");
        let device = BASE64.decode(ready["pub"].as_str().unwrap()).unwrap();
        let keys = agreement::agree(
            &key,
            agreement::UnparsedPublicKey::new(&agreement::X25519, &device),
            (),
            |secret| {
                let mut salt = b"mica-provisioning/1 none".to_vec();
                salt.extend_from_slice(public.as_ref());
                salt.extend_from_slice(&device);
                let salt = digest::digest(&digest::SHA256, &salt);
                Ok(channel::session_keys(salt.as_ref(), secret).unwrap())
            },
        )
        .unwrap();
        self.keys(&keys);
    }

    fn keys(&mut self, keys: &channel::SessionKeys) {
        self.phone.to_device = Some(Records::new(&keys.phone_to_device).unwrap());
        self.phone.from_device = Some(Records::new(&keys.device_to_phone).unwrap());
    }

    /// Run SPAKE2+ with `pw` up to the device's share; the phone's keys.
    async fn exchange(&mut self, auth: &str, pw: &str) -> Result<spake2plus::Keys, Value> {
        let params = self.session(json!({ "t": "start", "auth": auth })).await;
        if params["t"] != "params" {
            return Err(params);
        }
        let salt = BASE64.decode(params["salt"].as_str().unwrap()).unwrap();
        let iterations = u32::try_from(params["iterations"].as_u64().unwrap()).unwrap();
        let (w0, w1) = spake2plus::derive(
            pw.as_bytes(),
            &salt,
            NonZeroU32::new(iterations).unwrap(),
            ID_PROVER,
            ID_VERIFIER,
        );
        let x = scalar(&[0x42; 32]);
        let share = self
            .session(json!({ "t": "share", "shareP": BASE64.encode(prover::share(&w0, &x)) }))
            .await;
        let share_v = BASE64.decode(share["shareV"].as_str().unwrap()).unwrap();
        self.share = share.clone();
        let t = Transcript {
            context: if auth == "temp-code" {
                CONTEXT_TEMP_CODE
            } else {
                CONTEXT_PASSWORD
            },
            id_prover: ID_PROVER,
            id_verifier: ID_VERIFIER,
        };
        let keys = prover::finish(&t, &w0, &w1, &x, &share_v).unwrap();
        let confirm_v = BASE64.decode(share["confirmV"].as_str().unwrap()).unwrap();
        Ok(spake2plus::Keys {
            confirm_v: confirm_v.try_into().unwrap(),
            ..keys
        })
    }

    /// Authenticate with SPAKE2+; the device's last session message.
    async fn authenticate(&mut self, auth: &str, pw: &str) -> Value {
        let keys = match self.exchange(auth, pw).await {
            Ok(keys) => keys,
            Err(refusal) => return refusal,
        };
        let reply = self
            .session(json!({ "t": "confirm", "confirmP": BASE64.encode(keys.confirm_p) }))
            .await;
        if reply["t"] == "ready" {
            self.keys(&channel::session_keys(&[], &keys.shared).unwrap());
        }
        reply
    }

    fn set_verifier(&self, pw: &str) {
        self.store
            .set_verifier(&make_record(pw.as_bytes(), 1000).unwrap())
            .unwrap();
    }

    fn calls(&self) -> Vec<String> {
        self.device.calls.lock().unwrap().clone()
    }

    fn last_advert(&self) -> Option<String> {
        self.radio
            .advertised
            .lock()
            .unwrap()
            .last()
            .cloned()
            .flatten()
    }
}

#[tokio::test]
async fn an_unclaimed_device_joins_wi_fi_and_stays_unclaimed() {
    let mut bench = Bench::new(false).await;
    assert_eq!(bench.last_advert().as_deref(), Some("mica-a4e2e69e"));
    assert!(bench.radio.held.load(Ordering::SeqCst));

    let info = bench.read_info(PHONE).await.unwrap();
    assert_eq!(info["protocol"], 1);
    assert_eq!(info["claimed"], false);
    assert_eq!(info["auth"], "none");
    assert_eq!(info["maxWrite"], 182);
    assert_eq!(bench.last_advert(), None, "one phone at a time");

    bench.start_none().await;
    assert!(bench.wifi_locked.load(Ordering::SeqCst));

    *bench.device.wifi.lock().unwrap() = Some(Ok("192.168.1.40".into()));
    let events = bench
        .command(
            json!({ "op": "setWifi", "ssid": "Harbour", "psk": "sea-breeze-77", "hidden": false }),
        )
        .await;
    assert_eq!(events[0], json!({ "id": 1, "event": "connecting" }));
    assert_eq!(
        events[1],
        json!({ "id": 1, "event": "connected", "ip": "192.168.1.40", "hostname": "mica-a4e2e69e" })
    );

    let finished = bench.command(json!({ "op": "finish" })).await;
    assert_eq!(
        finished,
        vec![json!({
            "id": 2,
            "event": "finished",
            "claimed": false,
            "hostname": "mica-a4e2e69e",
            "interface": { "name": "wlan0", "kind": "wifi", "address": "192.168.1.40/24" },
            "consoleUrls": ["http://192.168.1.40:8080", "http://mica-a4e2e69e.local:8080"],
            "ap": { "on": false },
        })]
    );
    assert_eq!(bench.calls(), vec!["join Harbour"]);
    assert_eq!(*bench.radio.disconnected.lock().unwrap(), vec![PHONE]);
    assert!(!bench.wifi_locked.load(Ordering::SeqCst));
    bench.tick(1).await;
    assert_eq!(bench.last_advert(), None, "finish closes the window");
    let history = bench.store.history();
    assert_eq!(history.len(), 1);
    assert_eq!(
        (history[0].trigger.as_str(), history[0].outcome.as_str()),
        ("unclaimed", "finished")
    );
}

#[tokio::test]
async fn a_start_that_asks_for_the_status_gets_it_after_ready() {
    let mut bench = Bench::new(false).await;
    bench.read_info(PHONE).await.unwrap();
    bench.pump().await;
    assert_eq!(
        bench.device.warmed.load(Ordering::SeqCst),
        1,
        "a scan begins at the connection"
    );
    bench.start_none_with(json!({ "status": true })).await;
    bench.pump().await;
    let pushed = bench.events();
    assert_eq!(pushed.len(), 1);
    assert_eq!(pushed[0]["event"], "status");
    assert_eq!(pushed[0]["wifiMode"], "ap");
    assert_eq!(
        pushed[0]["ap"]["ssid"], "mica-789Z",
        "the pushed status carries the access point"
    );
    assert!(pushed[0].get("id").is_none());
    let status = bench.command(json!({ "op": "getStatus" })).await;
    assert_eq!(status[0]["id"], 1, "the commands' records follow it");

    let mut quiet = Bench::new(false).await;
    quiet.start_none().await;
    quiet.pump().await;
    assert!(quiet.events().is_empty(), "nothing unasked");
}

#[tokio::test]
async fn an_unclaimed_device_is_set_up_over_its_wired_port() {
    let mut bench = Bench::new(false).await;
    let info = bench.read_info(PHONE).await.unwrap();
    assert!(
        info["ops"]
            .as_array()
            .unwrap()
            .contains(&json!("setNetwork"))
    );
    bench.start_none().await;
    for malformed in [
        json!({ "op": "setNetwork", "dhcp": true }),
        json!({ "op": "setNetwork", "interface": "eth0" }),
        json!({ "op": "setNetwork", "interface": "eth0", "dhcp": true, "static": {} }),
    ] {
        let refused = bench.command(malformed.clone()).await;
        assert_eq!(refused[0]["reason"], "bad_request", "{malformed}");
    }
    let refused = bench
        .command(json!({ "op": "setNetwork", "interface": "wlan0", "dhcp": true }))
        .await;
    assert_eq!(
        refused,
        vec![
            json!({ "id": 4, "event": "applying" }),
            json!({ "id": 4, "event": "failed", "reason": "unknown_interface" }),
        ]
    );

    let set = bench
        .command(json!({
            "op": "setNetwork",
            "interface": "eth0",
            "static": { "address": "10.0.0.5/24", "gateway": "10.0.0.1", "dns": ["10.0.0.1"] },
        }))
        .await;
    assert_eq!(
        set,
        vec![
            json!({ "id": 5, "event": "applying" }),
            json!({ "id": 5, "event": "up", "interface": "eth0", "address": "10.0.0.5/24" }),
        ]
    );
    let finished = bench.command(json!({ "op": "finish" })).await;
    assert_eq!(finished[0]["event"], "finished");
    assert_eq!(finished[0]["claimed"], false);
    assert_eq!(
        finished[0]["ap"],
        json!({ "on": true, "ssid": "mica-789Z", "psk": "U8A5TU7RT6A6E8TV" }),
        "a wired session leaves the access point on, and says how to join it"
    );
    assert_eq!(
        finished[0]["interface"],
        json!({ "name": "eth0", "kind": "ethernet", "address": "10.0.0.5/24" })
    );
    assert_eq!(
        finished[0]["consoleUrls"],
        json!(["http://10.0.0.5:8080", "http://mica-a4e2e69e.local:8080"])
    );
    bench.pump().await;
    assert_eq!(
        bench.calls(),
        vec![
            r#"network eth0 {"static":{"address":"10.0.0.5/24","dns":["10.0.0.1"],"gateway":"10.0.0.1"}}"#,
        ],
        "a finished session keeps its wired entry and touches no Wi-Fi"
    );
}

#[tokio::test]
async fn an_unfinished_unclaimed_session_puts_back_a_wired_entry_not_up() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    *bench.device.network.lock().unwrap() = Some("dhcp_timeout");
    let events = bench
        .command(json!({ "op": "setNetwork", "interface": "eth0", "dhcp": true }))
        .await;
    assert_eq!(events[1]["reason"], "dhcp_timeout");
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    bench.tick(RECONNECT_SECONDS).await;
    bench.pump().await;
    assert_eq!(
        bench.calls().last().map(String::as_str),
        Some(r#"restore network [("eth0", Object {"dhcp": Bool(true)})]"#)
    );
}

#[tokio::test]
async fn a_device_claimed_meanwhile_takes_no_network_over_a_none_session() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    bench.device.facts.lock().unwrap().claimed = true;
    for command in [
        json!({ "op": "setWifi", "ssid": "Harbour", "psk": "", "hidden": false }),
        json!({ "op": "setNetwork", "interface": "eth0", "dhcp": true }),
        json!({ "op": "finish" }),
    ] {
        let refused = bench.command(command.clone()).await;
        assert_eq!(refused[0]["reason"], "already_claimed", "{command}");
    }
    assert!(bench.calls().is_empty());
}

#[tokio::test]
async fn finish_ignores_members_it_does_not_know_and_sets_nothing() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    let finished = bench
        .command(json!({ "op": "finish", "hostname": "deck-7" }))
        .await;
    assert_eq!(finished[0]["event"], "finished", "{finished:?}");
    assert_eq!(finished[0]["hostname"], "mica-a4e2e69e");
    assert!(bench.calls().is_empty(), "finish sets nothing");
}

#[tokio::test]
async fn ops_beyond_the_network_are_unknown() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    for command in [
        json!({ "op": "claim", "password": PASSWORD }),
        json!({ "op": "reboot" }),
    ] {
        let events = bench.command(command).await;
        assert_eq!(events[0]["reason"], "unknown_op", "{events:?}");
    }
}

#[tokio::test]
async fn a_wired_entry_that_came_up_stays_when_the_session_ends_without_finish() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    let events = bench
        .command(json!({ "op": "setNetwork", "interface": "eth0", "dhcp": true }))
        .await;
    assert_eq!(events[1]["event"], "up");
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    bench.tick(RECONNECT_SECONDS).await;
    bench.pump().await;
    assert_eq!(bench.calls(), vec![r#"network eth0 {"dhcp":true}"#]);
}

#[tokio::test]
async fn a_wi_fi_that_connected_stays_when_the_session_ends_without_finish() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    *bench.device.wifi.lock().unwrap() = Some(Ok("192.168.1.40".into()));
    let events = bench
        .command(
            json!({ "op": "setWifi", "ssid": "Harbour", "psk": "sea-breeze-77", "hidden": false }),
        )
        .await;
    assert_eq!(events[1]["event"], "connected");
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    bench.tick(RECONNECT_SECONDS).await;
    bench.pump().await;
    assert_eq!(
        bench.calls(),
        vec!["join Harbour"],
        "the network stays saved and the access point stays off"
    );
    assert!(!bench.wifi_locked.load(Ordering::SeqCst));
    assert_eq!(bench.store.history()[0].outcome, "wifi-connected");
}

#[tokio::test]
async fn a_wi_fi_still_connecting_when_the_phone_leaves_is_undone_after_two_minutes() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    let events = bench
        .command(
            json!({ "op": "setWifi", "ssid": "Harbour", "psk": "sea-breeze-77", "hidden": false }),
        )
        .await;
    assert_eq!(events, vec![json!({ "id": 1, "event": "connecting" })]);
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    assert!(
        bench.wifi_locked.load(Ordering::SeqCst),
        "held while the phone may return"
    );
    bench.tick(119).await;
    bench.pump().await;
    assert_eq!(bench.calls(), vec!["join Harbour"]);
    bench.tick(1).await;
    bench.pump().await;
    assert_eq!(
        bench.calls().last().unwrap(),
        r#"restore {"mode":"ap"} remove ["Harbour"]"#
    );
    assert!(!bench.wifi_locked.load(Ordering::SeqCst));
    assert_eq!(bench.store.history()[0].outcome, "abandoned");
}

/// The phone joins the device to Wi-Fi and leaves without `finish`; the
/// user claims the device on the console meanwhile.
#[tokio::test]
async fn a_claim_on_the_console_keeps_the_wi_fi_the_session_connected() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    *bench.device.wifi.lock().unwrap() = Some(Ok("192.168.1.40".into()));
    bench
        .command(
            json!({ "op": "setWifi", "ssid": "Harbour", "psk": "sea-breeze-77", "hidden": false }),
        )
        .await;
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    bench.device.facts.lock().unwrap().claimed = true;
    bench.engine.handle(Input::ClaimedElsewhere).await;
    bench.pump().await;
    assert_eq!(bench.calls(), vec!["join Harbour"]);
    assert!(!bench.wifi_locked.load(Ordering::SeqCst));
}

#[tokio::test]
async fn a_claim_on_the_console_undoes_a_wi_fi_that_did_not_connect() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    *bench.device.wifi.lock().unwrap() = Some(Err("wrong_password"));
    bench
        .command(
            json!({ "op": "setWifi", "ssid": "Harbour", "psk": "sea-breeze-77", "hidden": false }),
        )
        .await;
    bench.device.facts.lock().unwrap().claimed = true;
    bench.engine.handle(Input::ClaimedElsewhere).await;
    bench.pump().await;
    assert_eq!(
        bench.calls().last().unwrap(),
        r#"restore {"mode":"ap"} remove ["Harbour"]"#
    );
}

/// A second `setWifi` that does not connect goes back to the first one's
/// network, not to the access point.
#[tokio::test]
async fn a_failed_second_wi_fi_goes_back_to_the_first_that_connected() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    *bench.device.wifi.lock().unwrap() = Some(Ok("192.168.1.40".into()));
    bench
        .command(json!({ "op": "setWifi", "ssid": "Harbour", "psk": "", "hidden": false }))
        .await;
    *bench.device.wifi.lock().unwrap() = Some(Err("ssid_not_found"));
    bench
        .command(json!({ "op": "setWifi", "ssid": "Marina", "psk": "", "hidden": false }))
        .await;
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    bench.tick(RECONNECT_SECONDS).await;
    bench.pump().await;
    assert_eq!(
        bench.calls(),
        vec![
            "join Harbour",
            "join Marina",
            r#"restore {"mode":"client"} remove ["Marina"]"#,
        ]
    );
}

#[tokio::test]
async fn a_claimed_session_that_never_connected_restores_the_mode_and_keeps_its_network() {
    let mut bench = Bench::new(true).await;
    bench.set_verifier(PASSWORD);
    assert_eq!(bench.authenticate("password", PASSWORD).await["t"], "ready");
    *bench.device.wifi.lock().unwrap() = Some(Err("wrong_password"));
    let events = bench
        .command(
            json!({ "op": "setWifi", "ssid": "Harbour", "psk": "sea-breeze-77", "hidden": false }),
        )
        .await;
    assert_eq!(events[1]["reason"], "wrong_password");
    let finished = bench.command(json!({ "op": "finish" })).await;
    assert_eq!(finished[0]["event"], "finished");
    bench.pump().await;
    assert_eq!(
        bench.calls().last().unwrap(),
        r#"restore {"mode":"ap"} remove []"#
    );
    assert_eq!(bench.store.history()[0].outcome, "finished");
}

#[tokio::test]
async fn a_long_connect_does_not_count_as_idle_but_silence_does() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    let events = bench
        .command(json!({ "op": "setWifi", "ssid": "Harbour", "psk": "", "hidden": false }))
        .await;
    assert_eq!(events, vec![json!({ "id": 1, "event": "connecting" })]);
    bench.tick(300).await;
    assert!(bench.radio.disconnected.lock().unwrap().is_empty());

    *bench.device.wifi.lock().unwrap() = Some(Ok("10.0.0.9".into()));
    bench.device.release.notify_waiters();
    bench.pump().await;
    assert_eq!(bench.events()[0]["event"], "connected");
    bench.tick(119).await;
    assert!(bench.radio.disconnected.lock().unwrap().is_empty());
    bench.tick(1).await;
    assert_eq!(
        bench.events(),
        vec![json!({ "event": "closing", "reason": "idle" })]
    );
    assert_eq!(*bench.radio.disconnected.lock().unwrap(), vec![PHONE]);
}

#[tokio::test]
async fn a_second_phone_is_refused_while_one_is_connected() {
    let mut bench = Bench::new(false).await;
    bench.read_info(PHONE).await.unwrap();
    assert_eq!(bench.read_info(OTHER).await, Err(Refused::Busy));
    assert_eq!(
        bench.write(OTHER, Written::Session, b"{}").await,
        Err(Refused::Busy)
    );
}

#[tokio::test]
async fn a_closed_window_accepts_no_phone() {
    let mut bench = Bench::new(false).await;
    bench.tick(600).await;
    assert_eq!(bench.last_advert(), None);
    assert!(!bench.radio.held.load(Ordering::SeqCst));
    assert_eq!(bench.read_info(PHONE).await, Err(Refused::Closed));
}

#[tokio::test]
async fn a_claim_elsewhere_ends_the_unauthenticated_session() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    bench.device.facts.lock().unwrap().claimed = true;
    bench.engine.handle(Input::ClaimedElsewhere).await;
    assert_eq!(
        bench.events(),
        vec![json!({ "event": "closing", "reason": "claimed_elsewhere" })]
    );
    assert_eq!(*bench.radio.disconnected.lock().unwrap(), vec![PHONE]);
    assert_eq!(bench.last_advert(), None);
}

#[tokio::test]
async fn a_claimed_device_without_a_verifier_says_password_unavailable() {
    let mut bench = Bench::new(true).await;
    assert_eq!(
        bench.read_info(PHONE).await.unwrap()["auth"],
        "password-unavailable"
    );
    let refused = bench
        .session(json!({ "t": "start", "auth": "password" }))
        .await;
    assert_eq!(
        refused,
        json!({ "t": "error", "reason": "password_unavailable" })
    );
    let refused = bench.session(json!({ "t": "start", "auth": "none" })).await;
    assert_eq!(refused["reason"], "unsupported_auth");
}

#[tokio::test]
async fn wrong_passwords_count_down_and_the_fifth_locks() {
    let mut bench = Bench::new(true).await;
    bench.set_verifier(PASSWORD);
    for left in (1..=4).rev() {
        let refused = bench.authenticate("password", "wrong-password").await;
        assert_eq!(
            refused,
            json!({ "t": "error", "reason": "auth_failed", "attemptsLeft": left })
        );
    }
    let locked = bench.authenticate("password", "wrong-password").await;
    assert_eq!(
        locked,
        json!({ "t": "error", "reason": "locked", "retryAfter": 600 })
    );
    // Even the right password waits, and the device stops advertising.
    let locked = bench.authenticate("password", PASSWORD).await;
    assert_eq!(locked["reason"], "locked");
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    assert_eq!(bench.last_advert(), None);
    let audit = bench.audit.lock().unwrap().clone();
    assert_eq!(
        audit
            .iter()
            .filter(|line| *line == "ble-auth failed")
            .count(),
        5
    );
    assert_eq!(bench.store.history()[0].outcome, "auth-failed");
}

#[tokio::test]
async fn a_phone_that_stops_after_the_share_has_used_an_attempt() {
    let mut bench = Bench::new(true).await;
    bench.set_verifier(PASSWORD);
    bench.exchange("password", "anything").await.unwrap();
    assert_eq!(
        bench.store.lockout().failures,
        1,
        "counted before the answer"
    );
    // The page learns of a wrong password at confirmV, so the share says
    // what is left.
    assert_eq!(bench.share["attemptsLeft"], json!(4));
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    assert_eq!(bench.store.lockout().failures, 1);
    assert!(
        bench
            .audit
            .lock()
            .unwrap()
            .contains(&"ble-auth failed".to_string())
    );
}

#[tokio::test]
async fn the_right_password_takes_its_charge_back() {
    let mut bench = Bench::new(true).await;
    bench.set_verifier(PASSWORD);
    let mut lockout = Lockout::default();
    for _ in 0..4 {
        lockout.charge(1);
    }
    bench.store.set_lockout(&lockout).unwrap();
    assert_eq!(bench.authenticate("password", PASSWORD).await["t"], "ready");
    assert_eq!(bench.store.lockout(), Lockout::default());
    let status = bench.command(json!({ "op": "getStatus" })).await;
    assert_eq!(status[0]["event"], "status");
    assert_eq!(status[0]["wifiMode"], "ap");
    let refused = bench
        .command(json!({ "op": "claim", "password": PASSWORD }))
        .await;
    assert_eq!(refused[0]["reason"], "unknown_op");
}

#[tokio::test]
async fn a_manual_window_takes_its_code_and_a_lockout_voids_it() {
    let mut bench = Bench::new(true).await;
    bench.tick(600).await;
    assert_eq!(bench.last_advert(), None);
    let (reply, answer) = oneshot::channel();
    bench
        .engine
        .handle(Input::OpenManual {
            seconds: 300,
            code: "40718263".into(),
            record: make_record(b"40718263", 1000).unwrap(),
            reply,
        })
        .await;
    answer.await.unwrap().unwrap();
    assert_eq!(bench.last_advert().as_deref(), Some("mica-a4e2e69e"));
    let (reply, status) = oneshot::channel();
    bench.engine.handle(Input::Status { reply }).await;
    let status = status.await.unwrap();
    assert_eq!(status["state"], "waiting");
    assert_eq!(status["trigger"], "manual");
    assert_eq!(status["code"], "40718263");
    assert_eq!(status["expiresAt"], json!(1_800_000_000 + 600 + 300));

    assert_eq!(bench.read_info(PHONE).await.unwrap()["auth"], "temp-code");
    assert_eq!(
        bench.authenticate("temp-code", "40718263").await["t"],
        "ready"
    );
    bench
        .engine
        .handle(Input::Disconnected {
            device: PHONE.into(),
        })
        .await;
    bench.phone = Phone::default();

    for _ in 0..5 {
        bench.authenticate("temp-code", "00000000").await;
    }
    let (reply, status) = oneshot::channel();
    bench.engine.handle(Input::Status { reply }).await;
    let status = status.await.unwrap();
    assert_eq!(status["state"], "connected");
    assert!(status["code"].is_null(), "the code is void");
    assert_eq!(status["lockedUntil"], json!(1_800_000_000 + 600 + 600));
}

#[tokio::test]
async fn offline_opens_after_the_route_has_been_gone_for_two_minutes() {
    let mut bench = Bench::new(true).await;
    bench.set_verifier(PASSWORD);
    bench.engine.handle(Input::Route(true)).await;
    bench.tick(700).await;
    assert_eq!(bench.last_advert(), None);
    bench.engine.handle(Input::Route(false)).await;
    bench.tick(119).await;
    assert_eq!(bench.last_advert(), None);
    bench.tick(1).await;
    assert_eq!(bench.last_advert().as_deref(), Some("mica-a4e2e69e"));
    bench.engine.handle(Input::Route(true)).await;
    assert_eq!(bench.last_advert(), None);
}

#[tokio::test]
async fn auto_when_offline_switched_off_keeps_the_offline_window_shut() {
    let mut bench = Bench::new(true).await;
    bench.set_verifier(PASSWORD);
    bench.engine.handle(Input::Route(true)).await;
    bench.tick(700).await;
    bench.device.facts.lock().unwrap().auto_when_offline = false;
    bench.engine.handle(Input::Route(false)).await;
    bench.tick(300).await;
    assert_eq!(bench.last_advert(), None);
    let (reply, status) = oneshot::channel();
    bench.engine.handle(Input::Status { reply }).await;
    assert_eq!(status.await.unwrap()["autoWhenOffline"], json!(false));

    bench.device.facts.lock().unwrap().auto_when_offline = true;
    bench.tick(1).await;
    assert_eq!(bench.last_advert().as_deref(), Some("mica-a4e2e69e"));
}

/// The console's addresses follow `access.web`: its scheme and port, the port
/// left out only where it is the scheme's own, and none when apid serves no
/// network.
#[test]
fn console_urls_follow_the_web_listeners() {
    let urls = |web: WebSettings, host: Option<&str>| console_urls(&web, host, "deck-7");
    let defaults = WebSettings::default();
    assert_eq!(
        urls(defaults, Some("192.168.1.40")),
        ["http://192.168.1.40:8080", "http://deck-7.local:8080"]
    );
    assert_eq!(urls(defaults, None), ["http://deck-7.local:8080"]);
    let https = WebSettings {
        https_enabled: true,
        ..defaults
    };
    assert_eq!(
        urls(https, Some("192.168.1.40")),
        ["https://192.168.1.40:8443", "https://deck-7.local:8443"]
    );
    assert_eq!(
        urls(
            WebSettings {
                https_port: 443,
                ..https
            },
            Some("192.168.1.40")
        ),
        ["https://192.168.1.40", "https://deck-7.local"]
    );
    assert_eq!(
        urls(
            WebSettings {
                http_port: 80,
                ..defaults
            },
            Some("fd00::5")
        ),
        ["http://[fd00::5]", "http://deck-7.local"]
    );
    for mode in [WebMode::Local, WebMode::Off] {
        assert!(urls(WebSettings { mode, ..defaults }, Some("192.168.1.40")).is_empty());
    }
}

#[tokio::test]
async fn scan_wifi_answers_the_networks_and_whether_they_are_stale() {
    let mut bench = Bench::new(false).await;
    bench.start_none().await;
    let harbour =
        json!({ "ssid": "Harbour", "signalDbm": -51, "security": "wpa2-psk", "channel": 6 });
    *bench.device.scanned.lock().unwrap() = Some(Scanned {
        networks: vec![harbour.clone()],
        age: None,
    });
    let fresh = bench.command(json!({ "op": "scanWifi" })).await;
    assert_eq!(
        fresh,
        vec![
            json!({ "id": 1, "event": "networks", "networks": [harbour.clone()], "stale": false })
        ]
    );

    *bench.device.scanned.lock().unwrap() = Some(Scanned {
        networks: vec![harbour.clone()],
        age: Some(std::time::Duration::from_secs(185)),
    });
    let stale = bench
        .command(json!({ "op": "scanWifi", "rescan": true }))
        .await;
    assert_eq!(
        stale,
        vec![
            json!({ "id": 2, "event": "networks", "networks": [harbour], "stale": true, "ageSeconds": 185 })
        ]
    );
    assert_eq!(
        bench.device.rescans.lock().unwrap().last(),
        Some(&true),
        "rescan reaches the device"
    );
    let refused = bench
        .command(json!({ "op": "scanWifi", "rescan": "yes" }))
        .await;
    assert_eq!(refused[0]["reason"], "bad_request");
}

//! The provisioning engine: one task that owns the windows, the connection,
//! the session protocol and the Wi-Fi held for it
//! (`docs/ble-provisioning.md`).
//!
//! Everything reaches it as an [`Input`]: BlueZ's reads and writes, the
//! default route, the console's requests, and the operations it started
//! finishing. What it does to the world goes through [`Radio`] (advertise,
//! notify, disconnect) and [`Device`] (settings, Wi-Fi, the wired port), so a test
//! drives it with no Bluetooth and no network.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use aws_lc_rs::agreement;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use micad_settings::{WebMode, WebSettings};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::channel::{self, Reassembly, Records};
use super::spake2plus::{self, Answer, Record, Transcript};
use super::store::{Entry, Lockout, Store};
use super::window::{Auth, Open, Trigger, Windows};
use super::{CONTEXT_PASSWORD, CONTEXT_TEMP_CODE, ID_PROVER, ID_VERIFIER, PROTOCOL};

/// A connection with no command for this long is dropped, unless an
/// operation is running.
pub const IDLE_SECONDS: u64 = 120;
/// How long the Wi-Fi a session changed waits for the phone to come back.
pub const RECONNECT_SECONDS: u64 = 120;
/// The operations this device accepts.
pub const OPS: [&str; 5] = ["getStatus", "scanWifi", "setWifi", "setNetwork", "finish"];
/// The ATT MTU assumed until a request says otherwise.
const DEFAULT_MTU: usize = 23;

/// The characteristics the engine notifies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notify {
    Session,
    Event,
}

/// The characteristics a phone writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Session,
    Command,
}

/// Why a read or write is refused at the GATT level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refused {
    /// Another phone holds the session.
    Busy,
    /// No window is open.
    Closed,
}

/// What the device is, as `info` and the session rules need it.
#[derive(Debug, Clone, Default)]
pub struct Facts {
    pub claimed: bool,
    pub hostname: String,
    pub model: String,
    pub auto_when_offline: bool,
    pub has_wifi: bool,
    /// `access.web`: where the console is served, for `finished`'s URLs.
    pub web: WebSettings,
}

/// The device side of the operations.
#[async_trait::async_trait]
pub trait Device: Send + Sync + 'static {
    async fn facts(&self) -> Facts;
    /// `getStatus`: `wifiMode`, `connected`, `ssid`, `ip`, `interfaces` and,
    /// while the access point is on, `ap`.
    async fn status(&self) -> Value;
    /// `scanWifi`: the networks, or a `failed` reason.
    async fn scan(&self) -> Result<Vec<Value>, &'static str>;
    /// The radio's role, to restore later.
    async fn wifi_snapshot(&self) -> Value;
    /// Save `ssid` first in the list and switch the radio to client mode.
    /// `Ok(true)` when the network was not saved before.
    async fn join_wifi(&self, ssid: &str, psk: &str, hidden: bool) -> Result<bool, &'static str>;
    /// Wait until the station is on `ssid` with an address: the address, or a
    /// `failed` reason.
    async fn await_wifi(&self, ssid: &str) -> Result<String, &'static str>;
    /// Put the radio's role back and remove the networks named.
    async fn restore_wifi(&self, snapshot: Value, remove: Vec<String>);
    /// Write `entry` (`{"dhcp":true}` or `{"static":{…}}`) as the wired
    /// `interface`'s addressing: the entry it had before (null for none), or
    /// a `failed` reason.
    async fn set_network(&self, interface: &str, entry: &Value) -> Result<Value, &'static str>;
    /// Wait until `interface` has the address `entry` gives it: the IPv4
    /// address (none when nothing observes it), or a `failed` reason.
    async fn await_network(
        &self,
        interface: &str,
        entry: &Value,
    ) -> Result<Option<String>, &'static str>;
    /// Put back the entries `setNetwork` replaced.
    async fn restore_network(&self, previous: Vec<(String, Value)>);
    /// A phone connected: begin what it is likely to ask for, so that it is
    /// ready when it does (a Wi-Fi scan).
    async fn warm(&self);
}

/// The Bluetooth side.
#[async_trait::async_trait]
pub trait Radio: Send + Sync + 'static {
    /// Advertise the service under `name`, or stop.
    async fn advertise(&self, name: Option<&str>);
    /// Keep the adapter powered for a window or a connection, or give it back
    /// to its setting.
    async fn hold(&self, on: bool);
    async fn notify(&self, characteristic: Notify, fragment: Vec<u8>);
    async fn disconnect(&self, device: &str);
}

/// Seconds since boot and since the epoch.
pub trait Clock: Send + Sync + 'static {
    fn uptime(&self) -> u64;
    fn unix(&self) -> u64;
}

/// What reaches the engine.
pub enum Input {
    /// A phone read `info`.
    Read {
        device: String,
        mtu: Option<u16>,
        reply: oneshot::Sender<Result<Vec<u8>, Refused>>,
    },
    /// A phone wrote a fragment.
    Write {
        device: String,
        mtu: Option<u16>,
        characteristic: Written,
        value: Vec<u8>,
        reply: oneshot::Sender<Result<(), Refused>>,
    },
    /// A phone's link dropped.
    Disconnected { device: String },
    /// Whether a default route exists.
    Route(bool),
    /// Another channel claimed the device.
    ClaimedElsewhere,
    /// The console opens a `manual` window with this code and its verifier.
    OpenManual {
        seconds: u64,
        code: String,
        record: Record,
        reply: oneshot::Sender<Result<(), &'static str>>,
    },
    /// The console closes the `manual` window.
    CloseManual { reply: oneshot::Sender<()> },
    /// The console reads the state.
    Status { reply: oneshot::Sender<Value> },
    /// One second passed.
    Tick,
    /// An operation's network was saved; the session added it if `added`.
    WifiSaved {
        serial: u64,
        ssid: String,
        added: bool,
    },
    /// An operation replaced `interface`'s entry, which was `previous`.
    NetworkSaved {
        serial: u64,
        interface: String,
        previous: Value,
    },
    /// An event for connection `serial` that no command asked for.
    Pushed { serial: u64, event: Value },
    /// An operation ended with this final event.
    Done {
        serial: u64,
        id: Value,
        op: &'static str,
        event: Value,
    },
}

enum Phase {
    /// Nothing started, or the last exchange failed.
    Idle,
    /// `params` sent; waiting for the phone's share.
    Params { auth: Auth, record: Record },
    /// `share` sent and charged; waiting for `confirm`.
    Confirm {
        auth: Auth,
        answer: Answer,
        before: Lockout,
    },
    /// Authenticated: records in both directions.
    Ready {
        auth: Auth,
        from_phone: Records,
        to_phone: Records,
    },
}

struct Connection {
    serial: u64,
    device: String,
    mtu: usize,
    trigger: Trigger,
    auth: Auth,
    session_in: Reassembly,
    command_in: Reassembly,
    phase: Phase,
    op: Option<&'static str>,
    last_active: u64,
    finished: bool,
    failed: bool,
    /// When the phone connected, and when the command in hand arrived: the
    /// step log's times.
    since: Instant,
    command_at: Option<Instant>,
    /// `start` asked for a `status` event once the session is ready.
    push_status: bool,
    /// How the session last brought the device onto a network.
    reached: Option<Reached>,
}

/// A network a session brought the device onto.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reached {
    Wifi,
    Wired(String),
}

/// The Wi-Fi and the wired entries a session changed, kept until the
/// session is over.
struct Hold {
    /// The radio's role before `setWifi`, when it ran.
    snapshot: Option<Value>,
    added: Vec<String>,
    connected: bool,
    /// The wired entries `setNetwork` replaced, as they were.
    replaced: Vec<(String, Value)>,
    unclaimed: bool,
    away_since: Option<u64>,
}

/// The engine's state; [`run`] feeds it.
pub struct Engine {
    device: Arc<dyn Device>,
    radio: Arc<dyn Radio>,
    clock: Arc<dyn Clock>,
    store: Store,
    audit: Audit,
    tx: mpsc::UnboundedSender<Input>,
    wifi_locked: Arc<AtomicBool>,
    windows: Windows,
    temp_code: Option<(String, Record)>,
    advertised: Option<String>,
    holding: bool,
    connection: Option<Connection>,
    hold: Option<Hold>,
    serial: u64,
    done: bool,
    facts: Facts,
}

/// Appends one audit line: event, outcome.
pub type Audit = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// How an engine is built.
pub struct Parts {
    pub device: Arc<dyn Device>,
    pub radio: Arc<dyn Radio>,
    pub clock: Arc<dyn Clock>,
    pub store: Store,
    pub audit: Audit,
    pub wifi_locked: Arc<AtomicBool>,
}

impl Engine {
    /// The engine of a boot, with the facts it booted with.
    pub fn new(parts: Parts, facts: Facts, tx: mpsc::UnboundedSender<Input>) -> Self {
        Self {
            device: parts.device,
            radio: parts.radio,
            clock: parts.clock,
            store: parts.store,
            audit: parts.audit,
            tx,
            wifi_locked: parts.wifi_locked,
            windows: Windows::new(facts.claimed),
            temp_code: None,
            advertised: None,
            holding: false,
            connection: None,
            hold: None,
            serial: 0,
            done: false,
            facts,
        }
    }

    fn window(&self) -> Option<Open> {
        self.windows.current(
            self.clock.uptime(),
            self.facts.claimed,
            self.facts.auto_when_offline,
        )
    }

    fn locked_for(&self) -> Option<u64> {
        self.store.lockout().remaining(self.clock.unix())
    }

    /// `info.auth` for a window's mode.
    fn auth_name(&self, auth: Auth) -> &'static str {
        if auth == Auth::Password && !matches!(self.store.verifier(), Ok(Some(_))) {
            "password-unavailable"
        } else {
            auth.name()
        }
    }

    /// Handle one input.
    pub async fn handle(&mut self, input: Input) {
        // A phone's reads and writes come many to a message; what the device
        // is changes with what the rest brings, and on the tick.
        let refresh = !matches!(input, Input::Read { .. } | Input::Write { .. });
        match input {
            Input::Read { device, mtu, reply } => {
                let started = Instant::now();
                let answer = self.admit(&device, mtu).map(|()| self.info());
                let _ = reply.send(answer);
                self.step("info", started);
            }
            Input::Write {
                device,
                mtu,
                characteristic,
                value,
                reply,
            } => {
                let admitted = self.admit(&device, mtu);
                let _ = reply.send(admitted);
                if admitted.is_ok() {
                    match characteristic {
                        Written::Session => self.on_session(&value).await,
                        Written::Command => self.on_command(&value).await,
                    }
                }
            }
            Input::Disconnected { device } => {
                if self
                    .connection
                    .as_ref()
                    .is_some_and(|connection| connection.device == device)
                {
                    self.end_connection();
                }
            }
            Input::Route(present) => self.windows.route(self.clock.uptime(), present),
            Input::ClaimedElsewhere => self.claimed_elsewhere().await,
            Input::OpenManual {
                seconds,
                code,
                record,
                reply,
            } => {
                let answer = if !self.facts.claimed {
                    Err("not_claimed")
                } else if self.locked_for().is_some() {
                    Err("locked")
                } else {
                    self.windows.open_manual(self.clock.uptime(), seconds);
                    self.temp_code = Some((code, record));
                    self.done = false;
                    (self.audit)("ble-window", "opened");
                    Ok(())
                };
                let _ = reply.send(answer);
            }
            Input::CloseManual { reply } => {
                self.windows.close_manual();
                self.temp_code = None;
                (self.audit)("ble-window", "closed");
                let _ = reply.send(());
            }
            Input::Status { reply } => {
                let _ = reply.send(self.status());
            }
            Input::Tick => {}
            Input::WifiSaved {
                serial,
                ssid,
                added,
            } => {
                if added
                    && self.current_serial() == Some(serial)
                    && let Some(hold) = &mut self.hold
                {
                    hold.added.push(ssid);
                }
            }
            Input::NetworkSaved {
                serial,
                interface,
                previous,
            } => {
                if self.current_serial() == Some(serial)
                    && let Some(hold) = &mut self.hold
                    && !hold.replaced.iter().any(|(name, _)| *name == interface)
                {
                    hold.replaced.push((interface, previous));
                }
            }
            Input::Pushed { serial, event } => {
                if self.current_serial() == Some(serial) {
                    self.send_event(&event).await;
                }
            }
            Input::Done {
                serial,
                id,
                op,
                event,
            } => self.done_op(serial, id, op, event).await,
        }
        self.settle(refresh).await;
    }

    /// Log how long `step` took on the device, and when, from the
    /// connection: the gaps between steps are the link and the phone.
    fn step(&self, step: &str, started: Instant) {
        if let Some(connection) = &self.connection {
            tracing::info!(
                step,
                ms = millis(started),
                at = millis(connection.since),
                mtu = connection.mtu,
                "ble"
            );
        }
    }

    fn current_serial(&self) -> Option<u64> {
        self.connection.as_ref().map(|connection| connection.serial)
    }

    /// Whether `device` may use the service: the phone already connected, or
    /// a new one while a window is open and nobody is.
    fn admit(&mut self, device: &str, mtu: Option<u16>) -> Result<(), Refused> {
        if let Some(connection) = &mut self.connection {
            if connection.device != device {
                return Err(Refused::Busy);
            }
            if let Some(mtu) = mtu {
                connection.mtu = usize::from(mtu);
            }
            return Ok(());
        }
        let Some(open) = self.window() else {
            return Err(Refused::Closed);
        };
        self.serial += 1;
        self.connection = Some(Connection {
            serial: self.serial,
            device: device.to_string(),
            mtu: mtu.map_or(DEFAULT_MTU, usize::from),
            trigger: open.trigger,
            auth: open.auth,
            session_in: Reassembly::default(),
            command_in: Reassembly::default(),
            phase: Phase::Idle,
            op: None,
            last_active: self.clock.uptime(),
            finished: false,
            failed: false,
            since: Instant::now(),
            command_at: None,
            push_status: false,
            reached: None,
        });
        if let Some(hold) = &mut self.hold {
            hold.away_since = None;
        }
        if self.facts.has_wifi {
            let device = Arc::clone(&self.device);
            tokio::spawn(async move { device.warm().await });
        }
        Ok(())
    }

    fn info(&self) -> Vec<u8> {
        let (auth, mtu) = self
            .connection
            .as_ref()
            .map_or((Auth::None, DEFAULT_MTU), |connection| {
                (connection.auth, connection.mtu)
            });
        json!({
            "protocol": PROTOCOL,
            "model": self.facts.model,
            "hostname": self.facts.hostname,
            "claimed": self.facts.claimed,
            "auth": self.auth_name(auth),
            "ops": OPS,
            "maxWrite": (mtu.saturating_sub(3)).clamp(channel::MIN_FRAGMENT, channel::MAX_FRAGMENT),
        })
        .to_string()
        .into_bytes()
    }

    async fn send(&self, characteristic: Notify, message: &[u8]) {
        let mtu = self
            .connection
            .as_ref()
            .map_or(DEFAULT_MTU, |connection| connection.mtu);
        for fragment in channel::fragments(message, mtu.saturating_sub(3)) {
            self.radio.notify(characteristic, fragment).await;
        }
    }

    async fn send_session(&self, message: &Value) {
        self.send(Notify::Session, message.to_string().as_bytes())
            .await;
    }

    async fn session_error(&self, reason: &str) {
        self.send_session(&json!({ "t": "error", "reason": reason }))
            .await;
    }

    /// Seal `event` for the phone and send it; a session that cannot seal is
    /// over.
    async fn send_event(&mut self, event: &Value) {
        let Some(connection) = &mut self.connection else {
            return;
        };
        let Phase::Ready { to_phone, .. } = &mut connection.phase else {
            return;
        };
        match to_phone.seal(event.to_string().as_bytes()) {
            Ok(record) => self.send(Notify::Event, &record).await,
            Err(err) => {
                tracing::warn!(error = %err, "a provisioning event could not be sealed");
                self.drop_connection().await;
            }
        }
    }

    /// An exchange that was charged and never confirmed: one failure, which
    /// the audit trail records.
    fn abandon_exchange(&mut self) {
        if let Some(connection) = &mut self.connection
            && matches!(connection.phase, Phase::Confirm { .. })
        {
            connection.phase = Phase::Idle;
            connection.failed = true;
            (self.audit)("ble-auth", "failed");
        }
    }

    async fn on_session(&mut self, fragment: &[u8]) {
        let Some(connection) = &mut self.connection else {
            return;
        };
        connection.last_active = self.clock.uptime();
        let message = match connection.session_in.push(fragment) {
            Ok(Some(message)) => message,
            Ok(None) => return,
            Err(_) => return self.session_error("bad_message").await,
        };
        let Ok(message) = serde_json::from_slice::<Value>(&message) else {
            return self.session_error("bad_message").await;
        };
        let started = Instant::now();
        match message["t"].as_str() {
            Some("start") => self.start(&message).await,
            Some("share") => self.share(&message).await,
            Some("confirm") => self.confirm(&message).await,
            _ => return self.session_error("bad_message").await,
        }
        self.step(message["t"].as_str().unwrap_or_default(), started);
    }

    /// Send `ready`, and after it the `status` the `start` asked for, which
    /// `ready` then says will come.
    async fn ready(&mut self, mut ready: Value) {
        let Some(connection) = &self.connection else {
            return;
        };
        let (push, serial) = (connection.push_status, connection.serial);
        if push {
            ready["status"] = json!(true);
        }
        self.send_session(&ready).await;
        if !push {
            return;
        }
        let (device, tx) = (Arc::clone(&self.device), self.tx.clone());
        tokio::spawn(async move {
            let mut event = device.status().await;
            event["event"] = json!("status");
            let _ = tx.send(Input::Pushed { serial, event });
        });
    }

    async fn start(&mut self, message: &Value) {
        self.abandon_exchange();
        if let Some(connection) = &mut self.connection {
            connection.push_status = message["status"] == true;
        }
        let Some(requested) = message["auth"].as_str() else {
            return self.session_error("bad_message").await;
        };
        let Some(open) = self.window().filter(|open| open.auth.name() == requested) else {
            return self.session_error("unsupported_auth").await;
        };
        let record = match open.auth {
            Auth::None => return self.start_none(message).await,
            Auth::Password => match self.store.verifier() {
                Ok(Some(record)) => record,
                _ => return self.session_error("password_unavailable").await,
            },
            Auth::TempCode => match &self.temp_code {
                Some((_, record)) => record.clone(),
                None => return self.session_error("unsupported_auth").await,
            },
        };
        if let Some(retry_after) = self.locked_for() {
            return self
                .send_session(
                    &json!({ "t": "error", "reason": "locked", "retryAfter": retry_after }),
                )
                .await;
        }
        let params = json!({
            "t": "params",
            "salt": BASE64.encode(&record.salt),
            "iterations": record.iterations,
        });
        if let Some(connection) = &mut self.connection {
            connection.auth = open.auth;
            connection.trigger = open.trigger;
            connection.phase = Phase::Params {
                auth: open.auth,
                record,
            };
        }
        self.send_session(&params).await;
    }

    async fn start_none(&mut self, message: &Value) {
        let Some(phone) = binary(&message["pub"]) else {
            return self.session_error("bad_message").await;
        };
        let exchanged = agreement::PrivateKey::generate(&agreement::X25519)
            .map_err(|_| "X25519 key generation failed".to_string())
            .and_then(|key| channel::none_exchange(&key, &phone));
        let Ok((public, keys)) = exchanged else {
            return self.session_error("bad_message").await;
        };
        let (Ok(from_phone), Ok(to_phone)) = (
            Records::new(&keys.phone_to_device),
            Records::new(&keys.device_to_phone),
        ) else {
            return self.session_error("bad_message").await;
        };
        if let Some(connection) = &mut self.connection {
            connection.phase = Phase::Ready {
                auth: Auth::None,
                from_phone,
                to_phone,
            };
            connection.last_active = self.clock.uptime();
        }
        self.ready(json!({ "t": "ready", "pub": BASE64.encode(public) }))
            .await;
    }

    async fn share(&mut self, message: &Value) {
        let Some(connection) = &mut self.connection else {
            return;
        };
        let Phase::Params { auth, record } = std::mem::replace(&mut connection.phase, Phase::Idle)
        else {
            return self.session_error("bad_message").await;
        };
        let Some(share_p) = binary(&message["shareP"]) else {
            return self.session_error("bad_message").await;
        };
        let transcript = Transcript {
            context: if auth == Auth::TempCode {
                CONTEXT_TEMP_CODE
            } else {
                CONTEXT_PASSWORD
            },
            id_prover: ID_PROVER,
            id_verifier: ID_VERIFIER,
        };
        let Ok(answer) = spake2plus::answer(&record, &transcript, &share_p) else {
            return self.session_error("bad_message").await;
        };
        // Counted before the phone can learn anything from the answer, and
        // persisted first: a phone that stops here has used an attempt.
        let before = self.store.lockout();
        let mut charged = before.clone();
        charged.charge(self.clock.unix());
        if let Err(err) = self.store.set_lockout(&charged) {
            tracing::warn!(error = %format!("{err:#}"), "the failure count could not be written");
            return self.session_error("bad_message").await;
        }
        let reply = json!({
            "t": "share",
            "shareV": BASE64.encode(answer.share_v),
            "confirmV": BASE64.encode(answer.keys.confirm_v),
            "attemptsLeft": charged.attempts_left(),
        });
        if let Some(connection) = &mut self.connection {
            connection.phase = Phase::Confirm {
                auth,
                answer,
                before,
            };
        }
        self.send_session(&reply).await;
    }

    async fn confirm(&mut self, message: &Value) {
        let Some(connection) = &mut self.connection else {
            return;
        };
        let Phase::Confirm {
            auth,
            answer,
            before,
        } = std::mem::replace(&mut connection.phase, Phase::Idle)
        else {
            return self.session_error("bad_message").await;
        };
        let valid = binary(&message["confirmP"]).is_some_and(|confirm_p| {
            aws_lc_rs::constant_time::verify_slices_are_equal(&confirm_p, &answer.keys.confirm_p)
                .is_ok()
        });
        if !valid {
            connection.failed = true;
            (self.audit)("ble-auth", "failed");
            if let Some(retry_after) = self.locked_for() {
                // A lockout voids the code of a manual window and closes it.
                self.windows.close_manual();
                self.temp_code = None;
                (self.audit)("ble-lockout", "locked");
                return self
                    .send_session(
                        &json!({ "t": "error", "reason": "locked", "retryAfter": retry_after }),
                    )
                    .await;
            }
            let left = self.store.lockout().attempts_left();
            return self
                .send_session(
                    &json!({ "t": "error", "reason": "auth_failed", "attemptsLeft": left }),
                )
                .await;
        }
        let mut lockout = self.store.lockout();
        lockout.succeed(&before);
        if let Err(err) = self.store.set_lockout(&lockout) {
            tracing::warn!(error = %format!("{err:#}"), "the failure count could not be written");
        }
        let keys = channel::session_keys(&[], &answer.keys.shared);
        let records = keys.and_then(|keys| {
            Ok((
                Records::new(&keys.phone_to_device)?,
                Records::new(&keys.device_to_phone)?,
            ))
        });
        let Ok((from_phone, to_phone)) = records else {
            return self.session_error("bad_message").await;
        };
        if let Some(connection) = &mut self.connection {
            connection.phase = Phase::Ready {
                auth,
                from_phone,
                to_phone,
            };
            connection.last_active = self.clock.uptime();
        }
        (self.audit)("ble-auth", "succeeded");
        self.ready(json!({ "t": "ready" })).await;
    }

    async fn on_command(&mut self, fragment: &[u8]) {
        let Some(connection) = &mut self.connection else {
            return;
        };
        let Phase::Ready { from_phone, .. } = &mut connection.phase else {
            return;
        };
        let record = match connection.command_in.push(fragment) {
            Ok(Some(record)) => record,
            Ok(None) => return,
            Err(_) => return self.drop_connection().await,
        };
        let Ok(plaintext) = from_phone.open(&record) else {
            return self.drop_connection().await;
        };
        connection.last_active = self.clock.uptime();
        let Ok(command) = serde_json::from_slice::<Value>(&plaintext) else {
            return self.failed(Value::Null, "bad_request").await;
        };
        let id = command["id"].clone();
        if !id.is_i64() && !id.is_u64() {
            return self.failed(Value::Null, "bad_request").await;
        }
        if connection.op.is_some() {
            return self.failed(id, "busy").await;
        }
        connection.command_at = Some(Instant::now());
        match command["op"].as_str() {
            Some("getStatus") => self.spawn_status(id),
            Some("scanWifi") => self.spawn_scan(id),
            Some("setWifi") => self.set_wifi(id, &command).await,
            Some("setNetwork") => self.set_network(id, &command).await,
            Some("finish") => self.finish(id).await,
            Some(_) => self.failed(id, "unknown_op").await,
            None => self.failed(id, "bad_request").await,
        }
        // An operation that answered at once; the others log when done.
        if let Some(connection) = &mut self.connection
            && connection.op.is_none()
            && let Some(started) = connection.command_at.take()
        {
            self.step(command["op"].as_str().unwrap_or_default(), started);
        }
    }

    async fn failed(&mut self, id: Value, reason: &str) {
        self.send_event(&json!({ "id": id, "event": "failed", "reason": reason }))
            .await;
    }

    fn session_auth(&self) -> Option<Auth> {
        match &self.connection.as_ref()?.phase {
            Phase::Ready { auth, .. } => Some(*auth),
            _ => None,
        }
    }

    /// Run `work` as the connection's operation `op`; its event comes back
    /// as [`Input::Done`].
    fn spawn_op<F>(&mut self, id: Value, op: &'static str, work: F)
    where
        F: std::future::Future<Output = Value> + Send + 'static,
    {
        let Some(connection) = &mut self.connection else {
            return;
        };
        connection.op = Some(op);
        let serial = connection.serial;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let event = work.await;
            let _ = tx.send(Input::Done {
                serial,
                id,
                op,
                event,
            });
        });
    }

    fn spawn_status(&mut self, id: Value) {
        let device = Arc::clone(&self.device);
        self.spawn_op(id, "getStatus", async move {
            let mut event = device.status().await;
            event["event"] = json!("status");
            event
        });
    }

    fn spawn_scan(&mut self, id: Value) {
        let device = Arc::clone(&self.device);
        self.spawn_op(id, "scanWifi", async move {
            match device.scan().await {
                Ok(networks) => json!({ "event": "networks", "networks": networks }),
                Err(reason) => json!({ "event": "failed", "reason": reason }),
            }
        });
    }

    async fn set_wifi(&mut self, id: Value, command: &Value) {
        let (Some(ssid), Some(psk)) = (command["ssid"].as_str(), command["psk"].as_str()) else {
            return self.failed(id, "bad_request").await;
        };
        let hidden = command["hidden"].as_bool().unwrap_or(false);
        if let Err(reason) = self.still_unclaimed().await {
            return self.failed(id, reason).await;
        }
        if ssid.is_empty() || ssid.len() > 32 {
            return self.failed(id, "invalid_ssid").await;
        }
        if !psk.is_empty() && micad_settings::validate_wifi_psk(psk).is_err() {
            return self.failed(id, "invalid_psk").await;
        }
        if !self.facts.has_wifi {
            return self.failed(id, "no_radio").await;
        }
        if self
            .hold
            .as_ref()
            .is_none_or(|hold| hold.snapshot.is_none())
        {
            let snapshot = self.device.wifi_snapshot().await;
            self.hold().snapshot = Some(snapshot);
        }
        let hostname = self.facts.hostname.clone();
        let device = Arc::clone(&self.device);
        let tx = self.tx.clone();
        let serial = self.current_serial().unwrap_or_default();
        let (ssid, psk) = (ssid.to_string(), psk.to_string());
        self.spawn_op(id.clone(), "setWifi", async move {
            match device.join_wifi(&ssid, &psk, hidden).await {
                Ok(added) => {
                    let _ = tx.send(Input::WifiSaved {
                        serial,
                        ssid: ssid.clone(),
                        added,
                    });
                }
                Err(reason) => return json!({ "event": "failed", "reason": reason }),
            }
            match device.await_wifi(&ssid).await {
                Ok(ip) => json!({ "event": "connected", "ip": ip, "hostname": hostname }),
                Err(reason) => json!({ "event": "failed", "reason": reason }),
            }
        });
        self.send_event(&json!({ "id": id, "event": "connecting" }))
            .await;
    }

    /// A `none` session is for a device still unclaimed: one claimed
    /// meanwhile changes nothing over it.
    async fn still_unclaimed(&self) -> Result<(), &'static str> {
        if self.session_auth() == Some(Auth::None) && self.device.facts().await.claimed {
            return Err("already_claimed");
        }
        Ok(())
    }

    /// The session's hold on what it changes, begun if it was not.
    fn hold(&mut self) -> &mut Hold {
        let unclaimed = self.session_auth() == Some(Auth::None);
        self.hold.get_or_insert_with(|| Hold {
            snapshot: None,
            added: Vec::new(),
            connected: false,
            replaced: Vec::new(),
            unclaimed,
            away_since: None,
        })
    }

    async fn set_network(&mut self, id: Value, command: &Value) {
        let Some(interface) = command["interface"].as_str().map(str::to_string) else {
            return self.failed(id, "bad_request").await;
        };
        let entry = match (&command["dhcp"], &command["static"]) {
            (Value::Bool(true), Value::Null) => json!({ "dhcp": true }),
            (Value::Null | Value::Bool(false), Value::Object(_)) => {
                json!({ "static": command["static"] })
            }
            _ => return self.failed(id, "bad_request").await,
        };
        if let Err(reason) = self.still_unclaimed().await {
            return self.failed(id, reason).await;
        }
        self.hold();
        let device = Arc::clone(&self.device);
        let tx = self.tx.clone();
        let serial = self.current_serial().unwrap_or_default();
        let id_ack = id.clone();
        self.spawn_op(id, "setNetwork", async move {
            match device.set_network(&interface, &entry).await {
                Ok(previous) => {
                    let _ = tx.send(Input::NetworkSaved {
                        serial,
                        interface: interface.clone(),
                        previous,
                    });
                }
                Err(reason) => return json!({ "event": "failed", "reason": reason }),
            }
            match device.await_network(&interface, &entry).await {
                Ok(address) => json!({ "event": "up", "interface": interface, "address": address }),
                Err(reason) => json!({ "event": "failed", "reason": reason }),
            }
        });
        self.send_event(&json!({ "id": id_ack, "event": "applying" }))
            .await;
    }

    async fn finish(&mut self, id: Value) {
        if let Err(reason) = self.still_unclaimed().await {
            return self.failed(id, reason).await;
        }
        let Some(connection) = &self.connection else {
            return;
        };
        let reached = connection.reached.clone();
        let device = Arc::clone(&self.device);
        self.spawn_op(id, "finish", async move {
            let facts = device.facts().await;
            let hostname = facts.hostname;
            let status = device.status().await;
            let (interface, urls) = console(&status, reached.as_ref(), &hostname, &facts.web);
            json!({
                "event": "finished",
                "claimed": facts.claimed,
                "hostname": hostname,
                "interface": interface,
                "consoleUrls": urls,
                "ap": status
                    .get("ap")
                    .filter(|ap| ap.is_object())
                    .cloned()
                    .unwrap_or_else(|| json!({ "on": false })),
            })
        });
    }

    async fn done_op(&mut self, serial: u64, id: Value, op: &'static str, mut event: Value) {
        if let Some(hold) = &mut self.hold
            && op == "setWifi"
            && event["event"] == "connected"
        {
            hold.connected = true;
        }
        let Some(connection) = self.connection.as_mut().filter(|c| c.serial == serial) else {
            return;
        };
        match (op, event["event"].as_str(), event["interface"].as_str()) {
            ("setWifi", Some("connected"), _) => connection.reached = Some(Reached::Wifi),
            ("setNetwork", Some("up"), Some(name)) => {
                connection.reached = Some(Reached::Wired(name.to_string()));
            }
            _ => {}
        }
        connection.op = None;
        connection.last_active = self.clock.uptime();
        if let Some(started) = connection.command_at.take() {
            self.step(op, started);
        }
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        let finished = op == "finish" && event["event"] == "finished";
        if finished {
            connection.finished = true;
        }
        event["id"] = id;
        self.send_event(&event).await;
        if finished {
            self.windows.finish(self.clock.uptime());
            self.temp_code = None;
            self.done = true;
            self.facts = self.device.facts().await;
            self.end_hold(true);
            self.drop_connection().await;
        }
    }

    async fn claimed_elsewhere(&mut self) {
        self.facts = self.device.facts().await;
        let unclaimed = self
            .connection
            .as_ref()
            .is_some_and(|connection| connection.auth == Auth::None);
        if unclaimed {
            self.send_event(&json!({ "event": "closing", "reason": "claimed_elsewhere" }))
                .await;
            self.drop_connection().await;
        }
        if self.hold.as_ref().is_some_and(|hold| hold.unclaimed) {
            self.end_hold(false);
        }
    }

    /// Disconnect the phone and end its connection here, not waiting for
    /// BlueZ to report it.
    async fn drop_connection(&mut self) {
        if let Some(connection) = &self.connection {
            let device = connection.device.clone();
            self.radio.disconnect(&device).await;
        }
        self.end_connection();
    }

    fn end_connection(&mut self) {
        self.abandon_exchange();
        let Some(connection) = self.connection.take() else {
            return;
        };
        let outcome = if connection.finished {
            "finished"
        } else if self.hold.as_ref().is_some_and(|hold| hold.connected) {
            "wifi-connected"
        } else if connection.failed && !matches!(connection.phase, Phase::Ready { .. }) {
            "auth-failed"
        } else {
            "abandoned"
        };
        let trigger = serde_json::to_value(connection.trigger)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default();
        if let Err(err) = self.store.push_history(Entry {
            at: self.clock.unix(),
            trigger,
            outcome: outcome.to_string(),
        }) {
            tracing::warn!(error = %format!("{err:#}"), "the session history could not be written");
        }
        (self.audit)("ble-session", outcome);
        if let Some(hold) = &mut self.hold {
            hold.away_since = Some(self.clock.uptime());
        }
    }

    /// The session's hold is over: put back what it must not leave behind.
    fn end_hold(&mut self, finished: bool) {
        let Some(hold) = self.hold.take() else {
            return;
        };
        let abandoned = hold.unclaimed && !finished;
        let (restore, remove) = if abandoned {
            (true, hold.added)
        } else {
            (!hold.connected, Vec::new())
        };
        let device = Arc::clone(&self.device);
        if let (true, Some(snapshot)) = (restore, hold.snapshot) {
            let device = Arc::clone(&device);
            tokio::spawn(async move { device.restore_wifi(snapshot, remove).await });
        }
        if abandoned && !hold.replaced.is_empty() {
            let replaced = hold.replaced;
            tokio::spawn(async move { device.restore_network(replaced).await });
        }
    }

    /// Bring the timers, the advertisement and the radio in line with now.
    async fn settle(&mut self, refresh: bool) {
        if refresh {
            self.facts = self.device.facts().await;
        }
        let now = self.clock.uptime();
        let window = self.window();
        if window.is_none_or(|open| open.trigger != Trigger::Manual) {
            self.temp_code = None;
        }
        let idle = self.connection.as_ref().is_some_and(|connection| {
            connection.op.is_none() && now >= connection.last_active + IDLE_SECONDS
        });
        if idle {
            self.send_event(&json!({ "event": "closing", "reason": "idle" }))
                .await;
            self.drop_connection().await;
        }
        let expired = self.hold.as_ref().is_some_and(|hold| {
            hold.away_since
                .is_some_and(|since| now >= since + RECONNECT_SECONDS || window.is_none())
        });
        if expired {
            self.end_hold(false);
        }
        if window.is_some() && self.connection.is_none() {
            self.done = false;
        }
        let locked = self.locked_for().is_some();
        let advertise = window
            .filter(|open| self.connection.is_none() && !(open.auth != Auth::None && locked))
            .map(|_| self.facts.hostname.clone());
        if advertise != self.advertised {
            self.radio.advertise(advertise.as_deref()).await;
            self.advertised = advertise;
        }
        let hold = window.is_some() || self.connection.is_some();
        if hold != self.holding {
            self.radio.hold(hold).await;
            self.holding = hold;
        }
        let authenticated = self
            .connection
            .as_ref()
            .is_some_and(|connection| matches!(connection.phase, Phase::Ready { .. }));
        self.wifi_locked
            .store(authenticated || self.hold.is_some(), Ordering::SeqCst);
    }

    fn status(&self) -> Value {
        let window = self.window();
        let state = if self.connection.is_some() {
            "connected"
        } else if window.is_some() {
            "waiting"
        } else if self.done {
            "done"
        } else {
            "off"
        };
        let (now, unix) = (self.clock.uptime(), self.clock.unix());
        let lockout = self.store.lockout();
        let mut status = json!({
            "state": state,
            "trigger": window.map(|open| open.trigger),
            "expiresAt": window
                .and_then(|open| open.closes_at)
                .map(|at| unix + at.saturating_sub(now)),
            "autoWhenOffline": self.facts.auto_when_offline,
            "verifierReady": matches!(self.store.verifier(), Ok(Some(_))),
            "lockedUntil": lockout.locked_until.filter(|until| *until > unix),
            "wifiLocked": self.wifi_locked.load(Ordering::SeqCst),
            "history": self.store.history().iter().rev().collect::<Vec<_>>(),
        });
        if let Some((code, _)) = &self.temp_code {
            status["code"] = json!(code);
        }
        status
    }
}

/// The interface `finished` names, from `getStatus`'s `status`: the one the
/// session brought up, else the first with an address, Ethernet first; and
/// the console's addresses on it.
fn console(
    status: &Value,
    reached: Option<&Reached>,
    hostname: &str,
    web: &WebSettings,
) -> (Value, Vec<String>) {
    let interfaces = status["interfaces"].as_array().cloned().unwrap_or_default();
    let found = match reached {
        Some(Reached::Wired(name)) => interfaces.iter().find(|link| link["name"] == name.as_str()),
        Some(Reached::Wifi) => interfaces.iter().find(|link| link["kind"] == "wifi"),
        None => interfaces
            .iter()
            .filter(|link| link["address"].is_string())
            .min_by_key(|link| link["kind"] != "ethernet"),
    };
    let host = found
        .and_then(|link| link["address"].as_str())
        .map(|address| address.split('/').next().unwrap_or(address));
    let urls = console_urls(web, host, hostname);
    let interface = found.map_or(
        Value::Null,
        |link| json!({ "name": link["name"], "kind": link["kind"], "address": link["address"] }),
    );
    (interface, urls)
}

/// The console at `host`, when there is one, and at `<hostname>.local`, as
/// `web` serves it: HTTPS on its port when enabled, else HTTP on its port, the
/// port left out where it is the scheme's own. None when apid serves no
/// network (`local` or `off`).
pub(super) fn console_urls(web: &WebSettings, host: Option<&str>, hostname: &str) -> Vec<String> {
    if web.mode != WebMode::Network {
        return Vec::new();
    }
    let (scheme, port, default) = if web.https_enabled {
        ("https", web.https_port, 443)
    } else {
        ("http", web.http_port, 80)
    };
    let port = if port == default {
        String::new()
    } else {
        format!(":{port}")
    };
    let local = format!("{hostname}.local");
    host.map(|host| {
        if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_string()
        }
    })
    .into_iter()
    .chain([local])
    .map(|host| format!("{scheme}://{host}{port}"))
    .collect()
}

/// Milliseconds since `since`.
fn millis(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A base64 member, decoded.
fn binary(value: &Value) -> Option<Vec<u8>> {
    BASE64.decode(value.as_str()?).ok()
}

/// Feed `engine` from `rx`, with a tick every second.
pub async fn run(mut engine: Engine, mut rx: mpsc::UnboundedReceiver<Input>) {
    let mut ticks = tokio::time::interval(std::time::Duration::from_secs(1));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        let input = tokio::select! {
            input = rx.recv() => match input {
                Some(input) => input,
                None => return,
            },
            _ = ticks.tick() => Input::Tick,
        };
        engine.handle(input).await;
    }
}

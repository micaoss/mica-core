//! The BlueZ pairing agent micad registers: the passkey, confirmations and authorizations.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

use super::*;

/// A pairing waiting for the console's answer.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Pending {
    /// The device asking.
    pub address: String,
    /// The six digits BlueZ wants compared on both ends; `None` for a peer
    /// with no display, which has nothing to compare.
    pub passkey: Option<u32>,
    /// Seconds since the epoch when the request arrived, so the console can
    /// show how long it has been waiting.
    pub since: u64,
}

/// The pairing window: while it is open the adapter is discoverable and
/// pairable, and a request waits for the console; otherwise it is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    /// Seconds since the epoch when it closes by itself.
    pub expires_at: u64,
    /// Which opening this is, so the timer of an earlier one cannot close a
    /// later one.
    generation: u64,
}

/// How long a pairing window stays open.
pub const PAIR_WINDOW: Duration = Duration::from_secs(180);

/// The pairing agent's shared state.
///
/// Held by both the agent object BlueZ calls and the bus members the console
/// calls, which is the whole reason it exists: the agent blocks on a decision
/// that arrives through a different door.
#[derive(Default)]
pub struct Agent {
    pub(super) pending: tokio::sync::Mutex<Vec<(Pending, tokio::sync::oneshot::Sender<bool>)>>,
    window: tokio::sync::Mutex<Option<Window>>,
    openings: std::sync::atomic::AtomicU64,
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

impl Agent {
    /// What is waiting, oldest first.
    pub async fn pending(&self) -> Vec<Pending> {
        self.pending
            .lock()
            .await
            .iter()
            .map(|(pending, _)| pending.clone())
            .collect()
    }

    /// The open window, if any.
    pub async fn window(&self) -> Option<Window> {
        *self.window.lock().await
    }

    /// Open the window for [`PAIR_WINDOW`], or open it afresh.
    pub async fn open_window(&self) -> Window {
        let window = Window {
            expires_at: now_seconds() + PAIR_WINDOW.as_secs(),
            generation: self
                .openings
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        };
        *self.window.lock().await = Some(window);
        window
    }

    /// Close the window, and refuse every request still waiting.
    pub async fn close_window(&self) {
        *self.window.lock().await = None;
        for (_, reply) in self.pending.lock().await.drain(..) {
            let _ = reply.send(false);
        }
    }

    /// Close `window` when it is still the open one: what its timer does.
    /// `false` when it was closed or opened afresh since.
    pub async fn expire(&self, window: Window) -> bool {
        if *self.window.lock().await != Some(window) {
            return false;
        }
        self.close_window().await;
        true
    }

    /// Answer the request of `address`. `false` when none is waiting.
    pub async fn answer(&self, address: &str, accept: bool) -> bool {
        let mut pending = self.pending.lock().await;
        let Some(index) = pending
            .iter()
            .position(|(pending, _)| pending.address.eq_ignore_ascii_case(address))
        else {
            return false;
        };
        let (_, reply) = pending.remove(index);
        reply.send(accept).is_ok()
    }

    /// Record a request and wait for the console, or for the bound.
    ///
    /// Refused at once while the window is closed: nobody is looking. A second
    /// request from a device already waiting is refused too, so one device is
    /// one row.
    pub async fn confirm(&self, address: String, passkey: Option<u32>) -> bool {
        if self.window.lock().await.is_none() {
            return false;
        }
        let (reply, answer) = tokio::sync::oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending
                .iter()
                .any(|(pending, _)| pending.address == address)
            {
                return false;
            }
            pending.push((
                Pending {
                    address: address.clone(),
                    passkey,
                    since: now_seconds(),
                },
                reply,
            ));
        }
        let accepted = tokio::time::timeout(CONFIRM_TIMEOUT, answer)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false);
        // Taken out whichever way it ended, including the timeout: a request
        // nobody answered must not stay on the console.
        self.pending
            .lock()
            .await
            .retain(|(pending, _)| pending.address != address);
        accepted
    }
}

/// How long a pairing confirmation waits for the console.
///
/// BlueZ gives its agent about a minute; this is shorter, so the refusal that
/// reaches the peer is micad's own and says so.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(45);

/// The `org.bluez.Agent1` micad registers.
pub struct PairingAgent {
    pub state: Arc<Agent>,
}

#[zbus::interface(name = "org.bluez.Agent1")]
impl PairingAgent {
    /// BlueZ dropped the agent.
    pub(super) fn release(&self) {
        tracing::info!("bluetooth: the pairing agent was released");
    }

    /// A legacy peer asked for a code to type. Refused: a fixed code is not
    /// offered, and a headless appliance has no keypad for a peer's.
    pub(super) fn request_pin_code(
        &self,
        device: zbus::ObjectPath<'_>,
    ) -> zbus::fdo::Result<String> {
        tracing::info!(device = %device, "bluetooth: refusing a PIN request");
        Err(rejected("a fixed pairing code is not offered"))
    }

    /// The same, as a number.
    pub(super) fn request_passkey(&self, device: zbus::ObjectPath<'_>) -> zbus::fdo::Result<u32> {
        tracing::info!(device = %device, "bluetooth: refusing a passkey request");
        Err(rejected("a fixed pairing code is not offered"))
    }

    /// The peer is showing a code; nothing to decide.
    pub(super) fn display_pin_code(&self, device: zbus::ObjectPath<'_>, pincode: String) {
        tracing::info!(device = %device, pincode, "bluetooth: peer displays a PIN");
    }

    /// The same for a passkey.
    pub(super) fn display_passkey(&self, device: zbus::ObjectPath<'_>, passkey: u32, entered: u16) {
        tracing::info!(device = %device, passkey, entered, "bluetooth: peer displays a passkey");
    }

    /// The decision this agent exists for.
    pub(super) async fn request_confirmation(
        &self,
        device: zbus::ObjectPath<'_>,
        passkey: u32,
    ) -> zbus::fdo::Result<()> {
        let address = address_of(&device);
        if self.state.confirm(address, Some(passkey)).await {
            Ok(())
        } else {
            Err(rejected("the pairing was not confirmed"))
        }
    }

    /// A peer with no display asking to be let in. Same decision, no passkey
    /// to show, so it is presented as one with none.
    pub(super) async fn request_authorization(
        &self,
        device: zbus::ObjectPath<'_>,
    ) -> zbus::fdo::Result<()> {
        let address = address_of(&device);
        if self.state.confirm(address, None).await {
            Ok(())
        } else {
            Err(rejected("the pairing was not confirmed"))
        }
    }

    /// A paired device asking to use a profile. Allowed: it is already paired,
    /// and refusing here would pair devices that then cannot do anything.
    pub(super) fn authorize_service(
        &self,
        device: zbus::ObjectPath<'_>,
        uuid: String,
    ) -> zbus::fdo::Result<()> {
        tracing::info!(device = %device, uuid, "bluetooth: authorising a service on a paired device");
        Ok(())
    }

    /// BlueZ gave up on the request.
    pub(super) fn cancel(&self) {
        tracing::info!("bluetooth: the pairing request was cancelled");
    }
}

/// The refusal BlueZ passes on to the peer.
fn rejected(why: &str) -> zbus::fdo::Error {
    zbus::fdo::Error::Failed(format!("org.bluez.Error.Rejected: {why}"))
}

/// The address inside a BlueZ device object path.
///
/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` is the only shape BlueZ uses, so
/// the address is the last segment with its underscores turned back.
#[must_use]
pub fn address_of(path: &zbus::ObjectPath<'_>) -> String {
    path.as_str()
        .rsplit('/')
        .next()
        .and_then(|segment| segment.strip_prefix("dev_"))
        .map(|address| address.replace('_', ":"))
        .unwrap_or_default()
}

/// Where micad publishes its agent.
pub const AGENT_PATH: &str = "/com/mica/bluetooth/agent";

/// What the agent tells BlueZ it can do.
///
/// `DisplayYesNo`: it can show a passkey and take a yes or no, which is the
/// console's pairing dialog.
pub const AGENT_CAPABILITY: &str = "DisplayYesNo";

/// Register the agent with BlueZ and make it the default.
///
/// # Errors
///
/// Returns the failure when BlueZ is not running or refuses the registration.
pub async fn register_agent(connection: &zbus::Connection) -> Result<()> {
    let proxy = AgentManager1Proxy::new(connection).await?;
    let path = zbus::ObjectPath::try_from(AGENT_PATH)?;
    proxy.register_agent(&path, AGENT_CAPABILITY).await?;
    proxy.request_default_agent(&path).await?;
    Ok(())
}

/// The bus name bluetoothd owns.
const BLUEZ_NAME: &str = "org.bluez";

/// Keep the agent registered for as long as the connection lives: now, when
/// BlueZ is already running, and again each time `org.bluez` gains an owner.
///
/// Once is not enough. Bluetooth is off until the settings turn it on, so
/// bluetoothd usually starts long after micad does, and it forgets every agent
/// when it restarts; a device with no agent answers no pairing at all.
pub async fn keep_agent_registered(connection: zbus::Connection, agent: Arc<Agent>) {
    use std::future::poll_fn;
    use std::pin::pin;
    use zbus::export::futures_core::Stream;

    // Subscribed before the first attempt, so a bluetoothd that starts between
    // the two is not missed.
    let stream = async {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")
            .interface("org.freedesktop.DBus")
            .member("NameOwnerChanged")
            .arg(0, BLUEZ_NAME)
            .build()?;
        zbus::MessageStream::for_match_rule(rule, &connection, None).await
    }
    .await;
    register_and_report(&connection, &agent).await;
    let stream = match stream {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(error = %err, "bluetooth: could not watch for bluetoothd; the agent is registered only if it was running");
            return;
        }
    };
    let mut stream = pin!(stream);
    while let Some(message) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        let Ok(message) = message else {
            continue;
        };
        // `sss`, with an empty new owner when the name is given up.
        let Ok((_name, _old, new_owner)) = message.body().deserialize::<(String, String, String)>()
        else {
            continue;
        };
        if !new_owner.is_empty() {
            register_and_report(&connection, &agent).await;
        }
    }
}

async fn register_and_report(connection: &zbus::Connection, agent: &Agent) {
    match register_agent(connection).await {
        Ok(()) => {
            tracing::info!(path = AGENT_PATH, "Bluetooth pairing agent registered");
            // A bluetoothd that starts is pairable by its own default; outside
            // a window this device is neither pairable nor discoverable.
            if agent.window().await.is_none()
                && let Err(err) = BlueZ.set_pair_window(false).await
            {
                tracing::info!(error = %format!("{err:#}"), "bluetooth: the adapter could not be closed to pairing");
            }
        }
        Err(err) => {
            tracing::info!(error = %err, "no Bluetooth agent registered: bluetoothd did not answer");
        }
    }
}

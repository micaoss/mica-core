//! The BlueZ pairing agent micad registers: the passkey, confirmations and authorizations.

use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

use super::*;

/// What a pairing is waiting for, and how the console answers it.
///
/// One at a time. BlueZ can only have one pairing in flight per adapter, and a
/// queue of confirmations would be a queue of decisions an operator cannot
/// tell apart.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Pending {
    /// The device asking.
    pub address: String,
    /// The six digits BlueZ wants confirmed on both ends.
    pub passkey: u32,
    /// Seconds since the epoch when the request arrived, so the console can
    /// show how long it has been waiting.
    pub since: u64,
}

/// The pairing agent's shared state.
///
/// Held by both the agent object BlueZ calls and the bus members the console
/// calls, which is the whole reason it exists: the agent blocks on a decision
/// that arrives through a different door.
#[derive(Default)]
pub struct Agent {
    pub(super) pending: tokio::sync::Mutex<Option<(Pending, tokio::sync::oneshot::Sender<bool>)>>,
    /// The code a legacy peer is answered with, kept in step with the
    /// settings tree by the reconciler's own pass.
    pub(super) pin: tokio::sync::RwLock<String>,
}

impl Agent {
    /// An agent answering `pin`.
    #[cfg(test)]
    pub fn with_pin(pin: String) -> Self {
        Self {
            pending: tokio::sync::Mutex::new(None),
            pin: tokio::sync::RwLock::new(pin),
        }
    }

    /// Replace the code a legacy peer is answered with.
    pub async fn set_pin(&self, pin: String) {
        *self.pin.write().await = pin;
    }

    /// The code, for the agent and for the console that displays it.
    pub async fn pin(&self) -> String {
        self.pin.read().await.clone()
    }

    /// What is waiting, if anything.
    pub async fn pending(&self) -> Option<Pending> {
        self.pending
            .lock()
            .await
            .as_ref()
            .map(|(pending, _)| pending.clone())
    }

    /// Answer the pending request. `false` when nothing is waiting or the
    /// address does not match what is.
    pub async fn answer(&self, address: &str, accept: bool) -> bool {
        let mut slot = self.pending.lock().await;
        let matches = slot
            .as_ref()
            .is_some_and(|(pending, _)| pending.address == address);
        if !matches {
            return false;
        }
        let Some((_, reply)) = slot.take() else {
            return false;
        };
        reply.send(accept).is_ok()
    }

    /// Record a request and wait for the console, or for the bound.
    ///
    /// A second request while one is pending is **refused**, not queued: two
    /// passkeys on one screen is two decisions an operator cannot tell apart.
    pub async fn confirm(&self, address: String, passkey: u32) -> bool {
        let (reply, answer) = tokio::sync::oneshot::channel();
        {
            let mut slot = self.pending.lock().await;
            if slot.is_some() {
                return false;
            }
            *slot = Some((
                Pending {
                    address,
                    passkey,
                    since: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |since| since.as_secs()),
                },
                reply,
            ));
        }
        let accepted = tokio::time::timeout(CONFIRM_TIMEOUT, answer)
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(false);
        // The slot is cleared whichever way it ended, including the timeout:
        // a pending request nobody answered must not block the next one.
        *self.pending.lock().await = None;
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

    /// A legacy peer asked for a code to type.
    ///
    /// Answered with the device's own PIN, which is a setting and is displayed
    /// in the console: a headless appliance has no keypad to enter a
    /// peer-chosen code on, so the alternative is refusing every legacy peer.
    pub(super) async fn request_pin_code(&self, device: zbus::zvariant::ObjectPath<'_>) -> String {
        let pin = self.state.pin().await;
        tracing::info!(device = %device, "bluetooth: answering a PIN request");
        pin
    }

    /// The same, as a number.
    pub(super) async fn request_passkey(&self, device: zbus::zvariant::ObjectPath<'_>) -> u32 {
        let pin = self.state.pin().await;
        tracing::info!(device = %device, "bluetooth: answering a passkey request");
        pin.parse().unwrap_or(0)
    }

    /// The peer is showing a code; nothing to decide.
    pub(super) fn display_pin_code(&self, device: zbus::zvariant::ObjectPath<'_>, pincode: String) {
        tracing::info!(device = %device, pincode, "bluetooth: peer displays a PIN");
    }

    /// The same for a passkey.
    pub(super) fn display_passkey(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
        passkey: u32,
        entered: u16,
    ) {
        tracing::info!(device = %device, passkey, entered, "bluetooth: peer displays a passkey");
    }

    /// The decision this agent exists for.
    pub(super) async fn request_confirmation(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
        passkey: u32,
    ) -> zbus::fdo::Result<()> {
        let address = address_of(&device);
        if self.state.confirm(address, passkey).await {
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed(
                "org.bluez.Error.Rejected: the pairing was not confirmed".to_string(),
            ))
        }
    }

    /// A peer with no display asking to be let in. Same decision, no passkey
    /// to show, so it is presented as one with none.
    pub(super) async fn request_authorization(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
    ) -> zbus::fdo::Result<()> {
        let address = address_of(&device);
        if self.state.confirm(address, 0).await {
            Ok(())
        } else {
            Err(zbus::fdo::Error::Failed(
                "org.bluez.Error.Rejected: the pairing was not confirmed".to_string(),
            ))
        }
    }

    /// A paired device asking to use a profile. Allowed: it is already paired,
    /// and refusing here would pair devices that then cannot do anything.
    pub(super) fn authorize_service(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
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

/// The address inside a BlueZ device object path.
///
/// `/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF` is the only shape BlueZ uses, so
/// the address is the last segment with its underscores turned back.
#[must_use]
pub fn address_of(path: &zbus::zvariant::ObjectPath<'_>) -> String {
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
/// console's pairing dialog. It also answers a legacy PIN request, which no
/// capability string covers.
pub const AGENT_CAPABILITY: &str = "DisplayYesNo";

/// Register the agent with BlueZ and make it the default.
///
/// # Errors
///
/// Returns the failure when BlueZ is not running or refuses the registration.
pub async fn register_agent(connection: &zbus::Connection) -> Result<()> {
    let proxy = AgentManager1Proxy::new(connection).await?;
    let path = zbus::zvariant::ObjectPath::try_from(AGENT_PATH)?;
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
pub async fn keep_agent_registered(connection: zbus::Connection) {
    use std::future::poll_fn;
    use std::pin::pin;
    use zbus::export::futures_core::Stream;

    // Subscribed before the first attempt, so a bluetoothd that starts between
    // the two is not missed.
    let stream = async {
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg(0, BLUEZ_NAME)?
            .build();
        zbus::MessageStream::for_match_rule(rule, &connection, None).await
    }
    .await;
    register_and_report(&connection).await;
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
            register_and_report(&connection).await;
        }
    }
}

async fn register_and_report(connection: &zbus::Connection) {
    match register_agent(connection).await {
        Ok(()) => tracing::info!(path = AGENT_PATH, "Bluetooth pairing agent registered"),
        Err(err) => {
            tracing::info!(error = %err, "no Bluetooth agent registered: bluetoothd did not answer");
        }
    }
}

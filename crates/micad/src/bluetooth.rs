//! The Bluetooth adapter, its devices, and the agent that pairs them.
//!
//! BlueZ is the device's Bluetooth stack: `bluetoothd` owns the adapter and
//! publishes it on the system bus as `org.bluez`. micad does not reimplement
//! any of that. What it adds is the management-plane half:
//!
//! - **declared** trust lives in the settings tree (`bluetooth.devices`), so a
//!   paired device survives a reboot and a reset can take it away;
//! - **observed** state is read from BlueZ and never merged with the
//!   declaration -- a paired phone that is switched off and a phone in range
//!   that nobody paired are different facts;
//! - **pairing** is an action with an agent behind it, because it is a
//!   negotiation with a person in the middle and a reconcile pass is neither.
//!
//! Everything here is a trait with an "unsupported" default, like every other
//! observer in this daemon: a board with no radio, a dry run and a test never
//! reach the bus.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

/// How long one BlueZ call may take.
///
/// Pairing is the long one and it has its own bound below; everything else
/// here is a property read or a property write.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// How long a pairing attempt is given.
///
/// BlueZ's own default is around a minute; the bound is here so a request that
/// never completes cannot hold the action open for longer than an operator
/// will wait.
pub const PAIR_TIMEOUT: Duration = Duration::from_secs(60);

/// The adapter, as BlueZ reports it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Adapter {
    /// The adapter's own address.
    pub address: String,
    /// The name it advertises.
    pub alias: String,
    pub powered: bool,
    pub discoverable: bool,
    pub discovering: bool,
}

/// One device BlueZ knows about, in range or not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Device {
    pub address: String,
    /// What it calls itself; empty when it offers no name.
    pub name: String,
    pub paired: bool,
    pub trusted: bool,
    pub blocked: bool,
    pub connected: bool,
    /// Signal strength while it is in range.
    pub rssi: Option<i16>,
}

/// What micad can ask BlueZ to do.
///
/// Every method has an "unsupported" default so a board with no adapter, a dry
/// run and a test answer the same way: this device does not do Bluetooth. A
/// default that succeeded silently would report a paired device on a board
/// with no radio.
#[async_trait::async_trait]
pub trait BluetoothControl: Send + Sync {
    /// The adapter, or the reason there is none.
    async fn adapter(&self) -> Result<Adapter> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Every device BlueZ holds.
    async fn devices(&self) -> Result<Vec<Device>> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Power the adapter on or off.
    async fn set_powered(&self, _on: bool) -> Result<()> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Make the adapter answer scans, or stop answering them.
    async fn set_discoverable(&self, _on: bool) -> Result<()> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Set the name the adapter advertises.
    async fn set_alias(&self, _alias: &str) -> Result<()> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Start or stop a scan.
    async fn set_discovery(&self, _on: bool) -> Result<()> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Pair with a device, under [`PAIR_TIMEOUT`].
    async fn pair(&self, _address: &str) -> Result<()> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Set a device's trust, so it may reconnect without being confirmed.
    async fn set_trusted(&self, _address: &str, _trusted: bool) -> Result<()> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Set a device's block.
    async fn set_blocked(&self, _address: &str, _blocked: bool) -> Result<()> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }

    /// Drop a device from the adapter entirely, keys included.
    async fn remove(&self, _address: &str) -> Result<()> {
        anyhow::bail!("this device has no Bluetooth adapter")
    }
}

/// A device with no adapter: the default every build starts from.
pub struct NoAdapter;

impl BluetoothControl for NoAdapter {}

/// BlueZ's object tree, read through its object manager.
#[zbus::proxy(
    interface = "org.freedesktop.DBus.ObjectManager",
    default_service = "org.bluez",
    default_path = "/"
)]
trait ObjectManager {
    fn get_managed_objects(
        &self,
    ) -> zbus::Result<
        std::collections::HashMap<
            zbus::zvariant::OwnedObjectPath,
            std::collections::HashMap<
                String,
                std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            >,
        >,
    >;
}

/// One adapter.
#[zbus::proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
trait Adapter1 {
    fn start_discovery(&self) -> zbus::Result<()>;
    fn stop_discovery(&self) -> zbus::Result<()>;
    fn remove_device(&self, device: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    #[zbus(property)]
    fn address(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn alias(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn set_alias(&self, value: &str) -> zbus::Result<()>;
    #[zbus(property)]
    fn powered(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_powered(&self, value: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn discoverable(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn set_discoverable(&self, value: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn discovering(&self) -> zbus::Result<bool>;
}

/// One device the adapter knows.
#[zbus::proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait Device1 {
    fn pair(&self) -> zbus::Result<()>;

    #[zbus(property)]
    fn set_trusted(&self, value: bool) -> zbus::Result<()>;
    #[zbus(property)]
    fn set_blocked(&self, value: bool) -> zbus::Result<()>;
}

/// Where a pairing agent registers itself.
#[zbus::proxy(
    interface = "org.bluez.AgentManager1",
    default_service = "org.bluez",
    default_path = "/org/bluez"
)]
trait AgentManager1 {
    fn register_agent(
        &self,
        agent: &zbus::zvariant::ObjectPath<'_>,
        capability: &str,
    ) -> zbus::Result<()>;
    fn request_default_agent(&self, agent: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;
}

/// The adapter BlueZ publishes.
///
/// One adapter and not a chosen one: an appliance has a radio or it does not,
/// and picking between two would be a configuration nobody asked for. The
/// first `org.bluez.Adapter1` in the object tree is it.
pub struct BlueZ;

impl BlueZ {
    async fn connection() -> Result<zbus::Connection> {
        Ok(zbus::Connection::system().await?)
    }

    /// The object tree, once, under [`CALL_TIMEOUT`].
    async fn objects(
        connection: &zbus::Connection,
    ) -> Result<
        std::collections::HashMap<
            zbus::zvariant::OwnedObjectPath,
            std::collections::HashMap<
                String,
                std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            >,
        >,
    > {
        let proxy = ObjectManagerProxy::new(connection).await?;
        Ok(tokio::time::timeout(CALL_TIMEOUT, proxy.get_managed_objects()).await??)
    }

    /// The first adapter path in the tree, or the reason there is none.
    async fn adapter_path(
        connection: &zbus::Connection,
    ) -> Result<zbus::zvariant::OwnedObjectPath> {
        let objects = Self::objects(connection).await?;
        // Sorted by the path's own text: `OwnedObjectPath` has no ordering,
        // and "the first adapter" has to mean the same one on every pass or a
        // two-radio board would swap adapters between reconciles.
        let mut paths: Vec<_> = objects
            .into_iter()
            .filter(|(_, interfaces)| interfaces.contains_key("org.bluez.Adapter1"))
            .map(|(path, _)| path)
            .collect();
        paths.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        paths
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("bluetoothd reports no adapter on this device"))
    }

    /// The object path BlueZ gives a device, derived from its address.
    ///
    /// Derived and not searched for, because a device the adapter has not seen
    /// since it started has no object yet and the caller would get "no such
    /// device" for a device that is simply out of range.
    fn device_path(adapter: &zbus::zvariant::OwnedObjectPath, address: &str) -> Result<String> {
        anyhow::ensure!(
            micad_settings::is_bluetooth_address(address),
            "{address:?} is not a Bluetooth address"
        );
        Ok(format!(
            "{}/dev_{}",
            adapter.as_str(),
            address.to_uppercase().replace(':', "_")
        ))
    }

    async fn adapter_proxy(connection: &zbus::Connection) -> Result<Adapter1Proxy<'static>> {
        let path = Self::adapter_path(connection).await?;
        Ok(Adapter1Proxy::builder(connection)
            .path(path)?
            .build()
            .await?)
    }

    async fn device_proxy(
        connection: &zbus::Connection,
        address: &str,
    ) -> Result<Device1Proxy<'static>> {
        let adapter = Self::adapter_path(connection).await?;
        let path = Self::device_path(&adapter, address)?;
        Ok(Device1Proxy::builder(connection)
            .path(path)?
            .build()
            .await?)
    }
}

/// Read one property out of a managed-objects entry.
fn property<T: TryFrom<zbus::zvariant::OwnedValue>>(
    interface: &std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
    name: &str,
) -> Option<T> {
    interface.get(name)?.clone().try_into().ok()
}

#[async_trait::async_trait]
impl BluetoothControl for BlueZ {
    async fn adapter(&self) -> Result<Adapter> {
        let connection = Self::connection().await?;
        let proxy = Self::adapter_proxy(&connection).await?;
        Ok(Adapter {
            address: proxy.address().await.unwrap_or_default(),
            alias: proxy.alias().await.unwrap_or_default(),
            powered: proxy.powered().await.unwrap_or(false),
            discoverable: proxy.discoverable().await.unwrap_or(false),
            discovering: proxy.discovering().await.unwrap_or(false),
        })
    }

    async fn devices(&self) -> Result<Vec<Device>> {
        let connection = Self::connection().await?;
        let objects = Self::objects(&connection).await?;
        let mut devices: Vec<Device> = objects
            .values()
            .filter_map(|interfaces| {
                let device = interfaces.get("org.bluez.Device1")?;
                Some(Device {
                    address: property(device, "Address").unwrap_or_default(),
                    // `Alias` is what BlueZ shows; `Name` is what the device
                    // said. The alias falls back to the name, so reading the
                    // alias reads whichever exists.
                    name: property(device, "Alias")
                        .or_else(|| property(device, "Name"))
                        .unwrap_or_default(),
                    paired: property(device, "Paired").unwrap_or(false),
                    trusted: property(device, "Trusted").unwrap_or(false),
                    blocked: property(device, "Blocked").unwrap_or(false),
                    connected: property(device, "Connected").unwrap_or(false),
                    rssi: property(device, "RSSI"),
                })
            })
            .filter(|device| !device.address.is_empty())
            .collect();
        devices.sort_by(|left, right| left.address.cmp(&right.address));
        Ok(devices)
    }

    async fn set_powered(&self, on: bool) -> Result<()> {
        let connection = Self::connection().await?;
        Ok(Self::adapter_proxy(&connection)
            .await?
            .set_powered(on)
            .await?)
    }

    async fn set_discoverable(&self, on: bool) -> Result<()> {
        let connection = Self::connection().await?;
        Ok(Self::adapter_proxy(&connection)
            .await?
            .set_discoverable(on)
            .await?)
    }

    async fn set_alias(&self, alias: &str) -> Result<()> {
        let connection = Self::connection().await?;
        Ok(Self::adapter_proxy(&connection)
            .await?
            .set_alias(alias)
            .await?)
    }

    async fn set_discovery(&self, on: bool) -> Result<()> {
        let connection = Self::connection().await?;
        let proxy = Self::adapter_proxy(&connection).await?;
        if on {
            Ok(proxy.start_discovery().await?)
        } else {
            Ok(proxy.stop_discovery().await?)
        }
    }

    async fn pair(&self, address: &str) -> Result<()> {
        let connection = Self::connection().await?;
        let proxy = Self::device_proxy(&connection, address).await?;
        // The bound is the whole reason this is not just `proxy.pair()`: a
        // peer that stops answering mid-negotiation leaves BlueZ waiting, and
        // the action would never return.
        tokio::time::timeout(PAIR_TIMEOUT, proxy.pair())
            .await
            .map_err(|_| anyhow::anyhow!("the device did not finish pairing in time"))??;
        Ok(())
    }

    async fn set_trusted(&self, address: &str, trusted: bool) -> Result<()> {
        let connection = Self::connection().await?;
        Ok(Self::device_proxy(&connection, address)
            .await?
            .set_trusted(trusted)
            .await?)
    }

    async fn set_blocked(&self, address: &str, blocked: bool) -> Result<()> {
        let connection = Self::connection().await?;
        Ok(Self::device_proxy(&connection, address)
            .await?
            .set_blocked(blocked)
            .await?)
    }

    async fn remove(&self, address: &str) -> Result<()> {
        let connection = Self::connection().await?;
        let adapter_path = Self::adapter_path(&connection).await?;
        let device = Self::device_path(&adapter_path, address)?;
        let proxy = Adapter1Proxy::builder(&connection)
            .path(adapter_path)?
            .build()
            .await?;
        let path = zbus::zvariant::ObjectPath::try_from(device.as_str())?;
        Ok(proxy.remove_device(&path).await?)
    }
}

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
    pending: tokio::sync::Mutex<Option<(Pending, tokio::sync::oneshot::Sender<bool>)>>,
    /// The code a legacy peer is answered with, kept in step with the
    /// settings tree by the reconciler's own pass.
    pin: tokio::sync::RwLock<String>,
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
    fn release(&self) {
        tracing::info!("bluetooth: the pairing agent was released");
    }

    /// A legacy peer asked for a code to type.
    ///
    /// Answered with the device's own PIN, which is a setting and is displayed
    /// in the console: a headless appliance has no keypad to enter a
    /// peer-chosen code on, so the alternative is refusing every legacy peer.
    async fn request_pin_code(&self, device: zbus::zvariant::ObjectPath<'_>) -> String {
        let pin = self.state.pin().await;
        tracing::info!(device = %device, "bluetooth: answering a PIN request");
        pin
    }

    /// The same, as a number.
    async fn request_passkey(&self, device: zbus::zvariant::ObjectPath<'_>) -> u32 {
        let pin = self.state.pin().await;
        tracing::info!(device = %device, "bluetooth: answering a passkey request");
        pin.parse().unwrap_or(0)
    }

    /// The peer is showing a code; nothing to decide.
    fn display_pin_code(&self, device: zbus::zvariant::ObjectPath<'_>, pincode: String) {
        tracing::info!(device = %device, pincode, "bluetooth: peer displays a PIN");
    }

    /// The same for a passkey.
    fn display_passkey(&self, device: zbus::zvariant::ObjectPath<'_>, passkey: u32, entered: u16) {
        tracing::info!(device = %device, passkey, entered, "bluetooth: peer displays a passkey");
    }

    /// The decision this agent exists for.
    async fn request_confirmation(
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
    async fn request_authorization(
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
    fn authorize_service(
        &self,
        device: zbus::zvariant::ObjectPath<'_>,
        uuid: String,
    ) -> zbus::fdo::Result<()> {
        tracing::info!(device = %device, uuid, "bluetooth: authorising a service on a paired device");
        Ok(())
    }

    /// BlueZ gave up on the request.
    fn cancel(&self) {
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

/// The declared map and what BlueZ reports, each side named.
///
/// Never merged. A declared device the adapter has never heard of is one that
/// has not been in range since the adapter was last reset; a device the
/// adapter holds that the settings tree does not declare is one the reconciler
/// is about to remove. Merging them would lose both facts.
#[must_use]
pub fn observed_json(
    declared: &BTreeMap<String, micad_settings::PairedDevice>,
    adapter: Result<Adapter, String>,
    devices: Result<Vec<Device>, String>,
    pin: &str,
) -> Value {
    let adapter = match adapter {
        Ok(adapter) => json!({
            "available": true,
            "address": adapter.address,
            "alias": adapter.alias,
            "powered": adapter.powered,
            "discoverable": adapter.discoverable,
            "discovering": adapter.discovering,
        }),
        Err(detail) => json!({ "available": false, "detail": detail }),
    };
    let devices = match devices {
        Ok(devices) => json!({
            "available": true,
            "entries": devices
                .iter()
                .map(|device| json!({
                    "address": device.address,
                    "name": device.name,
                    "paired": device.paired,
                    "trusted": device.trusted,
                    "blocked": device.blocked,
                    "connected": device.connected,
                    "rssi": device.rssi,
                }))
                .collect::<Vec<_>>(),
        }),
        Err(detail) => json!({ "available": false, "detail": detail }),
    };
    json!({
        "declared": declared,
        // The code a legacy peer is answered with. Published because it has to
        // be typed on the other device, which is the whole of what it is for.
        "pin": pin,
        "adapter": adapter,
        "devices": devices,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

#[cfg(test)]
mod agent_tests {
    use super::*;

    /// The console's answer is what the agent returns to BlueZ.
    #[tokio::test]
    async fn a_confirmed_request_is_accepted_and_a_refused_one_is_not() {
        for accept in [true, false] {
            let agent = Arc::new(Agent::with_pin("4211".to_string()));
            let waiting = Arc::clone(&agent);
            let pairing = tokio::spawn(async move {
                waiting
                    .confirm("AA:BB:CC:DD:EE:01".to_string(), 123_456)
                    .await
            });

            // The request is visible while it waits: that is what the console
            // renders the dialog from.
            let pending = loop {
                if let Some(pending) = agent.pending().await {
                    break pending;
                }
                tokio::task::yield_now().await;
            };
            assert_eq!(pending.passkey, 123_456);
            assert!(agent.answer("AA:BB:CC:DD:EE:01", accept).await);

            assert_eq!(pairing.await.expect("the pairing task"), accept);
            assert!(agent.pending().await.is_none(), "the slot was not cleared");
        }
    }

    /// An answer for a device that is not the one waiting changes nothing: two
    /// dialogs cannot be told apart, so the address has to match.
    #[tokio::test]
    async fn an_answer_for_another_device_is_ignored() {
        let agent = Arc::new(Agent::with_pin("4211".to_string()));
        let waiting = Arc::clone(&agent);
        let pairing =
            tokio::spawn(async move { waiting.confirm("AA:BB:CC:DD:EE:01".to_string(), 1).await });
        while agent.pending().await.is_none() {
            tokio::task::yield_now().await;
        }

        assert!(!agent.answer("AA:BB:CC:DD:EE:02", true).await);
        assert!(agent.pending().await.is_some(), "the request was consumed");

        assert!(agent.answer("AA:BB:CC:DD:EE:01", true).await);
        assert!(pairing.await.expect("the pairing task"));
    }

    /// A second request while one is pending is refused rather than queued.
    #[tokio::test]
    async fn a_second_request_is_refused_while_one_is_pending() {
        let agent = Arc::new(Agent::with_pin("4211".to_string()));
        let waiting = Arc::clone(&agent);
        let first =
            tokio::spawn(async move { waiting.confirm("AA:BB:CC:DD:EE:01".to_string(), 1).await });
        while agent.pending().await.is_none() {
            tokio::task::yield_now().await;
        }

        assert!(!agent.confirm("AA:BB:CC:DD:EE:02".to_string(), 2).await);

        agent.answer("AA:BB:CC:DD:EE:01", true).await;
        assert!(first.await.expect("the pairing task"));
    }

    /// The address is read out of the object path BlueZ names a device by.
    #[test]
    fn an_address_is_read_out_of_the_object_path() {
        let path = zbus::zvariant::ObjectPath::try_from("/org/bluez/hci0/dev_AA_BB_CC_DD_EE_FF")
            .expect("a device path");

        assert_eq!(address_of(&path), "AA:BB:CC:DD:EE:FF");
    }

    /// The PIN the agent answers with is the one it was last given.
    #[tokio::test]
    async fn the_pin_is_whatever_the_settings_last_said() {
        let agent = Agent::with_pin("0000".to_string());
        assert_eq!(agent.pin().await, "0000");

        agent.set_pin("4211".to_string()).await;

        assert_eq!(agent.pin().await, "4211");
    }
}

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
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

mod agent;
pub use agent::*;

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
mod tests;

//! The provisioning service on BlueZ: the GATT application and the LE
//! advertisement micad serves on the system bus and registers with
//! `bluetoothd`, the links it watches, and the default route.
//!
//! The application is registered while a window is open or a phone is
//! connected, the advertisement while a window is open and no phone is. The
//! advertisement makes the LE advertising data discoverable; it never makes
//! the adapter discoverable or pairable.

use std::collections::HashMap;
use std::future::poll_fn;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, mpsc, oneshot};
use zbus::export::futures_core::Stream;
use zbus::{ObjectPath, OwnedObjectPath, OwnedVariant};

use super::engine::{Clock, Input, Notify, Radio, Refused, Written};
use super::{COMMAND_UUID, EVENT_UUID, INFO_UUID, SERVICE_UUID, SESSION_UUID};

/// The GATT application's root.
pub const APP_PATH: &str = "/com/mica/provisioning";
const SERVICE_PATH: &str = "/com/mica/provisioning/service0";
const INFO_PATH: &str = "/com/mica/provisioning/service0/char0";
const SESSION_PATH: &str = "/com/mica/provisioning/service0/char1";
const COMMAND_PATH: &str = "/com/mica/provisioning/service0/char2";
const EVENT_PATH: &str = "/com/mica/provisioning/service0/char3";
/// The advertisement, outside the application's tree.
pub const ADVERT_PATH: &str = "/com/mica/provisioning_advert";
const BLUEZ: &str = "org.bluez";
/// How long one registration may take.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// How often the default route is looked at.
const ROUTE_POLL: Duration = Duration::from_secs(2);

/// What a characteristic answers BlueZ with.
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.bluez.Error")]
pub enum GattError {
    #[zbus(error)]
    ZBus(zbus::Error),
    /// Another phone holds the session.
    InProgress(String),
    /// No window is open, or the characteristic does not do that.
    NotPermitted(String),
    Failed(String),
}

impl From<Refused> for GattError {
    fn from(refused: Refused) -> Self {
        match refused {
            Refused::Busy => Self::InProgress("another phone is provisioning this device".into()),
            Refused::Closed => Self::NotPermitted("no provisioning window is open".into()),
        }
    }
}

/// The primary service.
pub struct Service;

#[zbus::interface(name = "org.bluez.GattService1")]
impl Service {
    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        SERVICE_UUID.to_string()
    }

    #[zbus(property)]
    fn primary(&self) -> bool {
        true
    }
}

/// Which of the four a characteristic is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Info,
    Session,
    Command,
    Event,
}

/// One characteristic.
pub struct Characteristic {
    kind: Kind,
    engine: mpsc::UnboundedSender<Input>,
    value: Vec<u8>,
    notifying: bool,
}

/// The requesting device's object path, the ATT MTU and the read offset.
fn request(options: &HashMap<String, OwnedVariant>) -> (String, Option<u16>, usize) {
    let device = options
        .get("device")
        .and_then(|value| ObjectPath::try_from(value.clone()).ok())
        .map(|path| path.to_string())
        .unwrap_or_default();
    let number = |key: &str| {
        options
            .get(key)
            .and_then(|value| u16::try_from(value.clone()).ok())
    };
    (
        device,
        number("mtu"),
        number("offset").map_or(0, usize::from),
    )
}

#[zbus::interface(name = "org.bluez.GattCharacteristic1")]
impl Characteristic {
    async fn read_value(
        &self,
        options: HashMap<String, OwnedVariant>,
    ) -> Result<Vec<u8>, GattError> {
        if self.kind != Kind::Info {
            return Err(GattError::NotPermitted("not readable".into()));
        }
        let (device, mtu, offset) = request(&options);
        let (reply, answer) = oneshot::channel();
        self.engine
            .send(Input::Read { device, mtu, reply })
            .map_err(|_| GattError::Failed("provisioning is not running".into()))?;
        let info = answer
            .await
            .map_err(|_| GattError::Failed("provisioning is not running".into()))??;
        Ok(info.get(offset..).map(<[u8]>::to_vec).unwrap_or_default())
    }

    async fn write_value(
        &self,
        value: Vec<u8>,
        options: HashMap<String, OwnedVariant>,
    ) -> Result<(), GattError> {
        let characteristic = match self.kind {
            Kind::Session => Written::Session,
            Kind::Command => Written::Command,
            Kind::Info | Kind::Event => {
                return Err(GattError::NotPermitted("not writable".into()));
            }
        };
        let (device, mtu, _) = request(&options);
        let (reply, answer) = oneshot::channel();
        self.engine
            .send(Input::Write {
                device,
                mtu,
                characteristic,
                value,
                reply,
            })
            .map_err(|_| GattError::Failed("provisioning is not running".into()))?;
        Ok(answer
            .await
            .map_err(|_| GattError::Failed("provisioning is not running".into()))??)
    }

    fn start_notify(&mut self) -> Result<(), GattError> {
        if !matches!(self.kind, Kind::Session | Kind::Event) {
            return Err(GattError::NotPermitted("no notifications".into()));
        }
        self.notifying = true;
        Ok(())
    }

    fn stop_notify(&mut self) {
        self.notifying = false;
    }

    #[zbus(property, name = "UUID")]
    fn uuid(&self) -> String {
        match self.kind {
            Kind::Info => INFO_UUID,
            Kind::Session => SESSION_UUID,
            Kind::Command => COMMAND_UUID,
            Kind::Event => EVENT_UUID,
        }
        .to_string()
    }

    #[zbus(property)]
    fn service(&self) -> OwnedObjectPath {
        OwnedObjectPath::from(ObjectPath::from_static_str_unchecked(SERVICE_PATH))
    }

    #[zbus(property)]
    fn flags(&self) -> Vec<String> {
        let flags: &[&str] = match self.kind {
            Kind::Info => &["read"],
            Kind::Session => &["write", "notify"],
            Kind::Command => &["write"],
            Kind::Event => &["notify"],
        };
        flags.iter().map(|flag| (*flag).to_string()).collect()
    }

    #[zbus(property)]
    fn value(&self) -> Vec<u8> {
        self.value.clone()
    }

    #[zbus(property)]
    fn notifying(&self) -> bool {
        self.notifying
    }
}

/// The advertisement: the service UUID, and the hostname as the local name.
pub struct Advertisement {
    name: String,
}

#[zbus::interface(name = "org.bluez.LEAdvertisement1")]
impl Advertisement {
    fn release(&self) {
        tracing::info!("bluetooth: the provisioning advertisement was released");
    }

    #[zbus(property, name = "Type")]
    fn kind(&self) -> String {
        "peripheral".to_string()
    }

    #[zbus(property, name = "ServiceUUIDs")]
    fn service_uuids(&self) -> Vec<String> {
        vec![SERVICE_UUID.to_string()]
    }

    #[zbus(property)]
    fn local_name(&self) -> String {
        self.name.clone()
    }

    #[zbus(property)]
    fn discoverable(&self) -> bool {
        true
    }
}

#[zbus::proxy(interface = "org.bluez.GattManager1", default_service = "org.bluez")]
trait GattManager1 {
    fn register_application(
        &self,
        application: &ObjectPath<'_>,
        options: HashMap<&str, OwnedVariant>,
    ) -> zbus::Result<()>;
    fn unregister_application(&self, application: &ObjectPath<'_>) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.bluez.LEAdvertisingManager1",
    default_service = "org.bluez"
)]
trait LEAdvertisingManager1 {
    fn register_advertisement(
        &self,
        advertisement: &ObjectPath<'_>,
        options: HashMap<&str, OwnedVariant>,
    ) -> zbus::Result<()>;
    fn unregister_advertisement(&self, advertisement: &ObjectPath<'_>) -> zbus::Result<()>;
}

#[zbus::proxy(interface = "org.bluez.Device1", default_service = "org.bluez")]
trait Device1 {
    fn disconnect(&self) -> zbus::Result<()>;
}

/// Re-runs the Bluetooth reconciler, which starts or stops the adapter.
#[async_trait::async_trait]
pub trait Reconcile: Send + Sync + 'static {
    async fn bluetooth(&self);
}

#[derive(Default)]
struct Wanted {
    application: bool,
    advertisement: Option<String>,
}

/// [`Radio`] on BlueZ.
pub struct BluezRadio {
    connection: zbus::Connection,
    reconcile: Arc<dyn Reconcile>,
    wanted: Mutex<Wanted>,
}

impl BluezRadio {
    async fn adapter(&self) -> Option<OwnedObjectPath> {
        // The adapter appears a moment after bluetoothd starts.
        for _ in 0..10 {
            if let Ok(path) = crate::bluetooth::BlueZ::adapter_path(&self.connection).await {
                return Some(path);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        tracing::warn!("bluetooth: no adapter to provision over");
        None
    }

    async fn register_application(&self, adapter: &OwnedObjectPath) {
        let registered = async {
            let manager = GattManager1Proxy::builder(&self.connection)
                .path(adapter.clone())
                .build()
                .await?;
            manager
                .register_application(
                    &ObjectPath::from_static_str_unchecked(APP_PATH),
                    HashMap::new(),
                )
                .await
        };
        match tokio::time::timeout(CALL_TIMEOUT, registered).await {
            Ok(Ok(())) => tracing::info!("bluetooth: the provisioning service is registered"),
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "bluetooth: registering the provisioning service failed")
            }
            Err(_) => tracing::warn!("bluetooth: registering the provisioning service timed out"),
        }
    }

    async fn unregister_application(&self, adapter: &OwnedObjectPath) {
        let gone = async {
            GattManager1Proxy::builder(&self.connection)
                .path(adapter.clone())
                .build()
                .await?
                .unregister_application(&ObjectPath::from_static_str_unchecked(APP_PATH))
                .await
        };
        if let Ok(Err(err)) = tokio::time::timeout(CALL_TIMEOUT, gone).await {
            tracing::debug!(error = %err, "bluetooth: unregistering the provisioning service");
        }
    }

    async fn advertising_manager(
        &self,
        adapter: &OwnedObjectPath,
    ) -> zbus::Result<LEAdvertisingManager1Proxy<'static>> {
        LEAdvertisingManager1Proxy::builder(&self.connection)
            .path(adapter.clone())
            .build()
            .await
    }

    async fn register_advertisement(&self, adapter: &OwnedObjectPath, name: &str) {
        if let Ok(advert) = self
            .connection
            .object_server()
            .interface::<_, Advertisement>(ADVERT_PATH)
            .await
        {
            advert.get_mut().await.name = name.to_string();
        }
        let registered = async {
            self.advertising_manager(adapter)
                .await?
                .register_advertisement(
                    &ObjectPath::from_static_str_unchecked(ADVERT_PATH),
                    HashMap::new(),
                )
                .await
        };
        match tokio::time::timeout(CALL_TIMEOUT, registered).await {
            Ok(Ok(())) => tracing::info!(name, "bluetooth: advertising for provisioning"),
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "bluetooth: advertising for provisioning failed")
            }
            Err(_) => tracing::warn!("bluetooth: advertising for provisioning timed out"),
        }
    }

    async fn unregister_advertisement(&self, adapter: &OwnedObjectPath) {
        let gone = async {
            self.advertising_manager(adapter)
                .await?
                .unregister_advertisement(&ObjectPath::from_static_str_unchecked(ADVERT_PATH))
                .await
        };
        if let Ok(Err(err)) = tokio::time::timeout(CALL_TIMEOUT, gone).await {
            tracing::debug!(error = %err, "bluetooth: unregistering the advertisement");
        }
    }

    /// Register what is wanted again: bluetoothd forgets both when it
    /// restarts.
    async fn reregister(&self) {
        let wanted = self.wanted.lock().await;
        if !wanted.application {
            return;
        }
        let Some(adapter) = self.adapter().await else {
            return;
        };
        self.register_application(&adapter).await;
        if let Some(name) = &wanted.advertisement {
            self.register_advertisement(&adapter, name).await;
        }
    }
}

#[async_trait::async_trait]
impl Radio for BluezRadio {
    async fn advertise(&self, name: Option<&str>) {
        let mut wanted = self.wanted.lock().await;
        let Some(adapter) = self.adapter().await else {
            return;
        };
        if wanted.advertisement.is_some() {
            self.unregister_advertisement(&adapter).await;
        }
        if let Some(name) = name {
            self.register_advertisement(&adapter, name).await;
        }
        wanted.advertisement = name.map(str::to_string);
    }

    async fn hold(&self, on: bool) {
        let mut wanted = self.wanted.lock().await;
        wanted.application = on;
        if on {
            super::set_radio_held(true);
            self.reconcile.bluetooth().await;
            if let Some(adapter) = self.adapter().await {
                self.register_application(&adapter).await;
            }
        } else {
            if let Ok(adapter) = crate::bluetooth::BlueZ::adapter_path(&self.connection).await {
                self.unregister_application(&adapter).await;
            }
            super::set_radio_held(false);
            self.reconcile.bluetooth().await;
        }
    }

    async fn notify(&self, characteristic: Notify, fragment: Vec<u8>) {
        let path = match characteristic {
            Notify::Session => SESSION_PATH,
            Notify::Event => EVENT_PATH,
        };
        let Ok(object) = self
            .connection
            .object_server()
            .interface::<_, Characteristic>(path)
            .await
        else {
            return;
        };
        object.get_mut().await.value = fragment;
        if let Err(err) = object
            .get()
            .await
            .value_changed(object.signal_emitter())
            .await
        {
            tracing::warn!(error = %err, "bluetooth: a provisioning notification failed");
        }
    }

    async fn disconnect(&self, device: &str) {
        let Ok(path) = ObjectPath::try_from(device) else {
            return;
        };
        let disconnected = async {
            Device1Proxy::builder(&self.connection)
                .path(path)
                .build()
                .await?
                .disconnect()
                .await
        };
        if let Ok(Err(err)) = tokio::time::timeout(CALL_TIMEOUT, disconnected).await {
            tracing::warn!(error = %err, device, "bluetooth: disconnecting the phone failed");
        }
    }
}

/// The clock of this boot.
pub struct SystemClock;

impl Clock for SystemClock {
    fn uptime(&self) -> u64 {
        std::fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|text| text.split('.').next()?.parse().ok())
            .unwrap_or_default()
    }

    fn unix(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs())
    }
}

/// Serve the application and the advertisement on `connection`, and the
/// radio that registers them.
///
/// # Errors
/// An object could not be served.
pub async fn serve(
    connection: &zbus::Connection,
    engine: &mpsc::UnboundedSender<Input>,
    reconcile: Arc<dyn Reconcile>,
) -> zbus::Result<Arc<BluezRadio>> {
    let server = connection.object_server();
    server.at(APP_PATH, zbus::fdo::ObjectManager).await?;
    server.at(SERVICE_PATH, Service).await?;
    for (path, kind) in [
        (INFO_PATH, Kind::Info),
        (SESSION_PATH, Kind::Session),
        (COMMAND_PATH, Kind::Command),
        (EVENT_PATH, Kind::Event),
    ] {
        server
            .at(
                path,
                Characteristic {
                    kind,
                    engine: engine.clone(),
                    value: Vec::new(),
                    notifying: false,
                },
            )
            .await?;
    }
    server
        .at(
            ADVERT_PATH,
            Advertisement {
                name: String::new(),
            },
        )
        .await?;
    Ok(Arc::new(BluezRadio {
        connection: connection.clone(),
        reconcile,
        wanted: Mutex::new(Wanted::default()),
    }))
}

async fn signals(
    connection: &zbus::Connection,
    rule: zbus::Result<zbus::MatchRule<'static>>,
) -> Option<zbus::MessageStream> {
    let stream = async { zbus::MessageStream::for_match_rule(rule?, connection, None).await };
    match stream.await {
        Ok(stream) => Some(stream),
        Err(err) => {
            tracing::warn!(error = %err, "bluetooth: a provisioning watch could not subscribe");
            None
        }
    }
}

/// Tell the engine when a phone's link drops.
pub async fn watch_links(connection: zbus::Connection, engine: mpsc::UnboundedSender<Input>) {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(BLUEZ)
        .interface("org.freedesktop.DBus.Properties")
        .member("PropertiesChanged")
        .arg(0, "org.bluez.Device1")
        .build();
    let Some(stream) = signals(&connection, rule).await else {
        return;
    };
    let mut stream = pin!(stream);
    while let Some(message) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        let Ok(message) = message else {
            continue;
        };
        let Ok((_, changed, _)) =
            message
                .body()
                .deserialize::<(String, HashMap<String, OwnedVariant>, Vec<String>)>()
        else {
            continue;
        };
        let disconnected = changed
            .get("Connected")
            .and_then(|value| bool::try_from(value).ok())
            == Some(false);
        if let (true, Some(path)) = (disconnected, message.header().path()) {
            let _ = engine.send(Input::Disconnected {
                device: path.to_string(),
            });
        }
    }
}

/// Register again whenever bluetoothd comes back.
pub async fn keep_registered(connection: zbus::Connection, radio: Arc<BluezRadio>) {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.DBus")
        .interface("org.freedesktop.DBus")
        .member("NameOwnerChanged")
        .arg(0, BLUEZ)
        .build();
    let Some(stream) = signals(&connection, rule).await else {
        return;
    };
    let mut stream = pin!(stream);
    while let Some(message) = poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        let Ok(message) = message else {
            continue;
        };
        if let Ok((_, _, owner)) = message.body().deserialize::<(String, String, String)>()
            && !owner.is_empty()
        {
            radio.reregister().await;
        }
    }
}

/// Whether `/proc/net/route` (IPv4) names a default route.
#[must_use]
pub fn ipv4_default(table: &str) -> bool {
    table.lines().skip(1).any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.len() > 7 && fields[1] == "00000000" && fields[7] == "00000000"
    })
}

/// Whether `/proc/net/ipv6_route` names a default route.
#[must_use]
pub fn ipv6_default(table: &str) -> bool {
    table.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.len() > 9
            && fields[0] == "00000000000000000000000000000000"
            && fields[1] == "00"
            && fields[9] != "lo"
    })
}

/// Tell the engine whenever a default route comes or goes.
pub async fn watch_route(engine: mpsc::UnboundedSender<Input>) {
    let mut last = None;
    loop {
        let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();
        let present =
            ipv4_default(&read("/proc/net/route")) || ipv6_default(&read("/proc/net/ipv6_route"));
        if last != Some(present) {
            if engine.send(Input::Route(present)).is_err() {
                return;
            }
            last = Some(present);
        }
        tokio::time::sleep(ROUTE_POLL).await;
    }
}

/// Run BLE provisioning on `connection`: serve the objects, start the engine
/// with what the device is now, and watch the links, bluetoothd and the
/// default route.
///
/// # Errors
/// An object could not be served.
pub async fn start(
    connection: &zbus::Connection,
    device: Arc<dyn super::engine::Device>,
    reconcile: Arc<dyn Reconcile>,
    provisioning: Arc<super::Provisioning>,
) -> zbus::Result<()> {
    let Some(store) = provisioning.store().cloned() else {
        return Ok(());
    };
    let (tx, rx) = mpsc::unbounded_channel();
    let radio = serve(connection, &tx, reconcile).await?;
    let facts = device.facts().await;
    let audit = Arc::clone(&provisioning);
    let engine = super::engine::Engine::new(
        super::engine::Parts {
            device,
            radio: Arc::clone(&radio) as Arc<dyn Radio>,
            clock: Arc::new(SystemClock),
            store,
            audit: Arc::new(move |event: &str, outcome: &str| audit.audit(event, outcome)),
            wifi_locked: provisioning.wifi_lock(),
        },
        facts,
        tx.clone(),
    );
    provisioning.attach(tx.clone());
    tokio::spawn(super::engine::run(engine, rx));
    tokio::spawn(watch_links(connection.clone(), tx.clone()));
    tokio::spawn(keep_registered(connection.clone(), radio));
    tokio::spawn(watch_route(tx));
    Ok(())
}

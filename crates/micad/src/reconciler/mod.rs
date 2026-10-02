//! Reconciler contract shared by all micad reconcilers.

/// Visible to the daemon for the reason `network` is: the bus surfaces the
/// pairing actions, and they drive the unit this reconciler owns.
pub mod bluetooth;
/// Visible to the daemon for the reason `network` is: the container actions
/// the bus surfaces drive the unit name this reconciler renders.
pub mod container;
mod hostname;
pub use hostname::HostnameExecutor;
mod mqtt;
/// Visible to the daemon rather than to this module alone: the WireGuard key
/// rotation the bus surfaces ([`network::WireguardRotate`]) is this
/// reconciler's mechanism reached from outside a reconcile.
pub mod network;
mod sshd;
/// Visible to the daemon for the same reason: the container actions are
/// systemd verbs, and the bus needs the trait they are spoken through.
pub mod systemd;
mod time;
mod web;
mod wifi_ap;
mod wifi_client;

use micad_settings::Settings;

#[async_trait::async_trait]
pub trait Reconciler: Send + Sync {
    /// Stable name; also this reconciler's key in the live-state tree (e.g. "hostname", "network").
    fn name(&self) -> &'static str;
    /// Dot-path prefix of the settings subtree this reconciler watches (e.g. "hostname", "network").
    fn subtree(&self) -> &'static str;
    /// Apply `settings` to the system; return the applied live-state as JSON.
    async fn apply(&self, settings: &Settings) -> anyhow::Result<serde_json::Value>;
}

/// The reconcilers of the features the product carries: one whose subtree
/// belongs to a feature it does not carry is not registered, so nothing renders
/// that feature's files or drives its units.
pub fn for_features(
    features: &micad_settings::Features,
    init: micad_settings::Init,
) -> Vec<Box<dyn Reconciler>> {
    all(init)
        .into_iter()
        .filter(|reconciler| {
            micad_settings::Feature::owning(reconciler.subtree())
                .is_none_or(|feature| features.has(feature))
        })
        .collect()
}

/// All reconcilers compiled into micad with the production executors of
/// `init`.
///
/// Safe to call anywhere: executors connect to the system bus lazily and run
/// commands only when asked, so nothing touches the host until a
/// reconciler's `apply` runs.
pub fn all(init: micad_settings::Init) -> Vec<Box<dyn Reconciler>> {
    match init {
        micad_settings::Init::Systemd => systemd_reconcilers(),
        micad_settings::Init::Openrc => openrc_reconcilers(),
    }
}

/// An OpenRC root's: the same subtrees, through `rc-service`,
/// ifupdown, busybox ntpd and the OpenRC services of the option packages
/// (mica-ssh, mica-wifi, mica-wifi-ap, Bluetooth), which ship them.
fn openrc_reconcilers() -> Vec<Box<dyn Reconciler>> {
    use crate::openrc::{EtcHostname, OpenrcUnits};
    vec![
        Box::new(hostname::HostnameReconciler::new(EtcHostname::production())),
        Box::new(network::IfupdownReconciler::production()),
        Box::new(sshd::SshdReconciler::openrc(OpenrcUnits::production())),
        Box::new(wifi_client::WifiClientReconciler::openrc(
            OpenrcUnits::production(),
        )),
        Box::new(wifi_ap::WifiApReconciler::openrc(OpenrcUnits::production())),
        Box::new(mqtt::MqttReconciler::production(OpenrcUnits::production())),
        Box::new(container::ContainerReconciler::production(
            OpenrcUnits::production(),
        )),
        Box::new(time::TimeReconciler::openrc()),
        Box::new(web::WebReconciler::production(OpenrcUnits::production())),
        Box::new(bluetooth::BluetoothReconciler::openrc(
            OpenrcUnits::production(),
            std::sync::Arc::new(crate::bluetooth::BlueZ),
        )),
    ]
}

fn systemd_reconcilers() -> Vec<Box<dyn Reconciler>> {
    vec![
        Box::new(hostname::HostnameReconciler::new(
            hostname::Hostnamed::production(),
        )),
        Box::new(network::NetworkReconciler::production()),
        Box::new(sshd::SshdReconciler::production(systemd::Systemd::new())),
        Box::new(wifi_client::WifiClientReconciler::production()),
        Box::new(wifi_ap::WifiApReconciler::production()),
        Box::new(container::ContainerReconciler::production(
            systemd::Systemd::new(),
        )),
        Box::new(mqtt::MqttReconciler::production(systemd::Systemd::new())),
        Box::new(time::TimeReconciler::production()),
        Box::new(web::WebReconciler::production(systemd::Systemd::new())),
        // Last, and the only one whose subject is optional hardware: a board
        // with no radio reports `unsupported` rather than failing.
        Box::new(bluetooth::BluetoothReconciler::new(
            systemd::Systemd::new(),
            std::sync::Arc::new(crate::bluetooth::BlueZ),
        )),
    ]
}

#[cfg(test)]
mod feature_tests {
    use micad_settings::{Feature, Features};

    /// The reconcilers of a feature the product does not carry are not
    /// registered; the ones every product needs always are.
    #[test]
    fn only_the_features_the_product_carries_are_reconciled() {
        let subtrees = |features: &Features| {
            super::for_features(features, micad_settings::Init::Systemd)
                .iter()
                .map(|r| r.subtree())
                .collect::<Vec<_>>()
        };
        let all = subtrees(&Features::all());
        let minimal = subtrees(&Features::only(&[]));
        for always in ["hostname", "network", "time", "access.web"] {
            assert!(minimal.contains(&always), "{always}: {minimal:?}");
        }
        for gated in [
            "access.ssh",
            "wifi.client",
            "wifi",
            "container",
            "mqtt",
            "bluetooth",
        ] {
            assert!(all.contains(&gated), "{gated}: {all:?}");
            assert!(!minimal.contains(&gated), "{gated}: {minimal:?}");
        }
        let mqtt_only = subtrees(&Features::only(&[Feature::Mqtt]));
        assert!(mqtt_only.contains(&"mqtt"));
        assert!(!mqtt_only.contains(&"container"));
    }

    /// An OpenRC root reconciles what it can carry, under the same names.
    #[test]
    fn an_openrc_root_reconciles_its_features_under_the_same_names() {
        let subtrees: Vec<_> = super::for_features(&Features::all(), micad_settings::Init::Openrc)
            .iter()
            .map(|r| r.subtree())
            .collect();
        assert_eq!(
            subtrees,
            [
                "hostname",
                "network",
                "access.ssh",
                "wifi.client",
                "wifi",
                "mqtt",
                "container",
                "time",
                "access.web",
                "bluetooth"
            ]
        );
    }
}

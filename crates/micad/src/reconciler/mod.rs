//! Reconciler contract shared by all micad reconcilers.

/// Visible to the daemon for the reason `network` is: the bus surfaces the
/// pairing actions, and they drive the unit this reconciler owns.
pub mod bluetooth;
/// Visible to the daemon for the reason `network` is: the container actions
/// the bus surfaces drive the unit name this reconciler renders.
pub mod container;
mod hostname;
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
pub fn for_features(features: &micad_settings::Features) -> Vec<Box<dyn Reconciler>> {
    all()
        .into_iter()
        .filter(|reconciler| {
            micad_settings::Feature::owning(reconciler.subtree())
                .is_none_or(|feature| features.has(feature))
        })
        .collect()
}

/// All reconcilers compiled into micad with production executors.
///
/// Safe to call anywhere: executors connect to the system bus lazily, so
/// nothing touches the host until a reconciler's `apply` runs.
pub fn all() -> Vec<Box<dyn Reconciler>> {
    vec![
        Box::new(hostname::HostnameReconciler::new(
            hostname::Hostnamed::production(),
        )),
        Box::new(network::NetworkReconciler::production()),
        Box::new(sshd::SshdReconciler::production()),
        Box::new(wifi_client::WifiClientReconciler::production()),
        Box::new(wifi_ap::WifiApReconciler::production()),
        Box::new(container::ContainerReconciler::production()),
        Box::new(mqtt::MqttReconciler::production()),
        Box::new(time::TimeReconciler::production()),
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
            super::for_features(features)
                .iter()
                .map(|r| r.subtree())
                .collect::<Vec<_>>()
        };
        let all = subtrees(&Features::all());
        let minimal = subtrees(&Features::only(&[]));
        for always in ["hostname", "network", "time"] {
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
}

//! The uplink engine's run, fed the settings the service holds and telling
//! the bus of each change of the active uplink.

use std::sync::Arc;

use futures_util::FutureExt as _;
use zbus::object_server::InterfaceRef;

use super::MicadService;

/// Run `engine` for as long as the daemon runs, over `service`'s settings,
/// emitting `UplinkChanged` on each switch, with `dns` following it.
pub async fn uplink_upkeep(
    service: InterfaceRef<MicadService>,
    engine: crate::uplink::Shared,
    dns: Arc<dyn crate::uplink::dns::Follow>,
) {
    let inner = Arc::clone(&service.get().await.inner);
    let emitter = service.signal_emitter().clone();
    crate::uplink::run(
        engine,
        move || {
            let inner = Arc::clone(&inner);
            async move { inner.read().await.settings.network.clone() }.boxed()
        },
        move |change| {
            let emitter = emitter.clone();
            async move {
                let json = serde_json::to_string(&change).unwrap_or_default();
                if let Err(err) = MicadService::uplink_changed(&emitter, &json).await {
                    tracing::warn!(error = %err, "emit UplinkChanged failed");
                }
            }
            .boxed()
        },
        dns,
    )
    .await;
}

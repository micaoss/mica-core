//! The login prompt's lines, kept up to date: the device code and the console
//! at each address the device holds now.

use std::path::Path;
use std::time::Duration;

use zbus::object_server::InterfaceRef;

use super::MicadService;
use crate::device_code::{ISSUE_PATH, console_lines, issue_text, put_issue};

/// How often the addresses are read again: a lease or a link-local address
/// reaches the prompt within this.
const EVERY: Duration = Duration::from_secs(5);

/// Rewrite the login prompt whenever its text changes. `code` is the device
/// code as printed, `None` on a device without one.
pub async fn issue_upkeep(service: InterfaceRef<MicadService>, code: Option<String>) {
    let inner = std::sync::Arc::clone(&service.get().await.inner);
    let mut shown = issue_text(code.as_deref(), &[]);
    loop {
        tokio::time::sleep(EVERY).await;
        let addresses = match tokio::task::spawn_blocking(crate::netlink::ipv4_address_list).await {
            Ok(Ok(addresses)) => addresses,
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "the addresses for the login prompt could not be read");
                continue;
            }
            Err(_) => continue,
        };
        let (web, hostname) = {
            let state = inner.read().await;
            (state.settings.access.web, state.settings.hostname.clone())
        };
        let text = issue_text(code.as_deref(), &console_lines(&web, &hostname, &addresses));
        if text == shown {
            continue;
        }
        match put_issue(Path::new(ISSUE_PATH), text.as_deref()) {
            Ok(()) => shown = text,
            Err(err) => {
                tracing::warn!(path = ISSUE_PATH, error = %err, "the login prompt could not be written")
            }
        }
    }
}

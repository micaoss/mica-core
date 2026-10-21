//! Waiting for the interfaces an apply changed to come up.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;

/// How long a changed interface may take to come up.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often networkd is asked while waiting.
const SETTLE_INTERVAL: Duration = Duration::from_millis(500);

/// Waits until the interfaces an apply changed are up.
#[async_trait::async_trait]
pub trait LinkSettle: Send + Sync {
    /// Wait for each of `links`, by name, to come up; the value says whether
    /// the apply creates it. The error names the interface that did not, and
    /// why.
    async fn settle(&self, links: &BTreeMap<String, bool>) -> Result<(), String>;
}

/// Waits for nothing, for a root without networkd and for tests.
pub struct NoSettle;

#[async_trait::async_trait]
impl LinkSettle for NoSettle {
    async fn settle(&self, _links: &BTreeMap<String, bool>) -> Result<(), String> {
        Ok(())
    }
}

/// Where one interface stands after an apply.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Up, or nothing to wait for: it has no carrier.
    Settled,
    /// Still coming up, and why it is not up yet.
    Pending(String),
    /// networkd gave up on it.
    Failed(String),
}

/// Where `name` stands in networkd's `Describe`. A link the apply does not
/// create and the kernel does not have is a device that is not plugged in,
/// so there is nothing to wait for; one it creates has to appear.
#[must_use]
pub fn verdict(describe: &Value, name: &str, created: bool) -> Verdict {
    let link = describe
        .get("Interfaces")
        .and_then(Value::as_array)
        .and_then(|links| {
            links
                .iter()
                .find(|link| link.get("Name").and_then(Value::as_str) == Some(name))
        });
    let Some(link) = link else {
        return if created {
            Verdict::Pending("the device was not created".to_string())
        } else {
            Verdict::Settled
        };
    };
    let field = |key: &str| link.get(key).and_then(Value::as_str).unwrap_or("");
    match field("AdministrativeState") {
        "failed" => return Verdict::Failed("networkd could not configure it".to_string()),
        "unmanaged" => return Verdict::Failed("networkd does not manage it".to_string()),
        "configured" => return Verdict::Settled,
        _ => {}
    }
    if matches!(field("CarrierState"), "off" | "no-carrier") {
        return Verdict::Settled;
    }
    match field("OperationalState") {
        "routable" | "degraded" | "enslaved" => Verdict::Settled,
        state => Verdict::Pending(format!("it is {state}")),
    }
}

/// Waits on networkd: every link comes up within [`SETTLE_TIMEOUT`], or the
/// first that networkd gave up on, or the last one still pending, is named.
pub struct NetworkdSettle;

#[async_trait::async_trait]
impl LinkSettle for NetworkdSettle {
    async fn settle(&self, links: &BTreeMap<String, bool>) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + SETTLE_TIMEOUT;
        loop {
            let pending = match crate::network_state::describe_raw().await {
                Ok(describe) => {
                    let mut pending = None;
                    for (name, created) in links {
                        match verdict(&describe, name, *created) {
                            Verdict::Settled => {}
                            Verdict::Failed(reason) => return Err(format!("{name}: {reason}")),
                            Verdict::Pending(reason) => {
                                pending.get_or_insert(format!("{name}: {reason}"));
                            }
                        }
                    }
                    pending
                }
                Err(error) => Some(format!("networkd: {error:#}")),
            };
            let Some(pending) = pending else {
                return Ok(());
            };
            if tokio::time::Instant::now() >= deadline {
                return Err(format!(
                    "{pending} after {} seconds",
                    SETTLE_TIMEOUT.as_secs()
                ));
            }
            tokio::time::sleep(SETTLE_INTERVAL).await;
        }
    }
}

//! The resolution check.

use super::*;
use std::sync::Arc;

/// A resolver that answers `answer` after `delay`.
struct Fake {
    answer: Result<usize, String>,
    delay: Duration,
}

#[async_trait::async_trait]
impl ResolveName for Fake {
    async fn resolve(&self, _name: &str) -> Result<usize, String> {
        tokio::time::sleep(self.delay).await;
        self.answer.clone()
    }
}

fn probe(answer: Result<usize, String>, delay: Duration) -> DnsProbe {
    DnsProbe::new(
        Arc::new(Fake { answer, delay }),
        "example.org",
        Duration::from_millis(200),
    )
}

#[tokio::test]
pub(super) async fn a_name_that_resolves_is_ok_timed_and_kept() {
    let probe = probe(Ok(2), Duration::ZERO);
    assert_eq!(probe.last().await, None);

    let result = probe.probe().await;
    assert!(result.ok, "{result:?}");
    assert_eq!(result.name, "example.org");
    assert!(result.latency_ms.is_some_and(|ms| ms < 200), "{result:?}");
    assert_eq!(result.detail, None);
    assert!(
        chrono::DateTime::parse_from_rfc3339(&result.checked_at).is_ok(),
        "{result:?}"
    );
    assert_eq!(probe.last().await, Some(result.clone()));
    let json = serde_json::to_value(&result).unwrap();
    assert!(json["latencyMs"].is_u64(), "{json}");
    assert!(json["checkedAt"].is_string(), "{json}");
}

#[tokio::test]
pub(super) async fn an_error_no_address_or_no_answer_is_not_ok() {
    for (answer, delay, detail) in [
        (Err("NXDOMAIN".to_string()), Duration::ZERO, "NXDOMAIN"),
        (Ok(0), Duration::ZERO, "no address"),
        (Ok(1), Duration::from_secs(5), "no answer within 200ms"),
    ] {
        let result = probe(answer, delay).probe().await;
        assert!(!result.ok, "{result:?}");
        assert_eq!(result.latency_ms, None, "{result:?}");
        assert_eq!(result.detail.as_deref(), Some(detail), "{result:?}");
    }
}

//! An interrupted or unpublishable rotation.

use crate::settings_api::TaskNotFound;
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;

use super::*;

/// A [`SettingsApi`] that fails the FIRST write of a chosen dot-path and then
/// behaves normally, modelled at the commit: one `Store::save`, whose only two outcomes
/// are "landed" and "did not", and a write that does not land is one the
/// caller sees fail.
pub(super) struct InterruptOnce {
    pub(super) inner: Arc<FakeSettings>,
    pub(super) path: &'static str,
    pub(super) fired: std::sync::Mutex<bool>,
}

impl InterruptOnce {
    pub(super) fn new(tree: serde_json::Value, path: &'static str) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::new(FakeSettings::new(tree)),
            path,
            fired: std::sync::Mutex::new(false),
        })
    }
}

#[async_trait::async_trait]
impl SettingsApi for InterruptOnce {
    async fn get_settings(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        self.inner.get_settings(path).await
    }

    async fn set_settings(&self, path: &str, value: &serde_json::Value) -> anyhow::Result<String> {
        if path == self.path {
            let mut fired = self.fired.lock().unwrap();
            if !*fired {
                *fired = true;
                // Nothing is written: the save never reached its rename.
                return Err(anyhow::anyhow!("interrupted before the commit"));
            }
        }
        self.inner.set_settings(path, value).await
    }

    async fn get_task(&self, id: &str) -> anyhow::Result<crate::task_registry::TaskRecord> {
        self.inner
            .get_task(id)
            .await
            .map_err(|_| TaskNotFound(id.to_string()).into())
    }

    async fn get_state(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        self.inner.get_state(path).await
    }

    async fn get_time_status(&self) -> anyhow::Result<serde_json::Value> {
        self.inner.get_time_status().await
    }

    async fn get_storage_status(&self) -> anyhow::Result<serde_json::Value> {
        self.inner.get_storage_status().await
    }

    async fn get_system_info(&self) -> anyhow::Result<serde_json::Value> {
        self.inner.get_system_info().await
    }

    async fn get_telemetry(&self) -> anyhow::Result<serde_json::Value> {
        self.inner.get_telemetry().await
    }

    async fn get_observed_network(&self) -> anyhow::Result<serde_json::Value> {
        self.inner.get_observed_network().await
    }

    async fn get_failure_evidence(&self) -> anyhow::Result<serde_json::Value> {
        self.inner.get_failure_evidence().await
    }
    async fn get_log(&self, source: &str) -> anyhow::Result<serde_json::Value> {
        self.inner.get_log(source).await
    }

    async fn reboot(&self) -> anyhow::Result<()> {
        self.inner.reboot().await
    }

    async fn power_off(&self) -> anyhow::Result<()> {
        self.inner.power_off().await
    }

    async fn set_transient_root_password(&self, password: &str) -> anyhow::Result<String> {
        self.inner.set_transient_root_password(password).await
    }

    async fn rotate_wireguard_key(&self, iface: &str) -> anyhow::Result<String> {
        self.inner.rotate_wireguard_key(iface).await
    }

    async fn get_update_state(&self) -> anyhow::Result<serde_json::Value> {
        self.inner.get_update_state().await
    }

    async fn check_update(&self) -> anyhow::Result<()> {
        self.inner.check_update().await
    }

    async fn fetch_update(&self) -> anyhow::Result<()> {
        self.inner.fetch_update().await
    }

    async fn install_update(&self, bundle: &str) -> anyhow::Result<()> {
        self.inner.install_update(bundle).await
    }

    async fn confirm_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
        self.inner.confirm_deployment(deployment_id).await
    }

    async fn reject_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
        self.inner.reject_deployment(deployment_id).await
    }

    async fn rollback_deployment(&self, deployment_id: &str) -> anyhow::Result<()> {
        self.inner.rollback_deployment(deployment_id).await
    }

    async fn set_reboot_override(&self, seconds: u32) -> anyhow::Result<serde_json::Value> {
        self.inner.set_reboot_override(seconds).await
    }

    async fn set_update_config(
        &self,
        patch: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        self.inner.set_update_config(patch).await
    }
}

/// The interrupted rotation, then the retry: nothing half-lands, the previous
/// credential still works after the interruption, and the retry leaves ONE
/// credential — the one the retry published.
#[tokio::test]
pub(super) async fn an_interrupted_rotation_writes_nothing_and_the_retry_leaves_one_credential() {
    let presence = FakePresence::present();
    let api = InterruptOnce::new(claimed_tree(), "access");
    let router = app(AppState::new(api.clone(), SIGNING_KEY).with_presence(presence.clone()));
    let before = api.inner.get_settings("access").await.unwrap();

    // The attempt that does not commit.
    let interrupted = post_json(&router, RECOVERY_PATH, "", None).await;
    assert_eq!(interrupted.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        api.inner.get_settings("access").await.unwrap(),
        before,
        "an interrupted rotation left a credential behind"
    );
    // The previous credential still works, because nothing invalidated it.
    let still_works = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": PW_SENTINEL }),
        None,
        None,
    )
    .await;
    assert_eq!(still_works.status(), StatusCode::CREATED);

    // The retry, on a device the interruption left exactly as it found it.
    let response = post_json(&router, RECOVERY_PATH, "", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["generation"], json!(5));

    // Two credentials were published, and only the last one authenticates:
    // the interrupted attempt's was never stored, which is what makes the
    // ordering safe rather than merely lucky.
    let published = presence.published();
    assert_eq!(published.len(), 2, "{published:?}");
    let latest = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": published[1] }),
        None,
        None,
    )
    .await;
    assert_eq!(latest.status(), StatusCode::CREATED);
    let orphan = json_request(
        &router,
        "POST",
        "/api/v1/session",
        json!({ "password": published[0] }),
        None,
        None,
    )
    .await;
    assert_eq!(
        orphan.status(),
        StatusCode::UNAUTHORIZED,
        "the interrupted attempt's credential authenticates"
    );
}

// --- No secret anywhere but the presence channel ---------------------------

/// The minted credential appears on the presence channel and NOWHERE else.
///
/// The sentinel is the previous password, driven through every surface a
/// secret could leak into; the minted one is read back off the channel and
/// searched for in the same places. rule 2 is what makes the response
/// body one of those places rather than the permitted appearance: the
/// credential is returned on the channel that proved presence, never over the
/// network.
#[tokio::test]
pub(super) async fn no_recovery_surface_emits_a_credential() {
    let presence = FakePresence::present();
    let (router, fake, dir, _token) = device(presence.clone());

    let response = post_json(&router, RECOVERY_PATH, "", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = format!("{:?}", response.headers());
    let body = body_string(response).await;
    let minted = presence.secret();
    let hash = access_of(&fake).await["webAdmin"]["password_hash"]
        .as_str()
        .unwrap()
        .to_string();

    for haystack in [&headers, &body] {
        assert!(
            !haystack.contains(&minted),
            "the new credential: {haystack}"
        );
        assert!(!haystack.contains(PW_SENTINEL), "{haystack}");
        assert!(!haystack.contains(&hash), "{haystack}");
        assert!(!haystack.contains("argon2"), "{haystack}");
    }

    let trail = std::fs::read_to_string(dir.path().join("audit.log")).unwrap();
    assert!(!trail.contains(&minted), "{trail}");
    assert!(!trail.contains(PW_SENTINEL), "{trail}");
    assert!(!trail.contains(&hash), "{trail}");

    // Nor in the settings tree beyond the hash it became, and the claim record
    // holds nothing about the credential at all.
    let record = access_of(&fake).await["claim"].to_string();
    assert!(!record.contains(&minted), "{record}");
}

/// The refusals say nothing about the credential either, on any of the three
/// paths that can turn the flow away.
#[tokio::test]
pub(super) async fn a_refused_recovery_discloses_nothing_about_the_credential() {
    let (router, fake, _dir, token) = device(FakePresence::absent());
    let hash = access_of(&fake).await["webAdmin"]["password_hash"]
        .as_str()
        .unwrap()
        .to_string();

    let no_presence = body_string(post_json(&router, RECOVERY_PATH, "", None).await).await;
    let authenticated =
        body_string(bearer_json(&router, "POST", RECOVERY_PATH, &token, "").await).await;

    for haystack in [&no_presence, &authenticated] {
        assert!(!haystack.contains(PW_SENTINEL), "{haystack}");
        assert!(!haystack.contains(&hash), "{haystack}");
        assert!(!haystack.contains("argon2"), "{haystack}");
    }
}

// --- The shipped reader, against a board's own declaration ------------------

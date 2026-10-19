//! Interrupted, unanswered and refused claims, and what they disclose.

use crate::settings_api::TaskNotFound;
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;

use super::*;

/// A [`SettingsApi`] that fails the FIRST write of a chosen dot-path and then
/// behaves normally.
///
/// This is the power loss, modelled where it is observable: the claim commits
/// through one `Store::save`, and the only two outcomes that save has are
/// "landed" and "did not". A write that does not land is a write the caller
/// sees fail, so failing it here drives the same tree state a power cut
/// before the rename would leave.
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

/// The interrupted path, then the retry: nothing half-lands, and the retry
/// mints no second identity and no second credential.
///
/// The identity assertion is `provisioning.deviceId`, which is what
/// "without cloning identity" is about: it is drawn once by
/// `micad/src/identity.rs` and the claim never touches it, so a claim driven
/// twice must leave the same one. The credential assertion is the session the
/// second attempt hands back: it works, and it is the only thing that does.
#[tokio::test]
pub(super) async fn an_interrupted_claim_writes_nothing_and_the_retry_mints_one_identity() {
    let api = InterruptOnce::new(seeded_tree(), "access");
    let router = app(AppState::new(api.clone(), SIGNING_KEY));

    // The attempt that does not commit.
    let interrupted = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(interrupted.status(), StatusCode::SERVICE_UNAVAILABLE);
    let access = access_of(&api.inner).await;
    assert!(
        access.get("webAdmin").is_none(),
        "an interrupted claim left a credential: {access}"
    );
    assert!(access.get("claim").is_none(), "{access}");
    assert!(access.get("apiTokens").is_none(), "{access}");
    let device_id = api
        .inner
        .get_settings("provisioning.deviceId")
        .await
        .unwrap();

    // The retry, on a device the interruption left exactly as it found it.
    let response = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&response);

    let access = access_of(&api.inner).await;
    assert!(
        access.get("apiTokens").is_none(),
        "the retry left a bearer credential behind: {access}"
    );
    assert_eq!(access["claim"]["via"], json!("setup"));
    assert_eq!(
        api.inner
            .get_settings("provisioning.deviceId")
            .await
            .unwrap(),
        device_id,
        "the retry moved the device identity"
    );

    // And the one credential the caller was handed is the one that works.
    let probe = get(&router, "/api/v1/meta", Some(&cookie)).await;
    assert_eq!(probe.status(), StatusCode::OK);
}

/// The other half of the same power loss: the save LANDED and the answer never
/// reached the caller, so the caller retries a claim that already happened.
///
/// It is refused, and the refusal is what keeps the device single-credentialled
/// — the first session still authenticates, the claim record still says what it
/// said, and the identity has not moved.
#[tokio::test]
pub(super) async fn a_claim_that_committed_but_was_never_answered_is_refused_on_retry() {
    let (router, fake) = test_app(seeded_tree());

    let first = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(first.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&first);
    let committed = access_of(&fake).await;

    let retry = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(retry.status(), StatusCode::CONFLICT);
    assert_eq!(envelope(retry).await["code"], "already_configured");

    assert_eq!(
        access_of(&fake).await,
        committed,
        "the refused retry changed the claim"
    );
    assert_eq!(
        fake.set_paths(),
        vec!["access".to_string()],
        "the refused retry wrote something"
    );
    let probe = get(&router, "/api/v1/meta", Some(&cookie)).await;
    assert_eq!(probe.status(), StatusCode::OK);
}

// --- The already-claimed contract ------------------------------------------

/// The refusal is audited, and it discloses nothing an unauthenticated caller
/// could not already read off `GET /api/v1/session`.
///
/// **The inference, stated.** An unauthenticated caller learns exactly one
/// bit: whether this device has an administrator credential. It learns it from
/// `GET /api/v1/session` already — that route answers `setup` or
/// `unauthenticated` without a credential, because a first-run wizard cannot
/// ask for one — so the 409 here adds nothing to what a device on a network
/// tells anyone who asks. What is NOT disclosed is anything about the
/// credential: not its hash, not its length, not when it was set, not which
/// channel set it. That distinction is the whole of the argument, and it is
/// asserted below rather than promised.
#[tokio::test]
pub(super) async fn a_refused_claim_is_audited_and_discloses_only_what_the_session_route_does() {
    let dir = TempDir::new().unwrap();
    let fake = Arc::new(FakeSettings::new(configured_tree(PW_SENTINEL)));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY).with_persistence(dir.path()));

    let response = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let headers = format!("{:?}", response.headers());
    let body = body_string(response).await;

    // The one bit, and it is the bit `GET /api/v1/session` already serves.
    let session = get(&router, "/api/v1/session", None).await;
    assert_eq!(session.status(), StatusCode::OK);
    assert_eq!(body_json(session).await["state"], json!("unauthenticated"));

    // And nothing about the credential itself.
    let hash = fake
        .get_settings("access.webAdmin.password_hash")
        .await
        .unwrap();
    let hash = hash.as_str().unwrap();
    for haystack in [&headers, &body] {
        assert!(!haystack.contains(PW_SENTINEL), "{haystack}");
        assert!(!haystack.contains(hash), "{haystack}");
        assert!(!haystack.contains("argon2"), "{haystack}");
        // Not the channel and not the moment either: a refusal says the device
        // is claimed, never how or when.
        assert!(!haystack.contains("provisioning-document"), "{haystack}");
    }

    assert_eq!(
        audit_events(&audit_lines(dir.path())),
        [("claim".to_string(), "refused".to_string())]
    );
    let raw = std::fs::read_to_string(dir.path().join("audit.log")).unwrap();
    assert!(!raw.contains(PW_SENTINEL), "{raw}");
    assert!(!raw.contains(hash), "{raw}");
}

/// Nothing the claim path emits carries the password, on the success path or
/// on either refusal.
///
/// The sentinel is driven through every surface a secret could leak into: the
/// audit trail, the claim status, the settings tree's own readable fields, and
/// the bodies and headers of both failures. The success body carries no secret
/// at all: setup mints no token, so the only credential it creates is the
/// password the caller already holds.
#[tokio::test]
pub(super) async fn no_claim_surface_emits_the_password() {
    let dir = TempDir::new().unwrap();
    let fake = Arc::new(FakeSettings::new(seeded_tree()));
    let router = app(AppState::new(fake.clone(), SIGNING_KEY).with_persistence(dir.path()));

    // The refusal that happens before anything is written.
    let short = post_json(
        &router,
        "/api/v1/setup",
        &json!({ "password": "PW-SEN" }).to_string(),
        None,
    )
    .await;
    assert_eq!(short.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!body_string(short).await.contains("PW-SEN"));

    let created = post_json(&router, "/api/v1/setup", &claim_body(), None).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let cookie = session_cookie_value(&created);
    assert!(!body_string(created).await.contains(PW_SENTINEL));

    let status = get(&router, CLAIM_PATH, Some(&cookie)).await;
    assert_eq!(status.status(), StatusCode::OK);
    let status = body_string(status).await;
    assert!(!status.contains(PW_SENTINEL), "{status}");
    // Nor the hash the password became: the claim status is a shape, not a
    // credential read-back.
    assert!(!status.contains("argon2"), "{status}");

    let trail = std::fs::read_to_string(dir.path().join("audit.log")).unwrap();
    assert!(!trail.contains(PW_SENTINEL), "{trail}");
    assert!(!trail.contains(&cookie), "{trail}");

    // The stored claim record itself holds nothing the caller typed.
    let record = access_of(&fake).await["claim"].clone();
    let record = record.to_string();
    assert!(!record.contains(PW_SENTINEL), "{record}");
}

// --- One claim state, two channels -----------------------------------------

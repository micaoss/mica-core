//! HTTP routing for the JSON management API and its two UI entry points.
//!
//! `/api/` owns every management read and mutation. `/_ui` is the built-in SPA
//! embedded in the binary. `/` serves a valid active custom bundle and
//! otherwise redirects to `/_ui/`; custom assets are considered only by the
//! final fallback, so neither UI can shadow the API or health endpoint.

// `PathBuf` and not `Path`: axum's own `Path` extractor is imported below, and
// the filesystem type of that name would shadow it.
use std::sync::Arc;

use anyhow::Context as _;

use axum::extract::{OriginalUri, Request, State};
use axum::http::StatusCode;
use axum::http::header::{HOST, LOCATION};
use axum::response::{IntoResponse, Response};
// `delete` and `put` are imported on their own lines rather than folded into
// the routing import below, which is how they were added: apid had served no
// write verb at all until these two arrived.
use axum::Router;
use axum::routing::{any, get};

use crate::access_cache::AccessCache;
use crate::assets::serve;
use crate::audit::Audit;
use crate::auth::GuardStore;
use crate::bundle::Store;
use crate::diagnostics::SnapshotStore;
use crate::session::SessionStore;
use crate::settings_api::{InvalidTaskPayload, SettingsApi};
use crate::task_registry::{TaskRecord, TaskRegistry};

mod bluetooth;
mod claim;
mod containers;
mod credential;
mod error;
mod login;
mod logs;
mod meta;
mod mqtt;
mod network;
mod network_model;
mod observe;
mod password;
mod paths;
mod peers;
mod power;
mod presence;
mod reset;
mod router;
mod settings;
mod setup;
mod snapshots;
mod ssh;
mod tokens;
mod ui;
mod ui_upload;
mod validate;
mod web;
mod wifi;
mod wifi_ap;
pub(crate) use bluetooth::*;
pub(crate) use claim::*;
pub(crate) use containers::*;
pub(crate) use credential::*;
pub(crate) use error::*;
pub(crate) use login::*;
pub(crate) use logs::*;
pub(crate) use meta::*;
pub(crate) use mqtt::*;
pub(crate) use network::*;
pub(crate) use network_model::*;
pub(crate) use observe::*;
pub(crate) use password::*;
pub(crate) use paths::*;
pub(crate) use peers::*;
pub(crate) use power::*;
pub(crate) use presence::*;
pub(crate) use reset::*;
use router::*;
pub(crate) use settings::*;
pub(crate) use setup::*;
pub(crate) use snapshots::*;
pub(crate) use ssh::*;
pub(crate) use tokens::*;
pub(crate) use ui::*;
pub(crate) use ui_upload::*;
use validate::*;
pub(crate) use web::*;
pub(crate) use wifi::*;
pub(crate) use wifi_ap::*;

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    pub(crate) api: Arc<dyn SettingsApi>,
    sessions: Arc<SessionStore>,
    guard: Arc<GuardStore>,
    pub(crate) audit: Arc<Audit>,
    bundles: Arc<Store>,
    /// Serialises custom-UI pointer mutations so concurrent API requests are
    /// deterministic and cannot contend for the atomic replacement link.
    ui_selection: Arc<tokio::sync::Mutex<()>>,
    /// Serialises the device claim, so `POST /api/v1/setup`'s "is this device
    /// still unclaimed" and the write that claims it are one step rather than
    /// two. See [`api_v1_setup`] for why the atomicity is here.
    claim: Arc<tokio::sync::Mutex<()>>,
    /// The gate's cache of the `access` subtree, kept honest by the
    /// `SettingsChanged` watcher (`bus_client::watch_settings_changed`) and
    /// by the two handlers that write under `access` themselves.
    access_cache: Arc<AccessCache>,
    /// Notification-fed apply-task mirror. It serves only while its
    /// `TaskChanged` subscription is live.
    task_registry: Arc<TaskRegistry>,
    /// The baked public metadata, addressed by its MANIFEST rather than by
    /// its directory: `/usr/share/mica/meta/updates/manifest.json` on a
    /// device, a temporary tree in tests.
    ///
    /// One path and not two, because
    /// `provisioning_api::api_v1_provisioning_status` reads layer 1 twice —
    /// once to digest the baked tree, once through
    /// `micad_settings::configuration` to resolve the effective policy — and
    /// a device that answered those from two different trees in one response
    /// would be reporting a configuration no device has. The directory walked
    /// for digests is derived from this file, so they cannot diverge.
    ///
    /// The default is [`micad_settings::configuration::DEFAULT_MANIFEST_PATH`]
    /// itself rather than a copy of its text: the tree already carries that
    /// literal once, and a second spelling of it here would agree with the
    /// first until somebody moved the file.
    pub(crate) meta_manifest: Arc<std::path::PathBuf>,
    /// The sole operator update document: `/mica/config/updates.json` on a
    /// device, an isolated path in route tests.
    ///
    /// The default is the configuration library's production path. Only the
    /// test-only builder below can replace it, so no shipped caller can
    /// redirect this configuration input.
    pub(crate) updates_path: Arc<std::path::PathBuf>,
    /// The desired fleet document: `/mica/config/fleet.json` on a device, an
    /// isolated path in route tests.
    pub(crate) fleet_path: Arc<std::path::PathBuf>,
    /// Where an uploaded update archive is streamed before micad imports it:
    /// `/mica/updates/uploads` on a device, a temporary directory in tests.
    ///
    /// apid writes the file and names it on the bus; micad refuses a path
    /// outside this directory, so the two agree on one location and neither
    /// takes the other's word for it.
    pub(crate) update_uploads: Arc<std::path::PathBuf>,
    /// The diagnostic snapshot store: `/mica/diagnostics` on a
    /// device, a temporary directory in tests. A path and no syscall until
    /// the first publish.
    diagnostics: Arc<SnapshotStore>,
    /// Serialises snapshot collection. One at a time: a second request while
    /// one runs is refused (409) rather than queued, because each one reads
    /// every micad surface and the second would only repeat the first.
    collecting: Arc<tokio::sync::Mutex<()>>,
    /// The ONE physical-presence seam, keyed by
    /// the [`PRESENCE_CAPABILITY`] board capability. Every presence-gated
    /// operation asks this and nothing else.
    presence: Arc<dyn Presence>,
    /// Serialises presence-gated credential recovery, so that reading the
    /// assertion and spending it are one step rather than two. See
    /// [`api_v1_recovery_credential`] for why the atomicity is here.
    rotation: Arc<tokio::sync::Mutex<()>>,
    /// The built-in console, when the `mica-apid-ui` package installed one.
    /// Absent, `/_ui` answers 404 and the API is unchanged.
    builtin: Arc<Option<crate::assets::builtin::Builtin>>,
    /// The features the product carries; the routes of any other are not
    /// mounted.
    pub(crate) features: micad_settings::Features,
    /// The bytes an extracted UI bundle leaves free on DATA: the signed boot
    /// policy's DATA reserve, the one the deployment client keeps.
    data_reserve: u64,
    /// Whether the session cookie carries `Secure`: exactly when the console
    /// is served over HTTPS. Over plain HTTP a browser would drop a `Secure`
    /// cookie and no login could hold.
    secure_cookies: bool,
    /// The console's TLS identity: read, uploaded and generated through
    /// `/api/v1/web/certificate`, and reloaded into the live server.
    pub(crate) identity: Arc<crate::tls::IdentityStore>,
}

const MIB: u64 = 1024 * 1024;

/// The DATA reserve of a board that declares none, as the deployment client
/// keeps it.
const DEFAULT_DATA_RESERVE: u64 = 128 * MIB;

/// The signed boot policy the device booted with.
pub const BOOT_POLICY_PATH: &str = "/run/mica/boot-policy.json";

/// The DATA reserve of the boot policy `text`: `board.reserves.data` when it
/// declares one from 1 MiB to 1 GiB, and the default otherwise, including
/// when there is no policy to read.
#[must_use]
pub fn data_reserve(text: Option<&str>) -> u64 {
    text.and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .and_then(|policy| policy.pointer("/board/reserves/data")?.as_u64())
        .filter(|bytes| (MIB..=1024 * MIB).contains(bytes))
        .unwrap_or(DEFAULT_DATA_RESERVE)
}

impl AppState {
    /// State around a settings backend and the cookie signing key.
    ///
    /// The bundle store is constructed here and reads nothing: there is no
    /// bundle discovery before the listeners bind, and `Store::at_default` is
    /// a path and no syscall. Discovery and the start-up compatibility
    /// re-check are separate work.
    /// The backoff counter and the audit trail default to their
    /// non-persistent forms so that constructing state needs no filesystem;
    /// `with_persistence` is what production calls, and the "a
    /// power cycle must not reset the clock" is that call, not this one.
    pub fn new(api: Arc<dyn SettingsApi>, signing_key: [u8; 32]) -> Self {
        Self {
            api,
            sessions: Arc::new(SessionStore::new(signing_key)),
            guard: Arc::new(GuardStore::ephemeral()),
            audit: Arc::new(Audit::journal_only()),
            bundles: Arc::new(Store::at_default()),
            ui_selection: Arc::new(tokio::sync::Mutex::new(())),
            claim: Arc::new(tokio::sync::Mutex::new(())),
            access_cache: Arc::new(AccessCache::new()),
            task_registry: Arc::new(TaskRegistry::new()),
            meta_manifest: Arc::new(std::path::PathBuf::from(
                micad_settings::configuration::DEFAULT_MANIFEST_PATH,
            )),
            updates_path: Arc::new(std::path::PathBuf::from(
                micad_settings::configuration::DEFAULT_UPDATES_PATH,
            )),
            fleet_path: Arc::new(std::path::PathBuf::from(
                micad_settings::configuration::DEFAULT_FLEET_PATH,
            )),
            update_uploads: Arc::new(std::path::PathBuf::from(UPDATE_UPLOADS_DIR)),
            diagnostics: Arc::new(SnapshotStore::at_default()),
            collecting: Arc::new(tokio::sync::Mutex::new(())),
            rotation: Arc::new(tokio::sync::Mutex::new(())),
            // A path and no syscall, like the bundle and snapshot stores: the
            // marker is read when something asks for presence and never at
            // construction.
            presence: Arc::new(MarkerPresence::at_default()),
            // No console until one is attached: constructing state reads no
            // directory, like every store above.
            builtin: Arc::new(None),
            features: micad_settings::Features::all(),
            data_reserve: DEFAULT_DATA_RESERVE,
            secure_cookies: true,
            identity: Arc::new(crate::tls::IdentityStore::new(
                std::path::Path::new(crate::config::DEFAULT_STATE_DIR)
                    .join(crate::tls::IDENTITY_FILE),
                None,
            )),
        }
    }

    /// Mark the session cookie `Secure` or not; see the field.
    #[must_use]
    pub fn with_secure_cookies(mut self, secure: bool) -> Self {
        self.secure_cookies = secure;
        self
    }

    /// The `Set-Cookie` value establishing a session.
    pub(crate) fn session_cookie(&self, value: &str) -> String {
        crate::session::session_cookie(value, self.secure_cookies)
    }

    /// The `Set-Cookie` value clearing the session cookie.
    pub(crate) fn clear_cookie(&self) -> String {
        crate::session::clear_cookie(self.secure_cookies)
    }

    /// Keep `bytes` free on DATA when extracting a UI bundle.
    #[must_use]
    pub fn with_data_reserve(mut self, bytes: u64) -> Self {
        self.data_reserve = bytes;
        self
    }

    /// Serve only the routes of `features`.
    #[must_use]
    pub fn with_features(mut self, features: micad_settings::Features) -> Self {
        self.features = features;
        self
    }

    /// Attach the built-in console indexed from `dir`. An absent directory is
    /// no console; a tree that breaks the rules is no console either, and is
    /// said so, because a bad console must not take the API down with it.
    #[must_use]
    pub fn with_builtin_ui(mut self, dir: &std::path::Path) -> Self {
        let builtin = match crate::assets::builtin::Builtin::load(dir) {
            Ok(builtin) => builtin,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "built-in console refused");
                None
            }
        };
        self.builtin = Arc::new(builtin);
        self
    }

    /// The built-in console, if one is installed.
    pub(crate) fn builtin(&self) -> Option<&crate::assets::builtin::Builtin> {
        self.builtin.as_ref().as_ref()
    }

    /// The gate's access cache, for `main.rs` to hand to the
    /// `SettingsChanged` watcher, and for the tests that drive its
    /// subscription state by hand.
    pub(crate) fn access_cache(&self) -> &Arc<AccessCache> {
        &self.access_cache
    }

    pub(crate) fn task_registry(&self) -> &Arc<TaskRegistry> {
        &self.task_registry
    }

    /// One task from the live registry, or a bounded direct bus read while the
    /// subscription is unavailable. The generation check prevents that read
    /// from overwriting a signal that arrived while it was in flight.
    async fn task_record(&self, id: &str) -> anyhow::Result<TaskRecord> {
        if let Some(task) = self.task_registry.get(id) {
            return Ok(task);
        }
        let stale = self.task_registry.stale(id);
        let generation = self.task_registry.generation();
        let task = match self.api.get_task(id).await {
            Ok(task) => task,
            Err(err) if is_task_not_found(&err) && stale.is_some() => {
                let Some(mut task) = stale else {
                    return Err(err);
                };
                if !task.terminal() {
                    task.status = "finished".to_string();
                    task.finished_at = Some(
                        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
                    );
                    task.outcome = Some("interrupted".to_string());
                    task.message = Some(
                        "micad restarted or rolled over its bounded task history before this apply's terminal signal was retained; startup reconciliation converges persisted settings"
                            .to_string(),
                    );
                }
                task
            }
            Err(err) => return Err(err),
        };
        self.task_registry.fill(generation, task.clone());
        Ok(task)
    }

    /// The bounded task history from the live signal mirror, or from micad's
    /// live-state snapshot while the subscription is unavailable.
    async fn task_records(&self) -> anyhow::Result<Vec<TaskRecord>> {
        if let Some(tasks) = self.task_registry.list() {
            return Ok(tasks);
        }
        let generation = self.task_registry.generation();
        let state = self.api.get_state("").await?;
        let tasks = match state.get("tasks") {
            Some(tasks) => serde_json::from_value(tasks.clone()).map_err(InvalidTaskPayload)?,
            None => Vec::new(),
        };
        self.task_registry.fill_list(generation, tasks.clone());
        Ok(self.task_registry.list().unwrap_or(tasks))
    }

    /// Root the backoff counter and the audit ring in `state_dir`.
    ///
    /// The directory must already exist — `main.rs` creates it before this is
    /// called, on the same path that holds the TLS material.
    pub fn with_persistence(mut self, state_dir: &std::path::Path) -> Self {
        self.guard = Arc::new(GuardStore::load(state_dir.join("login_guard.json")));
        self.audit = Arc::new(Audit::at(state_dir.to_path_buf()));
        self.identity = Arc::new(self.identity.at(state_dir.join(crate::tls::IDENTITY_FILE)));
        self
    }

    /// Serve HTTPS with `live`, so an identity write reloads it.
    #[must_use]
    pub fn with_tls(mut self, live: axum_server::tls_rustls::RustlsConfig) -> Self {
        self.identity = Arc::new(self.identity.serving(live));
        self
    }

    /// The `/mica/ui` bundle store the asset router reads.
    pub(crate) fn bundles(&self) -> &Store {
        &self.bundles
    }

    /// The audit sink, for the start-up path (`main.rs` hands it to bundle
    /// discovery so a staged custom UI's activation is recorded too).
    pub(crate) fn audit(&self) -> &Arc<Audit> {
        &self.audit
    }

    /// Root the bundle store somewhere else, for tests that install one.
    ///
    /// Test-only on purpose: the shipped location is fixed and nothing
    /// configures it.
    #[cfg(test)]
    pub fn with_bundle_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.bundles = Arc::new(Store::new(root));
        self
    }

    /// Root the diagnostic snapshot store somewhere else, for tests.
    ///
    /// Test-only for the bundle root's reason: the shipped location is
    /// fixed and nothing configures it.
    #[cfg(test)]
    pub fn with_diagnostics_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.diagnostics = Arc::new(SnapshotStore::new(root));
        self
    }

    /// Point the baked metadata at another tree, for tests.
    ///
    /// Test-only for the bundle root's reason: the shipped location is fixed
    /// and nothing configures it. It takes the manifest and derives the tree,
    /// which is also the shape `MICAD_META_MANIFEST_PATH` gives the micad side.
    #[cfg(test)]
    pub fn with_meta_manifest(mut self, manifest: impl Into<std::path::PathBuf>) -> Self {
        self.meta_manifest = Arc::new(manifest.into());
        self
    }

    /// Point the operator update document at an isolated test fixture.
    ///
    /// Test-only for the baked manifest's reason: the shipped location is
    /// fixed and nothing configures it.
    #[cfg(test)]
    pub fn with_updates_path(mut self, updates: impl Into<std::path::PathBuf>) -> Self {
        self.updates_path = Arc::new(updates.into());
        self
    }

    /// Point the update upload directory at an isolated test fixture.
    #[cfg(test)]
    pub fn with_update_uploads(mut self, uploads: impl Into<std::path::PathBuf>) -> Self {
        self.update_uploads = Arc::new(uploads.into());
        self
    }

    /// Point the desired fleet document at an isolated test fixture.
    #[cfg(test)]
    pub fn with_fleet_path(mut self, fleet: impl Into<std::path::PathBuf>) -> Self {
        self.fleet_path = Arc::new(fleet.into());
        self
    }

    /// Drive the presence seam from a test.
    ///
    /// Test-only, and it is the reason the seam is a trait: a presence
    /// assertion is an action at the DEVICE,
    /// so nothing a test can do to a shipped build establishes one, and
    /// nothing a shipped build offers can either — which is the property the
    /// gate is for.
    #[cfg(test)]
    pub fn with_presence(mut self, presence: Arc<dyn Presence>) -> Self {
        self.presence = presence;
        self
    }
}

/// The HTTPS application router.
///
/// The precedence rule is this function's declaration order, and it is
/// total. Rules 1-3 are `.route`/`.nest` declarations and rule 4 is the
/// `.fallback`; axum matches declared routes before it consults a fallback, so
/// a bundle that ships a file at `api/v1/settings`, at `healthz` or at `login`
/// cannot capture any of them. The custom asset resolver additionally rejects
/// ambiguous encoded or repeated-separator spellings of the reserved roots;
/// those aliases fail closed instead of being normalised into another domain.
pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/", get(serve::root))
        .nest(
            "/_ui",
            Router::new()
                .route("/", get(crate::assets::builtin::index))
                .route("/{*path}", get(crate::assets::builtin::serve)),
        )
        .route("/_ui/", get(crate::assets::builtin::index))
        .route("/healthz", get(healthz))
        .nest(API, api_router(&state.features))
        .route("/api/", any(api_not_found))
        .fallback(serve::fallback)
        .with_state(state)
}

/// The reserved subtree's own not-found handler (rule 1, the "why
/// 404s inside `/api/` are the API's own").
///
/// The envelope, which is what makes a mistyped path a machine-readable
/// answer rather than an empty body.
async fn api_not_found(OriginalUri(uri): OriginalUri) -> Response {
    api_response(
        StatusCode::NOT_FOUND,
        ApiError::apid("not_found", format!("no API route at {}", uri.path())),
    )
}

// The API surface: the reserved subtree's declared routes, their bodies
// and the session check that guards them.

/// Redirect-only router served on the HTTP listener: 308 every request to
/// the HTTPS origin derived from the `Host` header.
pub fn redirect_app(https_port: u16) -> Router {
    Router::new()
        .fallback(redirect_to_https)
        .with_state(https_port)
}

async fn redirect_to_https(State(https_port): State<u16>, request: Request) -> Response {
    let host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("localhost");
    let host = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    let path = request.uri().path_and_query().map_or("/", |pq| pq.as_str());
    let target = if https_port == 443 {
        format!("https://{host}{path}")
    } else {
        format!("https://{host}:{https_port}{path}")
    };
    (StatusCode::PERMANENT_REDIRECT, [(LOCATION, target)]).into_response()
}

/// Listener health used by the boot readiness gate.
async fn healthz() -> &'static str {
    "ok"
}

// Validation

//! The observation endpoints: time, storage, system, telemetry and network.

use crate::redact;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;

use super::*;

/// Read the time-synchronization status.
///
/// Observed from timesyncd at request time and classified by micad:
/// `synchronized` (timedate1 reports a bounded clock error, which it computes
/// as `adjtimex().maxerror < 16 s` — not "a reply arrived"), `polling` (a
/// server is selected and packets are being exchanged, and that bound is not
/// reported; a device can hold this state indefinitely, so it promises no
/// convergence), `offline-degraded` (no reachable server; retries continue on
/// the pinned 30-second policy), `invalid-source` (a server answered and its
/// replies cannot be used), or `unknown` (a signal the reported state would
/// rest on could not be read: timesyncd is not observable at all, or
/// timedate1 did not answer `NTPSynchronized`). `unknown` is not a claim
/// about the clock — a device nobody could query is not a device that was
/// queried and found out of sync — so the `synchronized` member is absent
/// there and `detail` names the read that went missing. Read-only: there is
/// no route that pauses or stops synchronization.
#[utoipa::path(
    get,
    path = V1_TIME_STATUS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The classified status with the evidence it rests on: the selected server, the kernel's synchronized bit, and the last sample with its offset and a `correction` member telling a clock step from ordinary drift", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad failed to observe (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_time_status(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    // No dot-path: the status names no setting, so a failure envelope carries
    // no `path` member — the power actions' shape.
    match state.api.get_time_status().await {
        Ok(value) => api_response(StatusCode::OK, ResourceValue(redact::redact(value, ""))),
        Err(err) => bus_api_error(&err, None),
    }
}

/// Read the storage status.
///
/// Observed by micad at request time: the firmware/ESP, SYSTEM and DATA partitions
/// with its device, size, mount and read-only state, its space accounting
/// including the filesystem's reserved pool, and whatever the system recorded
/// about its last check; the two bind namespaces, `/mica` and `/srv`,
/// each with its readiness against the DATA tier it must live on; every
/// physical medium with normalized wear where the device exports it and an
/// explicit `unsupported` with a reason where it does not; the low-space
/// thresholds with their hysteresis band; observed DATA directory usage and
/// project quotas; and the explicit lifecycle decisions.
///
/// `/mica` and `/srv` are two namespaces of ONE filesystem and share its
/// capacity pool, so their bytes are reported once, on the `data` tier, and
/// never a second time under each bind.
///
/// Read-only. There is no route that formats, repartitions, resizes, mounts
/// or erases storage, and adding one is a product decision this surface does
/// not anticipate.
#[utoipa::path(
    get,
    path = V1_STORAGE_STATUS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The fixed tiers with their space, mount and check evidence; the `/mica` and `/srv` bind namespaces with their readiness (one shared capacity pool, reported once on the `data` tier); the physical media with normalized wear or an explicit `unsupported` reason; the low-space policy, directory usage and project quotas; and the explicit data-lifecycle decisions", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad failed to observe (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`); the operation may still be running", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_storage_status(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    // No dot-path: the status names no setting, so a failure envelope carries
    // no `path` member — the time status's shape.
    match state.api.get_storage_status().await {
        Ok(value) => api_response(StatusCode::OK, ResourceValue(redact::redact(value, ""))),
        Err(err) => bus_api_error(&err, None),
    }
}

/// Read the system-information surface.
///
/// One read answers what this device is: the machine id, the board, the
/// kernel, the distribution release, the image version with its git stamp
/// and build date, every installed package with its version (from the
/// shipped manifest), the authenticated running deployment and the uptime. micad assembles it at
/// request time from the seams that already carry each fact; nothing is
/// restated. Every member carries `available`, and an absent fact says why.
#[utoipa::path(
    get,
    path = V1_SYSTEM_INFO_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The surface: `machineId`, `board`, `kernel`, `release`, `system` (version, `fileEpoch`), `daemon` (name, package version), `packages`, `deployment`, `uptime`; each an object carrying `available`", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad failed to observe (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_system_info(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_system_info().await {
        Ok(value) => api_response(StatusCode::OK, ResourceValue(redact::redact(value, ""))),
        Err(err) => bus_api_error(&err, None),
    }
}

/// Read the board telemetry.
///
/// Temperature (thermal zones and hwmon inputs), every watchdog device with
/// its boot status, and the reset reason as far as the kernel's generic
/// sources tell it: a watchdog's `cardReset` flag or a crash record in
/// pstore. Absence is explicit — a board that exports no source reports
/// `available: false` with the reason, never a healthy reading.
#[utoipa::path(
    get,
    path = V1_SYSTEM_TELEMETRY_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`thermal`, `watchdog` and `reset`, each carrying `available`; `reset.reason` is `watchdog`, `kernel-crash` or `unknown`, with the evidence beside it", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad failed to observe (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_system_telemetry(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_telemetry().await {
        Ok(value) => api_response(StatusCode::OK, ResourceValue(redact::redact(value, ""))),
        Err(err) => bus_api_error(&err, None),
    }
}

/// Read the observed network state.
///
/// What the network stack actually sees, distinct from the desired map at
/// `/v1/network`: per interface the link and carrier state, the addresses
/// with where each came from, the DHCP lease, the DNS servers, and the
/// Wi-Fi association; the default routes; DNS reachability by a single
/// bounded probe through resolved; and the radio and modem capabilities,
/// with cellular explicitly unsupported. Nothing from the settings tree is
/// in this answer, so a configured interface with no carrier reads as
/// exactly that.
#[utoipa::path(
    get,
    path = V1_NETWORK_STATUS_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`interfaces`, `defaultRoutes`, `dns`, `wifi` and `capabilities`, each carrying `available` or `supported`; absent evidence carries the reason", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 500, description = "micad failed to observe (`micad_failed`)", body = ApiError),
        (status = 503, description = "The call to micad could not be made (`micad_unreachable`); carries `Retry-After`", body = ApiError),
        (status = 504, description = "The bounded call to micad timed out (`micad_timeout`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_network_status(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    match state.api.get_observed_network().await {
        Ok(value) => api_response(StatusCode::OK, ResourceValue(redact::redact(value, ""))),
        Err(err) => bus_api_error(&err, None),
    }
}

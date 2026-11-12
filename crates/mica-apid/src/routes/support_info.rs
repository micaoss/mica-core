//! The support summary (`GET /api/v1/system/support-info`): the facts a
//! support case opens with, each picked by name from what micad already
//! answers. Nothing is copied wholesale, so a member a source grows later does
//! not reach the summary until it is named here.

use axum::extract::State;
use axum::http::header::CONTENT_DISPOSITION;
use axum::http::{HeaderValue, StatusCode};
use axum::response::Response;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use super::*;

/// The shape version of the summary.
const SCHEMA_VERSION: u64 = 1;

/// The longest text value carried, in characters.
const MAX_TEXT_CHARS: usize = 128;

/// What the summary is derived from; `None` is a read that failed.
pub(super) struct SupportSources {
    pub(super) info: Option<Value>,
    pub(super) update: Option<Value>,
    pub(super) storage: Option<Value>,
    pub(super) time: Option<Value>,
    pub(super) timezone: Option<Value>,
}

/// The summary. Each section is an object with `available`; one whose source
/// could not be read is `{available: false, reason}` (`source_failed`, or
/// `unavailable` when the source answered without it) and the rest stand.
pub(super) fn support_info(sources: SupportSources, now: &str) -> Value {
    let info = sources.info.as_ref();
    let integrity = super::integrity(
        super::integrity::Sources {
            info: sources.info.clone(),
            update: sources.update.clone(),
            storage: sources.storage.clone(),
            ssh_keys: None,
            custom_ui: None,
        },
        now,
    );
    let items: Vec<Value> = integrity["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|item| {
            json!({
                "key": text(&item["key"]),
                "state": text(&item["state"]),
                "detailCode": text(&item["detailCode"]),
            })
        })
        .collect();
    let data_state = items
        .iter()
        .find(|item| item["key"] == json!("dataReady"))
        .map_or(Value::Null, |item| item["state"].clone());
    json!({
        "schemaVersion": SCHEMA_VERSION,
        "generatedAt": now,
        "device": section(info, |info| json!({
            "board": reported(&info["board"], "model"),
            "kernel": reported(&info["kernel"], "release"),
        })),
        "software": section(info, |info| json!({
            "product": reported(&info["release"], "imageId"),
            "productVersion": reported(&info["release"], "imageVersion"),
            "osVersion": reported(&info["release"], "versionId"),
            "core": reported(&info["system"], "version"),
            "daemon": text(&info["daemon"]["version"]),
        })),
        "deployment": match info.map(|info| &info["deployment"]) {
            None => absent("source_failed"),
            Some(deployment) if deployment["available"] != json!(true) => absent("unavailable"),
            Some(deployment) => json!({
                "available": true,
                "id": text(&deployment["id"]),
                "version": text(&deployment["version"]),
                "generation": number(&deployment["generation"]),
                "confirmed": boolean(&deployment["confirmed"]),
            }),
        },
        "uptimeSeconds": info.map_or(Value::Null, |info| {
            if info["uptime"]["available"] == json!(true) {
                number(&info["uptime"]["seconds"])
            } else {
                Value::Null
            }
        }),
        "time": if sources.time.is_none() && sources.timezone.is_none() {
            absent("source_failed")
        } else {
            let time = sources.time.unwrap_or(Value::Null);
            json!({
                "available": true,
                "timezone": sources.timezone.as_ref().map_or(Value::Null, text),
                "status": text(&time["status"]),
                "synchronized": boolean(&time["synchronized"]),
            })
        },
        "integrity": { "available": true, "items": items },
        "data": match sources.storage.as_ref() {
            None => absent("source_failed"),
            Some(storage) => match storage["tiers"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|tier| tier["name"] == json!("data"))
            {
                None => absent("unavailable"),
                Some(data) => json!({
                    "available": true,
                    "state": data_state,
                    "totalBytes": number(&data["space"]["totalBytes"]),
                    "usedBytes": number(&data["space"]["usedBytes"]),
                    "freeBytes": number(&data["space"]["freeBytes"]),
                    "usedPercent": number(&data["space"]["usedPercent"]),
                }),
            },
        },
        "lastUpdateError": match sources.update.as_ref() {
            None => absent("source_failed"),
            Some(update) => {
                let lifecycle = &update["lifecycle"];
                let (code, during) = if lifecycle["state"] == json!("failed") {
                    (text(&lifecycle["code"]), text(&lifecycle["failedDuring"]))
                } else if let Some(failed) = lifecycle.get("coreCheckFailed") {
                    (text(&failed["code"]), json!("core_check"))
                } else if update["install"]["status"] == json!("failed") {
                    (text(&update["install"]["error_code"]), json!("install"))
                } else {
                    (Value::Null, Value::Null)
                };
                json!({ "available": true, "code": code, "during": during })
            }
        },
    })
}

fn absent(reason: &str) -> Value {
    json!({ "available": false, "reason": reason })
}

/// A section built from `source`, or absent when the source failed.
fn section(source: Option<&Value>, build: impl FnOnce(&Value) -> Value) -> Value {
    let Some(source) = source else {
        return absent("source_failed");
    };
    let mut built = build(source);
    built["available"] = json!(true);
    built
}

/// `member[key]` of a member that carries `available: true`.
fn reported(member: &Value, key: &str) -> Value {
    if member["available"] == json!(true) {
        text(&member[key])
    } else {
        Value::Null
    }
}

/// A string as one line of at most [`MAX_TEXT_CHARS`] characters, every
/// control character a space, so no value can start a line of its own in
/// the text a console builds from it; anything else is null.
fn text(value: &Value) -> Value {
    value.as_str().map_or(Value::Null, |text| {
        json!(
            text.chars()
                .take(MAX_TEXT_CHARS)
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
        )
    })
}

fn number(value: &Value) -> Value {
    if value.is_number() {
        value.clone()
    } else {
        Value::Null
    }
}

fn boolean(value: &Value) -> Value {
    value.as_bool().map_or(Value::Null, Value::from)
}

/// `mica-support-<UTC time>.json` for the summary generated at `generated_at`.
pub(super) fn file_name(generated_at: &str) -> String {
    let stamp = DateTime::parse_from_rfc3339(generated_at).map_or_else(
        |_| "unknown".to_string(),
        |at| at.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ").to_string(),
    );
    format!("mica-support-{stamp}.json")
}

/// Read the support summary.
///
/// The facts a support case opens with, from what micad already answers:
/// `device` (board model, kernel release), `software` (product and its
/// version, OS, core and daemon versions), `deployment` (id, version,
/// generation, confirmed), `uptimeSeconds`, `time` (the stored zone and the
/// synchronization status), `integrity` (each item's `key`, `state` and
/// `detailCode`, as `GET /api/v1/system/integrity` derives them), `data`
/// (DATA's readiness and space) and `lastUpdateError` (the failed update
/// step's `code` and `during`). Each member is picked by name: the machine
/// id, the host name, network names and addresses, device and claim codes,
/// credentials, logs and settings are not among them. A section whose source
/// failed is `{available: false, reason}`. Text values are single lines.
/// Served as an attachment named `mica-support-<UTC time>.json`, never
/// cached; nothing is sent anywhere.
#[utoipa::path(
    get,
    path = V1_SYSTEM_SUPPORT_INFO_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "The summary (`schemaVersion` names its shape); `Content-Disposition: attachment`", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_system_support_info(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let api = &state.api;
    let (info, update, storage, time, timezone) = tokio::join!(
        api.get_system_info(),
        api.get_update_state(),
        api.get_storage_status(),
        api.get_time_status(),
        api.get_settings("time.timezone"),
    );
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let summary = support_info(
        SupportSources {
            info: info.ok(),
            update: update.ok(),
            storage: storage.ok(),
            time: time.ok(),
            timezone: timezone.ok(),
        },
        &now,
    );
    let mut response = api_response(StatusCode::OK, ResourceValue(summary));
    if let Ok(disposition) =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", file_name(&now)))
    {
        response
            .headers_mut()
            .insert(CONTENT_DISPOSITION, disposition);
    }
    response
}

#[cfg(test)]
mod tests;

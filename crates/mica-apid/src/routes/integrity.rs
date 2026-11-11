//! The integrity card (`GET /api/v1/system/integrity`): one list of checks
//! derived from what micad already answers -- the system information, the
//! update state and the storage status. Nothing is verified again here: an
//! item states what its source reported, and `checkedAt` is when the sources
//! were read, not when the boot evidence was made.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};

use super::*;

/// A last check older than this is stale: its answer is no longer news.
const STALE_AFTER_DAYS: i64 = 7;

/// What the card is derived from; `None` is a read that failed.
pub(super) struct Sources {
    pub(super) info: Option<Value>,
    pub(super) update: Option<Value>,
    pub(super) storage: Option<Value>,
    pub(super) ssh_keys: Option<usize>,
    pub(super) custom_ui: Option<bool>,
}

/// The card: `checkedAt`, the `items` in their fixed order, each
/// `{key, state, value, detailCode}` and some a member of their own, and the
/// neutral `extensions` that could be read.
pub(super) fn integrity(sources: Sources, now: &str) -> Value {
    let deployment = sources.info.as_ref().map(|info| &info["deployment"]);
    let mut items = vec![
        trust_grade(sources.info.as_ref()),
        flag(deployment, "secureBoot", "disabled"),
        flag(deployment, "bootVerified", "not_verified"),
        flag(deployment, "contentVerified", "not_verified"),
        confirmed(
            deployment,
            sources
                .update
                .as_ref()
                .and_then(|update| update.get("lastRollback")),
        ),
        update_status(sources.update.as_ref(), now),
        data_ready(sources.storage.as_ref()),
    ];
    for item in &mut items {
        if let Some(object) = item.as_object_mut() {
            object.entry("value").or_insert(Value::Null);
            object.entry("detailCode").or_insert(Value::Null);
        }
    }
    let mut extensions = Vec::new();
    if let Some(count) = sources.ssh_keys {
        extensions.push(json!({ "key": "sshAuthorizedKeys", "value": count }));
    }
    if let Some(custom) = sources.custom_ui {
        extensions.push(json!({ "key": "customUi", "value": custom }));
    }
    json!({ "checkedAt": now, "items": items, "extensions": extensions })
}

fn item(key: &str, state: &str, detail: Option<&str>) -> Value {
    json!({ "key": key, "state": state, "detailCode": detail })
}

fn trust_grade(info: Option<&Value>) -> Value {
    const KEY: &str = "trustGrade";
    let Some(info) = info else {
        return item(KEY, "unknown", Some("source_failed"));
    };
    let trust = &info["trust"];
    match trust["grade"].as_str() {
        Some("production") if trust["available"] == json!(true) => {
            json!({ "key": KEY, "state": "ok", "value": "production" })
        }
        Some("development") if trust["available"] == json!(true) => json!({
            "key": KEY,
            "state": "attention",
            "value": "development",
            "detailCode": "development",
            "domains": trust["developmentDomains"].clone(),
        }),
        _ => item(KEY, "unknown", Some("unavailable")),
    }
}

/// The running deployment, when micad reported one.
fn booted(deployment: Option<&Value>) -> Result<&Value, Value> {
    match deployment {
        None => Err(json!("source_failed")),
        Some(deployment) if deployment["available"] == json!(true) => Ok(deployment),
        Some(_) => Err(json!("unavailable")),
    }
}

/// A boolean of the running deployment: true is ok, false needs
/// `attention` for `when_false`, absent is unknown.
fn flag(deployment: Option<&Value>, key: &str, when_false: &str) -> Value {
    match booted(deployment).map(|deployment| deployment[key].as_bool()) {
        Ok(Some(true)) => json!({ "key": key, "state": "ok", "value": true }),
        Ok(Some(false)) => {
            json!({ "key": key, "state": "attention", "value": false, "detailCode": when_false })
        }
        Ok(None) => item(key, "unknown", Some("unavailable")),
        Err(code) => json!({ "key": key, "state": "unknown", "detailCode": code }),
    }
}

/// Whether the running deployment is the confirmed one; the value is its
/// generation. In the boot after a rollback, `rollback` is whether it took,
/// as micad decided it.
fn confirmed(deployment: Option<&Value>, last_rollback: Option<&Value>) -> Value {
    const KEY: &str = "deploymentConfirmed";
    let mut item = match booted(deployment) {
        Ok(deployment) => match deployment["confirmed"].as_bool() {
            Some(true) => json!({ "key": KEY, "state": "ok", "value": deployment["generation"] }),
            Some(false) => json!({
                "key": KEY, "state": "attention", "value": deployment["generation"],
                "detailCode": "trial_boot",
            }),
            None => item(KEY, "unknown", Some("unavailable")),
        },
        Err(code) => json!({ "key": KEY, "state": "unknown", "detailCode": code }),
    };
    if let Some(outcome) = last_rollback {
        item["rollback"] = outcome.clone();
    }
    item
}

/// The last check of the update lifecycle: a failed or never-run check, a
/// failed core line or a stale answer is unknown, a newer version needs
/// attention. `checkedAt` is the check's own time.
fn update_status(update: Option<&Value>, now: &str) -> Value {
    const KEY: &str = "updateStatus";
    let Some(update) = update else {
        return item(KEY, "unknown", Some("source_failed"));
    };
    let lifecycle = &update["lifecycle"];
    let last_check = lifecycle["last_check"].as_str();
    let mut item =
        if lifecycle["state"] == json!("failed") && lifecycle["failedDuring"] == json!("check") {
            item(KEY, "unknown", Some("check_failed"))
        } else if lifecycle.get("coreCheckFailed").is_some() {
            item(KEY, "unknown", Some("core_check_failed"))
        } else if last_check.is_none() {
            item(KEY, "unknown", Some("never_checked"))
        } else if last_check.is_some_and(|at| stale(at, now)) {
            item(KEY, "unknown", Some("stale"))
        } else if let Some(available) = lifecycle.get("available") {
            json!({
                "key": KEY, "state": "attention", "value": available["version"],
                "detailCode": "update_available",
            })
        } else {
            json!({ "key": KEY, "state": "ok" })
        };
    if let Some(at) = last_check {
        item["checkedAt"] = json!(at);
    }
    item
}

fn stale(at: &str, now: &str) -> bool {
    match (
        DateTime::parse_from_rfc3339(at),
        DateTime::parse_from_rfc3339(now),
    ) {
        (Ok(at), Ok(now)) => now - at > chrono::Duration::days(STALE_AFTER_DAYS),
        // A time that does not parse is no evidence of a recent check.
        _ => true,
    }
}

/// The DATA tier: mounted and writable is ok.
fn data_ready(storage: Option<&Value>) -> Value {
    const KEY: &str = "dataReady";
    let Some(storage) = storage else {
        return item(KEY, "unknown", Some("source_failed"));
    };
    let data = storage["tiers"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|tier| tier["name"] == json!("data"));
    match data {
        Some(data) if data["present"] == json!(true) => {
            if data["mounted"] != json!(true) {
                item(KEY, "attention", Some("not_mounted"))
            } else if data["readOnly"] == json!(true) {
                item(KEY, "attention", Some("read_only"))
            } else {
                json!({ "key": KEY, "state": "ok" })
            }
        }
        _ => item(KEY, "unknown", Some("unavailable")),
    }
}

/// Read the integrity card.
///
/// One list of checks, derived at request time from what micad already
/// answers: the trust grade of the image's material, Secure Boot, boot and
/// content verification and confirmation of the running deployment, the last
/// update check, and whether DATA is mounted writable. Each item is
/// `{key, state, value, detailCode}`: `state` is `ok`, `attention` or
/// `unknown`, and `detailCode` a closed code the console translates
/// (`disabled`, `not_verified`, `trial_boot`, `development`, `check_failed`,
/// `core_check_failed`, `never_checked`, `stale`, `update_available`,
/// `not_mounted`, `read_only`, `unavailable`, `source_failed`).
/// `trustGrade` carries the development `domains`, `updateStatus` the
/// check's own `checkedAt`, and `deploymentConfirmed`, in the boot after a
/// rollback, `rollback`: whether it took (`outcome` `rolled_back` or
/// `did_not_take`, the target and, when it did not take, what booted and
/// the `reason` the deployment state gives). A source that cannot be read leaves its items
/// `unknown` (`source_failed`) and the rest stand. `checkedAt` is when the
/// sources were read: nothing is verified again. `extensions` lists, neutrally,
/// the number of SSH authorized keys and whether a custom console is
/// installed. Read-only, and no secret is read for it.
#[utoipa::path(
    get,
    path = V1_SYSTEM_INTEGRITY_PATH,
    context_path = API,
    tag = "resources",
    responses(
        (status = 200, description = "`checkedAt`, the `items` and the `extensions`", body = ResourceValue),
        (status = 401, description = "No stored bearer token or authenticated browser session (`not_authenticated`)", body = ApiError),
        (status = 405, description = "A method this route does not serve (`method_not_allowed`); carries `Allow`", body = ApiError),
    ),
)]
pub(crate) async fn api_v1_system_integrity(
    _credential: ApiCredential,
    State(state): State<AppState>,
) -> Response {
    let api = &state.api;
    let (info, update, storage, keys) = tokio::join!(
        api.get_system_info(),
        api.get_update_state(),
        api.get_storage_status(),
        api.get_settings("access.ssh.authorizedKeys"),
    );
    let bundles = Arc::clone(&state.bundles);
    let custom_ui = tokio::task::spawn_blocking(move || super::ui::ui_status(&bundles))
        .await
        .ok()
        .and_then(Result::ok)
        .map(|status| status.mode != "builtIn");
    let sources = Sources {
        info: info.ok(),
        update: update.ok(),
        storage: storage.ok(),
        ssh_keys: keys.ok().and_then(|keys| keys.as_array().map(Vec::len)),
        custom_ui,
    };
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    api_response(StatusCode::OK, ResourceValue(integrity(sources, &now)))
}

#[cfg(test)]
mod tests;

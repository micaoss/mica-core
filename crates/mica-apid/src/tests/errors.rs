//! How micad's failures reach a client.

use crate::routes::{AppState, app};
use crate::settings_api::SettingsApi;
use axum::Router;
use axum::http::StatusCode;
use axum::http::header::LOCATION;
use serde_json::json;
use std::sync::Arc;

use super::*;

// A `zbus::Error::MethodError` naming `name`, with `message` as the body micad
// sent back.
pub(super) fn method_error(name: &'static str, message: &str) -> zbus::Error {
    let reply_to = zbus::message::Message::method_call("/com/mica/micad", "GetSettings")
        .expect("a well-formed method call")
        .build(&())
        .expect("an empty body serialises");
    let name = zbus::names::ErrorName::try_from(name).expect("a well-formed fdo error name");
    zbus::Error::MethodError(name.into(), Some(message.to_string()), reply_to)
}

// A [`SettingsApi`] whose resource reads fail with the error the test chose.
//
// `access` and the whole tree still read, because that is what the gate and
// `login_submit` need to get a session as far as a route that fails.
pub(super) struct FailingSettings {
    pub(super) tree: serde_json::Value,
    /// The fdo error name micad answered with, or `None` for a failure that
    /// never reached micad at all.
    pub(super) fdo_name: Option<&'static str>,
    /// Fail with the bounded call's own error type instead. Its own field
    /// rather than a third `fdo_name` value because a timeout is not an fdo
    /// error at all: it is apid's, raised when micad never answered, and the
    /// classification that reads it downcasts to the type rather than reading
    /// a name.
    pub(super) timeout: bool,
}

impl FailingSettings {
    pub(super) fn error(&self) -> anyhow::Error {
        if self.timeout {
            return anyhow::Error::new(crate::bus_client::MicadCallTimeout::new(
                "GetState",
                std::time::Duration::from_secs(5),
            ));
        }
        match self.fdo_name {
            Some(name) => method_error(name, MICAD_MESSAGE).into(),
            None => anyhow::anyhow!("no connection to micad"),
        }
    }
}

// The text micad is pretending to have sent, which apid carries through
// untouched.
pub(super) const MICAD_MESSAGE: &str =
    "invalid settings value at `network.eth0.100`: unknown field `100`";

#[async_trait::async_trait]
impl SettingsApi for FailingSettings {
    async fn get_settings(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        if path.is_empty() || path == "access" {
            return Ok(if path.is_empty() {
                self.tree.clone()
            } else {
                self.tree["access"].clone()
            });
        }
        Err(self.error())
    }

    /// The settings root stopped being read-only, and this
    /// fixture answers the write the same way it answers a read: the
    /// classification is exactly what the write route has to inherit.
    async fn set_settings(
        &self,
        _path: &str,
        _value: &serde_json::Value,
    ) -> anyhow::Result<String> {
        Err(self.error())
    }

    async fn get_state(&self, _path: &str) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    async fn get_time_status(&self) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    async fn get_storage_status(&self) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    async fn get_system_info(&self) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    async fn get_telemetry(&self) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    async fn get_observed_network(&self) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    async fn get_failure_evidence(&self) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }
    async fn get_log(&self, _source: &str) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    /// Power routes must propagate the same dispatch failures as other actions.
    async fn reboot(&self) -> anyhow::Result<()> {
        Err(self.error())
    }

    async fn power_off(&self) -> anyhow::Result<()> {
        Err(self.error())
    }

    async fn set_transient_root_password(&self, _password: &str) -> anyhow::Result<String> {
        Err(self.error())
    }

    /// The one write this fixture *does* answer, because the classification
    /// is exactly what the rotate route has to inherit from the read routes.
    async fn rotate_wireguard_key(&self, _iface: &str) -> anyhow::Result<String> {
        Err(self.error())
    }

    // The update cluster inherits the same classification through
    // `update_bus_error`, so this fixture answers all six the same way.
    async fn get_update_state(&self) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    async fn check_update(&self) -> anyhow::Result<()> {
        Err(self.error())
    }

    async fn fetch_update(&self) -> anyhow::Result<()> {
        Err(self.error())
    }

    async fn install_update(&self, _bundle: &str) -> anyhow::Result<()> {
        Err(self.error())
    }

    async fn confirm_deployment(&self, _deployment_id: &str) -> anyhow::Result<()> {
        Err(self.error())
    }

    async fn reject_deployment(&self, _deployment_id: &str) -> anyhow::Result<()> {
        Err(self.error())
    }

    async fn rollback_deployment(&self, _deployment_id: &str) -> anyhow::Result<()> {
        Err(self.error())
    }

    async fn set_reboot_override(&self, _seconds: u32) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }

    async fn set_update_config(
        &self,
        _patch: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        Err(self.error())
    }
}

// A router whose resource reads fail the way `fdo_name` says, plus a session
// cookie for it.
pub(super) async fn failing_app(fdo_name: Option<&'static str>) -> (Router, String) {
    // Seed the automation credential directly because this fixture refuses
    // the settings write a token mint would require.
    let (entry, wire) = seeded_token(0);
    let mut tree = configured_tree("hunter2secret");
    tree["access"]["apiTokens"] = json!([entry]);
    let api = Arc::new(FailingSettings {
        tree,
        fdo_name,
        timeout: false,
    });
    let router = app(AppState::new(api, SIGNING_KEY));
    (router, wire)
}

// The same fixture, failing with the bounded call's timeout rather than with
// a connection failure.
pub(super) async fn timing_out_app() -> (Router, String) {
    let (entry, wire) = seeded_token(0);
    let mut tree = configured_tree("hunter2secret");
    tree["access"]["apiTokens"] = json!([entry]);
    let api = Arc::new(FailingSettings {
        tree,
        fdo_name: None,
        timeout: true,
    });
    let router = app(AppState::new(api, SIGNING_KEY));
    (router, wire)
}

// The premise the classification rests on: `err.into` in `bus_client.rs`
// converts a `zbus::Error` to `anyhow::Error` through the blanket `From`,
// which STORES the concrete error rather than flattening it, so the fdo name
// is still there to be recovered. If this ever stops holding, every row of
// the table below collapses into the fallback and the tests would say so
// one at a time; this says it once, in the one sentence it depends on.
#[test]
pub(super) fn the_zbus_error_survives_the_conversion_to_anyhow() {
    let err: anyhow::Error = method_error("org.freedesktop.DBus.Error.InvalidArgs", "boom").into();
    let recovered = err
        .downcast_ref::<zbus::Error>()
        .expect("the conversion kept the zbus error");
    match recovered {
        zbus::Error::MethodError(name, message, _) => {
            assert_eq!(name.as_str(), "org.freedesktop.DBus.Error.InvalidArgs");
            assert_eq!(message.as_deref(), Some("boom"));
        }
        other => panic!("the variant changed: {other:?}"),
    }
}

// The table, row by row: micad classifies, apid translates the
// classification, and micad's message is carried through verbatim.
#[tokio::test]
pub(super) async fn each_fdo_error_name_gets_its_own_envelope() {
    for (fdo_name, code, status) in [
        (
            "com.mica.micad1.Error.NotFound",
            "settings_not_found",
            StatusCode::NOT_FOUND,
        ),
        (
            "com.mica.micad1.Error.ReadOnly",
            "settings_read_only",
            StatusCode::CONFLICT,
        ),
        (
            "org.freedesktop.DBus.Error.InvalidArgs",
            "settings_rejected",
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            "org.freedesktop.DBus.Error.IOError",
            "settings_io",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
        (
            "org.freedesktop.DBus.Error.Failed",
            "micad_failed",
            StatusCode::INTERNAL_SERVER_ERROR,
        ),
    ] {
        for path in ["/api/v1/settings/wifi.ap", "/api/v1/state/wifiAp"] {
            let (router, token) = failing_app(Some(fdo_name)).await;
            let response = bearer(&router, "GET", path, &token).await;
            assert_eq!(response.status(), status, "{fdo_name} at {path}");
            assert_api_headers(&response, path);
            assert_eq!(
                response.headers().get(axum::http::header::RETRY_AFTER),
                None,
                "only the unreachable class carries Retry-After: {fdo_name}"
            );
            let error = envelope(response).await;
            assert_eq!(error["code"], code, "{fdo_name}");
            assert_eq!(error["source"], "micad", "{fdo_name}");
            // apid substituting its own phrasing would hide every
            // message micad learns to produce.
            assert_eq!(error["message"], MICAD_MESSAGE, "{fdo_name}");
            // The optional member, which these routes DO name.
            assert_eq!(
                error["path"],
                json!(path.rsplit('/').next().unwrap()),
                "{fdo_name}"
            );
        }
    }
}

// The fallback row, and the only one whose `source` is apid: the call could
// not be made at all, which is a statement about this server rather than
// about the request. 503, because apid itself is up and answering.
#[tokio::test]
pub(super) async fn an_unreachable_micad_is_503_with_retry_after() {
    // No `MethodError` at all, and a `MethodError` under a name the table
    // does not list: both are the fallback.
    for fdo_name in [None, Some("org.freedesktop.DBus.Error.UnknownObject")] {
        for path in ["/api/v1/settings/wifi.ap", "/api/v1/state/wifiAp"] {
            let (router, token) = failing_app(fdo_name).await;
            let response = bearer(&router, "GET", path, &token).await;
            assert_eq!(
                response.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{fdo_name:?} at {path}"
            );
            assert_api_headers(&response, path);
            assert_eq!(
                header_value(&response, axum::http::header::RETRY_AFTER),
                "5",
                "{fdo_name:?} at {path}"
            );
            let error = envelope(response).await;
            assert_eq!(error["code"], "micad_unreachable");
            assert_eq!(error["source"], "apid");
            assert!(error["message"].is_string());
        }
    }
}

// The HTML half of the same condition: a pane whose micad call fails answers
// **503 with `Retry-After`**, exactly like the API path above, so one outage
// reports one status on both surfaces.

// A dot-path that does not exist answers **404 `settings_not_found`**, no
// longer 422: micad names `SettingsError::NotFound` with its own error name
// (`com.mica.micad1.Error.NotFound`), so a missing path and a bad value stop
// sharing a code. The 422 assertion beside it is the control: a rejection
// that IS a rejection still reports as one.
#[tokio::test]
pub(super) async fn a_dot_path_that_does_not_exist_is_404_and_a_rejection_stays_422() {
    let (router, token) = failing_app(Some("com.mica.micad1.Error.NotFound")).await;
    let response = bearer(&router, "GET", "/api/v1/settings/no.such.path", &token).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_not_found");
    assert_eq!(error["path"], json!("no.such.path"));

    let (router, token) = failing_app(Some("org.freedesktop.DBus.Error.InvalidArgs")).await;
    let response = bearer(&router, "GET", "/api/v1/settings/no.such.path", &token).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_rejected");
    assert_eq!(error["path"], json!("no.such.path"));
}

// A live-state dot-path that does not resolve answers **404
// `settings_not_found`**, the same code the settings tree gives the same
// condition -- not the 422 the table gives fdo `InvalidArgs`.
#[tokio::test]
pub(super) async fn a_state_dot_path_that_does_not_resolve_is_404_not_422() {
    let (router, token) = failing_app(Some("com.mica.micad1.Error.NotFound")).await;

    let response = bearer(&router, "GET", "/api/v1/state/no.such.path", &token).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_not_found");
    assert_eq!(error["path"], json!("no.such.path"));

    let (router, token) = failing_app(Some("org.freedesktop.DBus.Error.InvalidArgs")).await;
    let response = bearer(&router, "GET", "/api/v1/state/no.such.path", &token).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_rejected");

    let response = bearer(&router, "GET", "/api/v1/settings/no.such.path", &token).await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let error = envelope(response).await;
    assert_eq!(error["code"], "settings_rejected");
}

// The trap again, for the resource routes: an unauthenticated
// resource read answers the error envelope with a 401 and **never** a redirect,
// in both gate modes. They inherit it from `ApiSession`; inheriting is not
// the same as being asserted.
#[tokio::test]
pub(super) async fn the_resource_routes_are_401_without_a_session_in_both_gate_modes() {
    const PATHS: [&str; 2] = ["/api/v1/settings/hostname", "/api/v1/state/hostname"];

    let (configured, _) = test_app(secret_tree("hunter2secret"));
    let (fresh, _) = test_app(unconfigured_tree());

    for (mode, router) in [("configured", &configured), ("setup mode", &fresh)] {
        for path in PATHS {
            let response = get(router, path, None).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{path} in {mode}"
            );
            assert_eq!(
                response.headers().get(LOCATION),
                None,
                "{path} in {mode} answered a redirect, which a script reads as success"
            );
            assert_api_headers(&response, path);
            let error = envelope(response).await;
            assert_eq!(error["code"], "not_authenticated", "{path} in {mode}");
            assert_eq!(error["source"], "apid", "{path} in {mode}");
            // The `path` is the dot-path at fault, and a request that failed
            // to authenticate never named one: the read did not happen.
            assert_eq!(error.get("path"), None, "{path} in {mode}");
        }
    }
}

// The three spellings of each resource root are one string plus two suffixes.
//
// The router, the OpenAPI attribute and the gate predicate each need a
// different one, and a typo in any of them would serve a path the document
// does not describe or hand off a path the router does not have.
#[test]
pub(super) fn the_resource_path_spellings_agree() {
    for (prefix, route, doc) in [
        crate::routes::SETTINGS_SPELLINGS,
        crate::routes::STATE_SPELLINGS,
    ] {
        assert_eq!(route, format!("{prefix}{{*path}}"));
        assert_eq!(doc, format!("{prefix}{{path}}"));
    }
}

// The admin password can be changed after setup, on both surfaces.

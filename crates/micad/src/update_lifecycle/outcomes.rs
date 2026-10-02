//! What the deployment client's answers mean.

use crate::update_codes::{self, CodedReason};
use anyhow::Result;
use serde_json::Value;
use std::path::Path;

use super::*;

/// The candidate the last `check` selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Available {
    pub deployment_id: String,
    pub version: String,
}

/// What a finished `check` said.
#[derive(Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    Selected(Available),
    /// The signed catalog selects no newer deployment for this device.
    NoneCompatible,
}

pub(super) fn client_json(operation: &str, output: &ClientOutput) -> Result<Value, CodedReason> {
    if output.code != Some(0) {
        return Err(exit_reason(operation, output.code, &output.stderr));
    }
    serde_json::from_str(&output.stdout).map_err(|error| {
        CodedReason::new(
            update_codes::CLIENT_OUTPUT_UNPARSEABLE,
            format!("invalid {operation} JSON: {error}"),
        )
    })
}

/// The helper authenticates the catalog before returning its exact selection.
pub fn parse_check(output: &ClientOutput) -> Result<CheckOutcome, CodedReason> {
    let value = client_json("check", output)?;
    if value.get("selected") == Some(&Value::Null) {
        return Ok(CheckOutcome::NoneCompatible);
    }
    let parsed = (|| {
        let id = value.pointer("/selected/deploymentId")?.as_str()?;
        let version = value.pointer("/selected/deployment/version")?.as_str()?;
        if !crate::deployment::valid_id(id) || version.is_empty() {
            return None;
        }
        Some(Available {
            deployment_id: id.to_string(),
            version: version.into(),
        })
    })();
    parsed.map(CheckOutcome::Selected).ok_or_else(|| {
        CodedReason::new(
            update_codes::CLIENT_OUTPUT_UNPARSEABLE,
            "check did not return a deployment selection",
        )
    })
}

/// What an awaited lifecycle operation did, in the four answers the recorded
/// state distinguishes.
#[derive(Debug, PartialEq, Eq)]
pub enum Settled<T> {
    /// The operation produced its result: a selected candidate, a staged
    /// descriptor path.
    Done(T),
    /// Nothing published is compatible — the device is up to date, or the
    /// source holds nothing newer for this product than the running system.
    NoneCompatible,
    /// The `/mica/updates` workspace refused it before anything was acquired.
    Unready(Unready),
    /// It failed, with the same code and reason recorded beside the `failed`
    /// state.
    Failed(CodedReason),
}

/// Native workspace preflight refused acquisition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unready {
    pub status: String,
    /// One of [`crate::update_codes`]'s five workspace codes, or
    /// `unknown` — never the word the client printed. `&'static str` is what
    /// makes that structural: this field cannot hold a client's string.
    pub kind: &'static str,
    pub detail: String,
}

impl Unready {
    /// The reason string recorded beside `update-unavailable`.
    pub fn reason(&self) -> String {
        format!("{} {}: {}", self.status, self.kind, self.detail)
    }
}

/// What a finished `probe` said.
#[derive(Debug, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// The workspace is ready; the bounded native JSON report is preserved.
    Ready(Value),
    Unready(Unready),
}

/// Parse the bounded native readiness report.
pub fn parse_probe(output: &ClientOutput) -> Result<ProbeOutcome, CodedReason> {
    if output.code != Some(0) {
        return Ok(ProbeOutcome::Unready(Unready {
            status: "degraded".into(),
            kind: update_codes::WORKSPACE_PROBE_FAILED,
            detail: output.stderr.trim().into(),
        }));
    }
    let value = client_json("probe", output)?;
    if value.get("status").and_then(Value::as_str) != Some("ready") {
        return Err(CodedReason::new(
            update_codes::CLIENT_OUTPUT_UNPARSEABLE,
            "probe did not report a ready workspace",
        ));
    }
    Ok(ProbeOutcome::Ready(value))
}

/// What a finished `fetch` said.
#[derive(Debug, PartialEq, Eq)]
pub enum FetchOutcome {
    /// A verified descriptor at this path, inside `verified/`.
    Staged(String),
    /// No deployment was selected.
    NoneCompatible,
}

/// Accept only the descriptor and object directory returned by native acquisition.
pub fn parse_fetch(
    output: &ClientOutput,
    verified_dir: &Path,
) -> Result<FetchOutcome, CodedReason> {
    let value = client_json("fetch", output)?;
    if value.is_null() {
        return Ok(FetchOutcome::NoneCompatible);
    }
    let path = (|| {
        let id = value.get("id")?.as_str()?;
        let path = Path::new(value.get("path")?.as_str()?);
        let objects = Path::new(value.get("objects")?.as_str()?);
        if !crate::deployment::valid_id(id)
            || path != verified_dir.join(format!("{id}.json"))
            || objects != verified_dir.join("objects")
        {
            return None;
        }
        Some(path.to_string_lossy().into_owned())
    })();
    path.map(FetchOutcome::Staged).ok_or_else(|| {
        CodedReason::new(
            update_codes::UNVERIFIED_DEPLOYMENT_PATH,
            "fetch did not return a verified deployment descriptor",
        )
    })
}

/// How a client operation did not produce its outcome: the workspace refused
/// it (a state), or it failed (a reason).
pub(super) enum Failure {
    Unready(Unready),
    Error(CodedReason),
}

/// A client invocation that ended badly, coded at the site.
///
/// The two codes are not the same fault and a fleet must be able to tell them
/// apart: an exit status is the client having run and judged something, while
/// a signal is the bound in this module (or the OOM killer) having stopped it
/// before it judged anything.
pub(super) fn exit_reason(verb: &str, code: Option<i32>, stderr: &str) -> CodedReason {
    let reason = stderr.lines().last().unwrap_or("").trim();
    match code {
        Some(code) if !reason.is_empty() => CodedReason::new(
            update_codes::CLIENT_EXIT_FAILURE,
            format!("{verb} failed (exit {code}): {reason}"),
        ),
        Some(code) => CodedReason::new(
            update_codes::CLIENT_EXIT_FAILURE,
            format!("{verb} failed with exit {code}"),
        ),
        None if !reason.is_empty() => CodedReason::new(
            update_codes::CLIENT_SPAWN_FAILED,
            format!("{verb} was killed by a signal: {reason}"),
        ),
        None => CodedReason::new(
            update_codes::CLIENT_SPAWN_FAILED,
            format!("{verb} was killed by a signal"),
        ),
    }
}

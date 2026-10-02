//! Apply tasks across a resubscribe.

use crate::routes::{AppState, app};
use crate::settings_api::FakeSettings;
use crate::task_registry::TaskRecord;
use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;

use super::*;

#[tokio::test]
pub(super) async fn a_running_task_missing_after_resubscribe_becomes_interrupted() {
    let (tree, token) = with_token(ssh_tree(json!([])));
    let fake = Arc::new(FakeSettings::new(tree));
    let state = AppState::new(fake, SIGNING_KEY);
    let registry = state.task_registry().clone();
    registry.subscribed();
    registry.update(TaskRecord {
        id: "task-before-restart".to_string(),
        operation: "settings-write".to_string(),
        dot_path: "access.ssh.enabled".to_string(),
        source: ":1.9".to_string(),
        status: "running".to_string(),
        enqueued_at: "2026-08-31T00:00:00.000Z".to_string(),
        started_at: Some("2026-08-31T00:00:01.000Z".to_string()),
        finished_at: None,
        outcome: None,
        message: None,
        folded_count: 0,
    });
    registry.lapsed();
    registry.subscribed();
    assert_eq!(registry.get("task-before-restart"), None);

    let router = app(state);
    let response = bearer(&router, "GET", "/api/v1/tasks/task-before-restart", &token).await;
    assert_eq!(response.status(), StatusCode::OK);
    let task = body_json(response).await;
    assert_eq!(task["status"], "finished", "{task}");
    assert_eq!(task["outcome"], "interrupted", "{task}");
    assert!(task["finishedAt"].as_str().is_some(), "{task}");
}

// The asset router: route precedence, the reserved `/api/` subtree, the SPA
// fallback and the headers as applied.

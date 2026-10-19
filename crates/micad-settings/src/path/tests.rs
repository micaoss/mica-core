use serde_json::json;

use super::*;

/// An empty path names no node to write: a refusal, never a panic.
#[test]
fn a_write_at_an_empty_path_is_refused() {
    let mut root = json!({});
    let error = json_path_set(&mut root, &[], json!(1)).expect_err("an empty path");
    assert!(matches!(error, SettingsError::Validation { .. }), "{error}");
    assert_eq!(root, json!({}));
}

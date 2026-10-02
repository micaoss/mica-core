//! The one spelling of "this could not be observed" in published state.

use serde_json::{Value, json};

/// `{"available": false, "detail": detail}`: the member a state or diagnostics
/// document carries where a reading could not be taken, so a reader tells a
/// missing source from an empty one.
pub fn absent(detail: impl Into<String>) -> Value {
    json!({ "available": false, "detail": detail.into() })
}

//! The diagnostic snapshot: collection, redaction and
//! the bounded on-disk store. The route layer is `routes.rs`.
//!
//! Three properties are enforced here rather than promised.

use std::time::Duration;

mod collect;
mod redaction;
mod schema;
mod store;
pub use collect::*;
pub use redaction::*;
use schema::*;
pub use store::*;

/// Snapshot schema for signed deployment and component evidence.
pub const SCHEMA_VERSION: u64 = 1;
/// Redaction allowlist for the native deployment schema.
pub const REDACTION_SCHEMA_VERSION: u64 = 1;
/// Persistent snapshot store in the system-owned DATA namespace.
pub const DEFAULT_ROOT: &str = "/mica/diagnostics";
/// The most snapshots retained; publishing one more removes the oldest.
pub const MAX_SNAPSHOTS: usize = 8;
/// The most bytes the store holds across all snapshots.
pub const MAX_TOTAL_BYTES: u64 = 16 * 1024 * 1024;
/// The most bytes one snapshot may be; larger is refused, not trimmed.
pub const MAX_SNAPSHOT_BYTES: usize = 2 * 1024 * 1024;
/// The whole collection's deadline.
pub const COLLECTION_DEADLINE: Duration = Duration::from_secs(20);
/// One section's timeout, inside the deadline. Above the bus client's own
/// 5 s bound so a micad timeout keeps its own classification.
pub const SECTION_TIMEOUT: Duration = Duration::from_secs(6);
/// The most bytes any one string in a snapshot keeps.
pub const MAX_TEXT_BYTES: usize = 1024;
/// The most failed tasks the `failures.tasks` member carries.
pub const MAX_FAILED_TASKS: usize = 32;
/// What a string carrying a secret marker becomes, whole.
pub const REDACTED_LINE: &str = "<redacted line>";
/// What a hardware-address-shaped token inside a string becomes.
pub const REDACTED_MAC: &str = "<mac>";
#[cfg(test)]
mod tests;

//! The signed boot policy: `/etc/mica/boot.json` in the kernel component's
//! initramfs, which the runkit reads at boot and copies to
//! `/run/mica/boot-policy.json` for the running system's `mica-deploy`.
//!
//! One type for both readers, so neither refuses a field the other accepts.

use serde::Deserialize;

use crate::board::BoardFacts;
use crate::components::BootIdentity;

/// The boot policy. Unknown fields are refused.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BootPolicy {
    pub identity: BootIdentity,
    pub board: BoardFacts,
    pub public_keys: Vec<String>,
    /// SYSTEM's and DATA's GPT partition UUIDs; a `ubi` board has neither.
    #[serde(default)]
    pub system_part_uuid: String,
    #[serde(default)]
    pub data_part_uuid: String,
    /// What the product asks of the system; absent asks nothing.
    #[serde(default)]
    pub product: crate::boot::trust::ProductPolicy,
    /// DATA encryption; absent is off.
    #[serde(default)]
    pub data_encryption: Option<crate::data_crypt::runkit::Encryption>,
}

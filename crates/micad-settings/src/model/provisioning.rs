//! Provisioning, the device identity and reset.

/// First-boot self-provisioning status.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProvisioningSettings {
    /// Whether first-boot provisioning has run to completion.
    pub state: ProvisioningState,
    /// Device identity assigned at first boot, lowercase hex.
    #[serde(rename = "deviceId", default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Seeding revision that produced this tree.
    #[serde(rename = "seededGeneration")]
    pub seeded_generation: u32,
    /// The provisioning-document record (schema v10); absent until a document
    /// has been offered to this device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub document: Option<ProvisioningDocumentSettings>,
}

/// What the last provisioning document did to this device.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProvisioningDocumentSettings {
    /// `version` of the document last APPLIED, absent when none ever was.
    ///
    /// The DOCUMENT's own schema version, which moves independently of
    /// [`SCHEMA_VERSION`]: a document format revision does not reshape the
    /// settings tree and a settings bump does not invalidate a document.
    #[serde(
        rename = "appliedVersion",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub applied_version: Option<u32>,
    /// Digest of the document last applied, lowercase hex.
    ///
    /// The short-circuit that makes a re-apply a no-op: an offered document
    /// whose digest equals this one is not applied again. Over a CANONICAL
    /// rendering of the parsed document, so a comment, a reordered key or a
    /// changed indentation in the source file is the same document.
    #[serde(
        rename = "appliedDigest",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub applied_digest: Option<String>,
    /// The last import ATTEMPT, applied or not.
    ///
    /// Distinct from the two fields above on purpose: a rejected document
    /// leaves them exactly as they were and lands only here, so a bad file on
    /// a stick can never make a device look configured by it.
    #[serde(
        rename = "lastImport",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_import: Option<ProvisioningImport>,
}

/// One provisioning-document import attempt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisioningImport {
    /// Which transport offered the document: `boot` or `media`.
    pub source: String,
    /// How it ended: `applied`, `unchanged` or `rejected`.
    pub outcome: String,
    /// Why it was rejected, naming the offending KEY PATH and never its value;
    /// absent for an outcome that is not a rejection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Seconds since the UNIX epoch as the device clock read them, saturating
    /// at 0.
    pub at: u64,
}

/// Stage of first-boot self-provisioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisioningState {
    /// The device has not provisioned itself yet.
    #[default]
    Pending,
    /// First-boot provisioning finished; the tree is the device's own.
    Complete,
}

/// A staged reset intent (schema v12).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetSettings {
    /// Which tier is staged. There is no parameterless reset.
    pub tier: ResetTier,
    /// Seconds since the UNIX epoch as the device clock read them when the
    /// intent committed, saturating at 0.
    pub requested: u64,
    /// The presence mechanism that authorized a presence-gated tier; absent
    /// for the tiers that are authenticated management actions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<String>,
}

/// Which reset tier is staged — the tier table, whose
/// rows are the whole of the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResetTier {
    /// Tier 1: return the modelled settings to their schema defaults.
    Configuration,
    /// Tier 2: remove operator applications and their data.
    ApplicationData,
    /// Tier 3: return the device to its first-boot state, keeping identity,
    /// calibration, META and both system slots.
    FullFactory,
}

/// Characters a device identifier occupies: 16 bytes spelled in lowercase hex.
pub const DEVICE_ID_LEN: usize = 32;

/// The shortest administrator bootstrap password a provisioning document may
/// carry.
pub const MIN_ADMIN_PASSWORD_LEN: usize = 8;

/// Refuse a `provisioning.deviceId` that is not the identifier
/// `micad`'s `identity` module mints.
pub fn validate_device_id(device_id: &str) -> Result<(), String> {
    if device_id.len() != DEVICE_ID_LEN
        || !device_id
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return Err(format!(
            "a device identifier is exactly {DEVICE_ID_LEN} lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

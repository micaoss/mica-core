//! `mica/set/v1`: the components of one kind, channel and architecture,
//! released once and taken by every device of that architecture on that
//! channel.
//!
//! A set is the same document for every kind. What a component is, which of
//! them a device takes, how the set takes effect and what makes it good are
//! the kind's own; `core`, the one kind there is, has them in `core_set`.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::components::{
    Artifact, ContractError, CoreComponent, MAX_CORE_COMPONENTS, MAX_INTEGER, authenticate_payload,
    integer, name, needs_met, require, sha256,
};

/// What a set is a set of. A kind decides what a component record is, which
/// of them a device takes, how a set takes effect and what makes it good;
/// everything else of a set is the same for every kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SetKind {
    /// mica-core's packages (`mica/core/v1`), composed over the root at boot.
    Core,
}

impl SetKind {
    /// The kind's name on the wire and on disk: lowercase letters and digits.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
        }
    }

    /// The kind `name` names, when this reader implements it.
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        (name == "core").then_some(Self::Core)
    }
}

impl std::fmt::Display for SetKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for SetKind {
    type Err = ContractError;
    fn from_str(name: &str) -> Result<Self> {
        Self::named(name).ok_or(ContractError("unsupported set kind"))
    }
}

type Result<T> = std::result::Result<T, ContractError>;

/// The components of one kind, channel and architecture (`mica/set/v1`), released once and taken
/// by every device of that architecture on that channel. Which of them a device takes is its
/// kind's rule (core: [`crate::core_set::select`]).
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSet {
    pub schema: String,
    /// Inside the signed payload: a set signed as one kind is never taken as another.
    pub kind: SetKind,
    /// `general`, or the name of a specific channel.
    pub channel: String,
    pub arch: String,
    /// Monotonic within the channel and architecture.
    pub generation: u64,
    pub version: String,
    /// Every component of the channel, a record of the kind's own schema; one per package,
    /// sorted by package.
    pub components: Vec<CoreComponent>,
}

pub const MAX_SET_BYTES: usize = 32768;

/// A channel name: lowercase letters, digits and hyphens, starting with a letter or digit.
fn channel(value: &str) -> Result<()> {
    require(
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "invalid set channel",
    )
}

impl UpdateSet {
    fn validate(&self, raw: &Value) -> Result<()> {
        require(self.schema == "mica/set/v1", "wrong set schema")?;
        channel(&self.channel)?;
        require(
            matches!(self.arch.as_str(), "amd64" | "arm64" | "armhf"),
            "unsupported architecture",
        )?;
        integer(self.generation, MAX_INTEGER)?;
        name(&self.version)?;
        require(!self.components.is_empty(), "a set carries no component")?;
        require(
            self.components.len() <= MAX_CORE_COMPONENTS,
            "too many components in a set",
        )?;
        require(
            self.components
                .windows(2)
                .all(|pair| pair[0].package < pair[1].package),
            "set components not unique and sorted by package",
        )?;
        for (index, core) in self.components.iter().enumerate() {
            core.validate(&self.arch, &raw["components"][index])?;
            needs_met(
                core,
                self.components.iter(),
                "a core component's need is not in the set",
            )?;
        }
        Ok(())
    }

    /// Every object the set names: each component's image and its signature.
    pub fn artifacts(&self) -> impl Iterator<Item = &Artifact> {
        self.components
            .iter()
            .flat_map(|core| [&core.content.image, &core.content.signature])
    }

    /// The set's own identity: the digest of its canonical payload.
    pub fn id(&self) -> Result<String> {
        // Through a `Value`, whose keys are sorted: the struct's own field
        // order is not the canonical form that was signed.
        let value = serde_json::to_value(self).map_err(|_| ContractError("invalid JSON"))?;
        Ok(sha256(
            &serde_json::to_vec(&value).map_err(|_| ContractError("invalid JSON"))?,
        ))
    }
}

/// Parse a compact, key-sorted `mica/set/v1` payload.
pub fn parse(payload: &[u8]) -> Result<UpdateSet> {
    require(payload.len() <= MAX_SET_BYTES, "set too large")?;
    let raw: Value = serde_json::from_slice(payload).map_err(|_| ContractError("invalid JSON"))?;
    let canonical = serde_json::to_vec(&raw).map_err(|_| ContractError("invalid JSON"))?;
    require(
        canonical == payload,
        "noncanonical or duplicate JSON fields",
    )?;
    // Before the fields: a kind this reader does not implement is said so,
    // whatever its components look like.
    if let Some(kind) = raw.get("kind").and_then(Value::as_str) {
        require(SetKind::named(kind).is_some(), "unsupported set kind")?;
    }
    let set: UpdateSet = serde_json::from_value(raw.clone())
        .map_err(|_| ContractError("unknown, missing or invalid fields"))?;
    set.validate(&raw)?;
    Ok(set)
}

/// A signed set: the release key's `mica/update-envelope/v1` around its payload.
pub fn authenticate(bytes: &[u8], public_keys: &[[u8; 32]]) -> Result<UpdateSet> {
    parse(&authenticate_payload(bytes, public_keys, MAX_SET_BYTES)?)
}

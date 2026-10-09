//! `mica/core-set/v1`: the core components of one channel and architecture,
//! released once and taken by every product of that architecture on that
//! channel, and the selection a device makes from it.
//!
//! A set carries every component of its channel. Which of them a device
//! composes is its product's choice ([`select`]): the one rule, applied by the
//! runkit before it maps anything, by acquisition before it fetches anything,
//! and by the producer before it publishes a set.
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::components::{
    Artifact, ContractError, CoreComponent, MAX_CORE_COMPONENTS, MAX_INTEGER, authenticate_payload,
    integer, name, needs_met, require, sha256,
};

type Result<T> = std::result::Result<T, ContractError>;

/// The core components of one channel and architecture (`mica/core-set/v1`), released once and
/// taken by every product of that architecture on that channel. A device composes the ones its
/// product's features select ([`select`]).
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSet {
    pub schema: String,
    /// `general`, or the name of a specific channel.
    pub channel: String,
    pub arch: String,
    /// Monotonic within the channel and architecture.
    pub generation: u64,
    pub version: String,
    /// Every core component of the channel, one per package, sorted by package.
    pub components: Vec<CoreComponent>,
}

pub const MAX_CORE_SET_BYTES: usize = 32768;

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
        "invalid core channel",
    )
}

impl CoreSet {
    fn validate(&self, raw: &Value) -> Result<()> {
        require(self.schema == "mica/core-set/v1", "wrong core set schema")?;
        channel(&self.channel)?;
        require(
            matches!(self.arch.as_str(), "amd64" | "arm64"),
            "unsupported architecture",
        )?;
        integer(self.generation, MAX_INTEGER)?;
        name(&self.version)?;
        require(
            !self.components.is_empty(),
            "a core set carries no component",
        )?;
        require(
            self.components.len() <= MAX_CORE_COMPONENTS,
            "too many core components",
        )?;
        require(
            self.components
                .windows(2)
                .all(|pair| pair[0].package < pair[1].package),
            "core components not unique and sorted by package",
        )?;
        for (index, core) in self.components.iter().enumerate() {
            core.validate(&self.arch, &raw["components"][index])?;
            needs_met(
                core,
                self.components.iter(),
                "a core component's need is not in the core set",
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

/// Parse a compact, key-sorted `mica/core-set/v1` payload.
pub fn parse(payload: &[u8]) -> Result<CoreSet> {
    require(payload.len() <= MAX_CORE_SET_BYTES, "core set too large")?;
    let raw: Value = serde_json::from_slice(payload).map_err(|_| ContractError("invalid JSON"))?;
    let canonical = serde_json::to_vec(&raw).map_err(|_| ContractError("invalid JSON"))?;
    require(
        canonical == payload,
        "noncanonical or duplicate JSON fields",
    )?;
    let set: CoreSet = serde_json::from_value(raw.clone())
        .map_err(|_| ContractError("unknown, missing or invalid fields"))?;
    set.validate(&raw)?;
    Ok(set)
}

/// A signed core set: the release key's `mica/update-envelope/v1` around its payload.
pub fn authenticate(bytes: &[u8], public_keys: &[[u8; 32]]) -> Result<CoreSet> {
    parse(&authenticate_payload(
        bytes,
        public_keys,
        MAX_CORE_SET_BYTES,
    )?)
}

/// The components a device composes from `set`: those a feature of its product (`features`, from
/// its product file) selects. Each runs on its root's interface `level`, and every need of a
/// selected component is itself selected, at a version in range. The rule mica-build applies before
/// it publishes a set, so a set no product of the channel can boot is never offered.
pub fn select<'a>(
    set: &'a CoreSet,
    features: &[String],
    level: u64,
) -> Result<Vec<&'a CoreComponent>> {
    let chosen: Vec<&CoreComponent> = set
        .components
        .iter()
        .filter(|core| core.features.iter().any(|f| features.contains(f)))
        .collect();
    for core in &chosen {
        require(
            core.runs_on(level),
            "a selected core component does not run on this root's interface level",
        )?;
        needs_met(
            core,
            chosen.iter().copied(),
            "a selected core component's need is not selected",
        )?;
    }
    Ok(chosen)
}

/// The features of `/usr/lib/mica/product.conf`: its one `FEATURES="<f> ..."` line.
pub fn device_features(text: &str) -> Result<Vec<String>> {
    let mut found = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("FEATURES=") {
            require(
                found.is_none(),
                "product file names its features more than once",
            )?;
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .ok_or(ContractError("invalid features in the product file"))?;
            let features: Vec<String> = value.split_ascii_whitespace().map(str::to_owned).collect();
            for feature in &features {
                name(feature).map_err(|_| ContractError("invalid features in the product file"))?;
            }
            found = Some(features);
        }
    }
    found.ok_or(ContractError("product file names no features"))
}

//! Redaction: what a snapshot may carry, and scrubbing what it may not.

use crate::redact;
use Rule::Scalar as S;
use serde_json::{Map, Value};

use super::*;

/// The longest dynamic key (a unit name, a health component) kept.
pub(super) const MAX_KEY_LEN: usize = 128;

/// Field names whose values never ship, whatever the allowlist says of
/// them. `crate::redact`'s five plus the names a credential travels under.
pub(super) const SNAPSHOT_SECRET_FIELDS: [&str; 14] = [
    "token",
    "tokens",
    "secret",
    "secrets",
    "password",
    "passwd",
    "passphrase",
    "credential",
    "credentials",
    "authorization",
    "cookie",
    "apiTokens",
    "key",
    "keys",
];

/// Substrings (matched case-insensitively) that mark a string as carrying
/// a secret; the whole string is replaced.
pub(super) const SECRET_MARKERS: [&str; 15] = [
    "password",
    "passwd",
    "passphrase",
    "psk",
    "secret",
    "token",
    "private key",
    "privatekey",
    "private_key",
    "authorization:",
    "bearer ",
    "-----begin",
    "api key",
    "apikey",
    "api_key",
];

// ---- redaction ----------------------------------------------------------

/// One node of the allowlist schema.
#[derive(Debug, Clone)]
pub(super) enum Rule {
    /// A scalar: null, bool, number, or a string (scrubbed). An object or
    /// array here is unclassified and dropped.
    Scalar,
    /// Kept as the sentinel: the field's presence is evidence, its value is
    /// identifying.
    Redact,
    /// An object with these named members. `available` (bool) and `detail`
    /// (string) are allowed in every object, because every absent member
    /// carries them.
    Object(Vec<(&'static str, Rule)>),
    /// An object with dynamic keys, each value by the rule.
    Map(Box<Rule>),
    /// An array of values by the rule.
    Array(Box<Rule>),
}

pub(super) fn obj(fields: Vec<(&'static str, Rule)>) -> Rule {
    Rule::Object(fields)
}

pub(super) fn map(rule: Rule) -> Rule {
    Rule::Map(Box::new(rule))
}

pub(super) fn arr(rule: Rule) -> Rule {
    Rule::Array(Box::new(rule))
}

/// What one redaction pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RedactionStats {
    /// Fields the schema did not name, and values of the wrong shape: gone.
    pub dropped_fields: usize,
    /// Fields replaced by a sentinel, strings replaced whole, and tokens
    /// replaced inside strings.
    pub redacted_fields: usize,
}

/// Whether `token` is a hardware address: six pairs of hex separated by
/// colons, and nothing else.
pub(super) fn is_mac_token(token: &str) -> bool {
    let bytes = token.as_bytes();
    bytes.len() == 17
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index % 3 == 2 {
                *byte == b':'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

/// Scrub one string: a secret marker anywhere replaces it whole, a
/// hardware-address-shaped token is replaced in place, and the result is
/// capped at [`MAX_TEXT_BYTES`]. Returns the text and how many
/// replacements were made.
#[must_use]
pub fn scrub(text: &str) -> (String, usize) {
    let lowered = text.to_ascii_lowercase();
    if SECRET_MARKERS.iter().any(|marker| lowered.contains(marker)) {
        return (REDACTED_LINE.to_string(), 1);
    }
    let mut replaced = 0;
    let mut out = String::with_capacity(text.len());
    for (index, token) in text.split(' ').enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let trimmed = token.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        if is_mac_token(trimmed) {
            out.push_str(&token.replace(trimmed, REDACTED_MAC));
            replaced += 1;
        } else {
            out.push_str(token);
        }
    }
    if out.len() > MAX_TEXT_BYTES {
        let mut cut = MAX_TEXT_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("…[cut]");
    }
    (out, replaced)
}

pub(super) fn is_snapshot_secret(name: &str) -> bool {
    SNAPSHOT_SECRET_FIELDS.contains(&name)
}

/// Whether a dynamic key is one the store will carry: printable, bounded,
/// and not a secret field name.
pub(super) fn key_allowed(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= MAX_KEY_LEN
        && !is_snapshot_secret(key)
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '@' | '/'))
}

pub(super) fn apply(rule: &Rule, value: Value, stats: &mut RedactionStats) -> Option<Value> {
    if value.is_null() {
        return Some(Value::Null);
    }
    match rule {
        Rule::Scalar => match value {
            Value::String(text) => {
                let (text, replaced) = scrub(&text);
                stats.redacted_fields += replaced;
                Some(Value::String(text))
            }
            Value::Bool(_) | Value::Number(_) => Some(value),
            Value::Object(_) | Value::Array(_) | Value::Null => {
                stats.dropped_fields += 1;
                None
            }
        },
        Rule::Redact => {
            stats.redacted_fields += 1;
            Some(Value::String(redact::REDACTED.to_string()))
        }
        Rule::Object(fields) => {
            let Value::Object(members) = value else {
                stats.dropped_fields += 1;
                return None;
            };
            let mut out = Map::new();
            for (key, member) in members {
                if is_snapshot_secret(&key) {
                    stats.redacted_fields += 1;
                    out.insert(key, Value::String(redact::REDACTED.to_string()));
                    continue;
                }
                let rule = match key.as_str() {
                    "available" => Some(&S),
                    "detail" => Some(&S),
                    other => fields
                        .iter()
                        .find(|(name, _)| *name == other)
                        .map(|(_, rule)| rule),
                };
                match rule {
                    Some(rule) => {
                        if let Some(kept) = apply(rule, member, stats) {
                            out.insert(key, kept);
                        }
                    }
                    None => stats.dropped_fields += 1,
                }
            }
            Some(Value::Object(out))
        }
        Rule::Map(rule) => {
            let Value::Object(members) = value else {
                stats.dropped_fields += 1;
                return None;
            };
            let mut out = Map::new();
            for (key, member) in members {
                if !key_allowed(&key) {
                    stats.dropped_fields += 1;
                    continue;
                }
                if let Some(kept) = apply(rule, member, stats) {
                    out.insert(key, kept);
                }
            }
            Some(Value::Object(out))
        }
        Rule::Array(rule) => {
            let Value::Array(items) = value else {
                stats.dropped_fields += 1;
                return None;
            };
            Some(Value::Array(
                items
                    .into_iter()
                    .filter_map(|item| apply(rule, item, stats))
                    .collect(),
            ))
        }
    }
}

/// `snapshot` with the denylist applied, then the allowlist, then
/// [`scrub`] on every string. Fails closed: what the schema does not name
/// does not come back.
#[must_use]
pub fn redact_snapshot(snapshot: Value) -> (Value, RedactionStats) {
    let mut stats = RedactionStats::default();
    let denied = redact::redact(snapshot, "");
    let kept = apply(&schema(), denied, &mut stats).unwrap_or(Value::Object(Map::new()));
    (kept, stats)
}

// ---- collection ---------------------------------------------------------

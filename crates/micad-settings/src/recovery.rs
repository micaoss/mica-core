//! Board-declared physical recovery actions: the ONE interface the system
//! layer reserves for them.
//!
//! **The physical action is a BOARD fact and this module knows nothing about
//! it.** A GRUB menu entry, a U-Boot menu selection, a button pattern held
//! across a power cycle, a USB event — whichever a board has, its BSP
//! implements it and the board DECLARES it. What crosses into the system layer
//! is one string, the *recovery intent*, placed on the kernel command line by
//! whatever ran before Linux. This module reads that intent, maps it through
//! the board's declaration, and hands back the action the board named. It
//! never learns which mechanism produced the intent, and nothing here is
//! per-board.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::model::ResetTier;

/// Where a board's declaration is read from on the device.
pub const DEFAULT_DECLARATION_PATH: &str = "/usr/lib/mica/recovery-actions.conf";

/// Test hook relocating [`DEFAULT_DECLARATION_PATH`].
pub const DECLARATION_PATH_ENV: &str = "MICA_RECOVERY_DECLARATION_PATH";

/// Where the recovery intent is read from.
pub const DEFAULT_CMDLINE_PATH: &str = "/proc/cmdline";

/// Test hook relocating [`DEFAULT_CMDLINE_PATH`].
pub const CMDLINE_PATH_ENV: &str = "MICA_RECOVERY_CMDLINE_PATH";

/// The kernel command-line parameter a board's mechanism sets.
///
/// `mica.recovery=<intent>`. A bootloader menu entry that appends it is the
/// industry-standard shape this interface expects; nothing bespoke is invented
/// at the system layer, and a board free to choose its own parameter would
/// make the system layer per-board.
pub const INTENT_PARAMETER: &str = "mica.recovery";

/// The board key listing the actions a board implements.
pub const ACTIONS_KEY: &str = "BOARD_RECOVERY_ACTIONS";

/// How long a presence assertion produced from a boot-time action stands.
pub const PRESENCE_WINDOW_SECS: u64 = 15 * 60;

/// The tier value a declared action uses when it authorizes presence and
/// stages no reset.
pub const TIER_NONE: &str = "none";

/// Where the presence assertion this interface produces is left.
///
/// On tmpfs and owned by root, so the assertion does not survive the boot the
/// operator made it on.
pub const DEFAULT_PRESENCE_MARKER_PATH: &str = "/run/mica/presence";

/// Test hook relocating [`DEFAULT_PRESENCE_MARKER_PATH`].
pub const PRESENCE_MARKER_PATH_ENV: &str = "MICA_PRESENCE_MARKER_PATH";

/// The audit event a mapped recovery action is recorded under, before the
/// mechanism is appended: see [`recovery_action_event`].
///
/// Used bare for a REFUSAL, where there is no mechanism to name because
/// nothing mapped.
pub const RECOVERY_ACTION_EVENT: &str = "recovery-action";

/// The outcome recorded when a recovery intent mapped to nothing.
#[must_use]
pub fn refusal_outcome(reason: &NoAction) -> &'static str {
    match reason {
        NoAction::BoardDeclaresNone => "refused-board-declares-none",
        NoAction::Unreadable(_) => "refused-declaration-unreadable",
        NoAction::UnknownIntent => "refused-unknown-intent",
    }
}

/// The outcome recorded when the command line itself was malformed, so no
/// intent was read at all.
pub const REFUSED_MALFORMED_INTENT: &str = "refused-malformed-intent";

/// The source recorded for a refused intent: the local surface it arrived on,
/// never the intent's own text, which is not this trail's to carry.
pub const INTENT_SOURCE: &str = "cmdline";

/// The audit event naming a mechanism that asserted presence at boot.
#[must_use]
pub fn recovery_action_event(mechanism: &str) -> String {
    format!("{RECOVERY_ACTION_EVENT}-{mechanism}")
}

/// The audit event a credential recovery is recorded under:
/// the flow and the presence mechanism, so
/// "which door was used" is greppable.
#[must_use]
pub fn credential_recovery_event(mechanism: &str) -> String {
    format!("credential-recovery-{mechanism}")
}

/// The audit event a staged reset tier is recorded under, whichever half of
/// the system staged it.
///
/// One name per tier, shared by the API route that stages one and by the
/// boot-time action that stages one, so an operator greps a device's history
/// for `reset-full-factory` and finds both.
#[must_use]
pub fn reset_event(tier: ResetTier) -> &'static str {
    match tier {
        ResetTier::Configuration => "reset-configuration",
        ResetTier::ApplicationData => "reset-application-data",
        ResetTier::FullFactory => "reset-full-factory",
    }
}

/// What [`DEFAULT_PRESENCE_MARKER_PATH`] holds.
///
/// Three members and no room for a fourth: an assertion is a mechanism, a
/// channel and a deadline. Declared once and serialized by micad, deserialized
/// by apid, so the writer and the reader cannot come to disagree about the
/// shape.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PresenceMarker {
    /// The mechanism asserted, which must be one the board declares.
    pub mechanism: String,
    /// The device the operator is attached to, and therefore the ONE channel a
    /// minted credential may be published on.
    pub channel: PathBuf,
    /// UNIX seconds at which the assertion stops standing.
    pub expires: u64,
}

/// The presence marker path this device uses, honouring the test hook.
#[must_use]
pub fn presence_marker_path() -> PathBuf {
    std::env::var_os(PRESENCE_MARKER_PATH_ENV).map_or_else(
        || PathBuf::from(DEFAULT_PRESENCE_MARKER_PATH),
        PathBuf::from,
    )
}

/// One physical recovery action a board declares.
///
/// Four facts and no fifth: what the mechanism puts on the command line, what
/// the assertion is called, where a minted credential may be published, and
/// which reset tier the action stages. Everything the system does with an
/// action is one of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryAction {
    /// The board's own name for the action, as it appears in
    /// [`ACTIONS_KEY`] — `GRUB_RECOVERY_ENTRY`, `UBOOT_MENU`. This is what the
    /// audit trail records, because "which declared action produced it" is the
    /// question an audit of a reset has to answer.
    pub name: String,
    /// The `mica.recovery=` value the board's mechanism sets.
    pub intent: String,
    /// What the presence assertion is called: the mechanism the reader
    /// validates and the suffix the audit event carries.
    pub mechanism: String,
    /// The device the operator is attached to, and therefore the ONE channel a
    /// minted credential may be published on.
    pub channel: PathBuf,
    /// The tier this action stages, or `None` when it only asserts presence.
    ///
    /// `None` is the credential-recovery shape: the operator proves presence
    /// at boot, rotates the management credential, and decides afterwards
    /// whether anything destructive is wanted.
    pub tier: Option<ResetTier>,
}

/// What a board declares, read as a total function of the file.
///
/// Three states and not two: a board that declares nothing and a declaration
/// this build cannot read both map no intent, and telling them apart is the
/// difference between "this board has no recovery action" and "this image is
/// broken".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Declaration {
    /// The board declares no physical recovery action. Both mica boards.
    None,
    /// The board declares these, in file order.
    Actions(Vec<RecoveryAction>),
    /// A declaration is present and is not one this build can read. Fails
    /// closed: no intent maps through it, and the reason is carried so the
    /// refusal can name it.
    Unreadable(String),
}

/// Why no action was produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoAction {
    /// The board declares no physical recovery action at all.
    BoardDeclaresNone,
    /// The board's declaration could not be read.
    Unreadable(String),
    /// The intent names no action this board declares.
    UnknownIntent,
}

impl Declaration {
    /// Read the declaration at `path`. A file that is not there is
    /// [`Declaration::None`] — see [`DEFAULT_DECLARATION_PATH`].
    #[must_use]
    pub fn read(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::None,
            Err(err) => Self::Unreadable(format!("{} could not be read: {err}", path.display())),
        }
    }

    /// Read the declaration this device ships, honouring the test hook.
    #[must_use]
    pub fn from_env() -> Self {
        Self::read(&declaration_path())
    }

    /// Parse a declaration's text.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        let values = match parse_assignments(text) {
            Ok(values) => values,
            Err(reason) => return Self::Unreadable(reason),
        };
        let Some(list) = values.iter().find(|(key, _)| key == ACTIONS_KEY) else {
            return Self::Unreadable(format!(
                "the declaration does not declare {ACTIONS_KEY}, so it says nothing about which \
                 physical actions this board has; a board with none declares it empty"
            ));
        };
        let names: Vec<&str> = list.1.split_whitespace().collect();
        if names.is_empty() {
            return Self::None;
        }

        let mut actions = Vec::with_capacity(names.len());
        for name in names {
            match action_from(&values, name) {
                Ok(action) => actions.push(action),
                Err(reason) => return Self::Unreadable(reason),
            }
        }
        if let Err(reason) = refuse_collisions(&actions) {
            return Self::Unreadable(reason);
        }
        Self::Actions(actions)
    }

    /// The declared actions; empty for both of the other states.
    #[must_use]
    pub fn actions(&self) -> &[RecoveryAction] {
        match self {
            Self::Actions(actions) => actions,
            Self::None | Self::Unreadable(_) => &[],
        }
    }

    /// Whether some declared action asserts presence under `mechanism`.
    #[must_use]
    pub fn declares_mechanism(&self, mechanism: &str) -> bool {
        self.actions().iter().any(|a| a.mechanism == mechanism)
    }

    /// The mechanisms this board answers `recovery.presence` with, in
    /// declaration order, for a refusal that has to say what the board offers.
    #[must_use]
    pub fn mechanisms(&self) -> Vec<&str> {
        self.actions()
            .iter()
            .map(|a| a.mechanism.as_str())
            .collect()
    }

    /// The action a recovery intent names.
    pub fn map_intent(&self, intent: &str) -> Result<&RecoveryAction, NoAction> {
        match self {
            Self::None => Err(NoAction::BoardDeclaresNone),
            Self::Unreadable(reason) => Err(NoAction::Unreadable(reason.clone())),
            Self::Actions(actions) => actions
                .iter()
                .find(|a| a.intent == intent)
                .ok_or(NoAction::UnknownIntent),
        }
    }
}

/// The declaration path this device uses, honouring the test hook.
#[must_use]
pub fn declaration_path() -> PathBuf {
    std::env::var_os(DECLARATION_PATH_ENV)
        .map_or_else(|| PathBuf::from(DEFAULT_DECLARATION_PATH), PathBuf::from)
}

/// The kernel command-line path this device uses, honouring the test hook.
#[must_use]
pub fn cmdline_path() -> PathBuf {
    std::env::var_os(CMDLINE_PATH_ENV)
        .map_or_else(|| PathBuf::from(DEFAULT_CMDLINE_PATH), PathBuf::from)
}

/// The recovery intent a kernel command line carries, if any.
///
/// Fails closed on a command line carrying [`INTENT_PARAMETER`] more than once
/// or carrying it with an empty value: both are a mechanism that did not do
/// what it meant to, and guessing which occurrence was meant is how a
/// mechanism ends up selecting a tier nobody asked for.
pub fn intent_from_cmdline(cmdline: &str) -> Result<Option<&str>, String> {
    let prefix = format!("{INTENT_PARAMETER}=");
    let mut found: Option<&str> = None;
    for token in cmdline.split_whitespace() {
        if token == INTENT_PARAMETER {
            return Err(format!(
                "the kernel command line carries `{INTENT_PARAMETER}` with no value; a recovery \
                 intent names the action that produced it"
            ));
        }
        let Some(value) = token.strip_prefix(&prefix) else {
            continue;
        };
        if value.is_empty() {
            return Err(format!(
                "the kernel command line carries `{INTENT_PARAMETER}=` with an empty value"
            ));
        }
        if found.is_some() {
            return Err(format!(
                "the kernel command line carries `{INTENT_PARAMETER}` more than once; which \
                 action was meant cannot be decided here"
            ));
        }
        found = Some(value);
    }
    Ok(found)
}

/// The tier a declaration's `_TIER` value names.
fn tier_from(value: &str) -> Option<Option<ResetTier>> {
    match value {
        TIER_NONE => Some(None),
        "configuration" => Some(Some(ResetTier::Configuration)),
        "application-data" => Some(Some(ResetTier::ApplicationData)),
        "full-factory" => Some(Some(ResetTier::FullFactory)),
        _ => None,
    }
}

/// Build one action out of the parsed assignments, or say why it cannot be.
fn action_from(values: &[(String, String)], name: &str) -> Result<RecoveryAction, String> {
    if !is_action_name(name) {
        return Err(format!(
            "{ACTIONS_KEY} names `{name}`, which is not an action name: a name is uppercase \
             letters, digits and underscores, starting with a letter, because it is the middle of \
             the keys that describe it"
        ));
    }
    let get = |suffix: &str| -> Result<String, String> {
        let key = format!("RECOVERY_{name}_{suffix}");
        match values.iter().find(|(k, _)| *k == key) {
            None => Err(format!(
                "{ACTIONS_KEY} names `{name}` and the declaration has no {key}"
            )),
            Some((_, value)) if value.is_empty() => Err(format!(
                "{key} is declared empty; an empty declaration is not a value"
            )),
            Some((_, value)) => Ok(value.clone()),
        }
    };

    let intent = get("INTENT")?;
    if !is_intent_token(&intent) {
        return Err(format!(
            "RECOVERY_{name}_INTENT is `{intent}`, which is not a kernel command-line token: \
             lowercase letters, digits and dashes"
        ));
    }
    let mechanism = get("MECHANISM")?;
    if !is_mechanism_token(&mechanism) {
        return Err(format!(
            "RECOVERY_{name}_MECHANISM is `{mechanism}`, which is not a mechanism name: \
             lowercase letters, digits and dashes, starting with a letter"
        ));
    }
    let channel = get("CHANNEL")?;
    if !is_channel_path(&channel) {
        return Err(format!(
            "RECOVERY_{name}_CHANNEL is `{channel}`; a channel is a device under /dev, because a \
             minted credential is written to it and never to a file"
        ));
    }
    let tier_value = get("TIER")?;
    let Some(tier) = tier_from(&tier_value) else {
        return Err(format!(
            "RECOVERY_{name}_TIER is `{tier_value}`; it is one of {TIER_NONE}, configuration, \
             application-data, full-factory"
        ));
    };

    Ok(RecoveryAction {
        name: name.to_string(),
        intent,
        mechanism,
        channel: PathBuf::from(channel),
        tier,
    })
}

/// Two actions must not share an intent or a mechanism.
///
/// A shared intent is a mapping with two answers; a shared mechanism is an
/// audit trail that cannot say which door was used, which is the whole reason
/// the mechanism rides in the event name.
fn refuse_collisions(actions: &[RecoveryAction]) -> Result<(), String> {
    let mut intents = BTreeSet::new();
    let mut mechanisms = BTreeSet::new();
    for action in actions {
        if !intents.insert(action.intent.as_str()) {
            return Err(format!(
                "two declared actions share the intent `{}`; one command line would name both",
                action.intent
            ));
        }
        if !mechanisms.insert(action.mechanism.as_str()) {
            return Err(format!(
                "two declared actions share the mechanism `{}`; the audit trail could not say \
                 which one was used",
                action.mechanism
            ));
        }
    }
    Ok(())
}

fn is_action_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn is_intent_token(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_mechanism_token(value: &str) -> bool {
    let mut chars = value.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_channel_path(value: &str) -> bool {
    value.starts_with("/dev/") && value.len() > "/dev/".len() && !value.contains("..")
}

/// `KEY=value` lines, in file order, or the sentence refusing the file.
fn parse_assignments(text: &str) -> Result<Vec<(String, String)>, String> {
    let mut values = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let number = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "line {number} is not a `KEY=value` assignment: {line}"
            ));
        };
        if key.is_empty() || !is_key(key) {
            return Err(format!(
                "line {number} assigns to `{key}`, which is not a declaration key"
            ));
        }
        let value = unquote(value)
            .ok_or_else(|| format!("line {number} is not a value this reader accepts: {value}"))?;
        values.push((key.to_string(), value));
    }
    Ok(values)
}

fn is_key(key: &str) -> bool {
    let mut chars = key.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A value, with one layer of matching quotes removed.
///
/// A quoted value may hold anything but its own quote; an unquoted one may
/// hold no whitespace and none of the shell metacharacters that would mean
/// something to the `source` this reader deliberately never performs.
fn unquote(value: &str) -> Option<String> {
    let value = value.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = value
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return (!inner.contains(quote)).then(|| inner.to_string());
        }
    }
    if value.contains(['"', '\'', '$', '`', '\\', ' ', '\t']) {
        return None;
    }
    Some(value.to_string())
}

#[cfg(test)]
mod tests;

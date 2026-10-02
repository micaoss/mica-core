//! The effective update policy: the baked manifest under the operator's document.

use super::*;

/// What this device follows and where it looks: the three keys layer 1 bakes
/// defaults for, after layer 2 has had its say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The effective source URL. `None` is *no online source configured
    /// anywhere*, not *fall back to the baked one*.
    pub url: Option<String>,
    pub mode: UpdateMode,
    pub check_interval_minutes: u64,
}

/// The workspace values layer 2 owns outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    pub max_bytes: u64,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            max_bytes: default_max_bytes(),
        }
    }
}

/// The policy after the precedence: what every caller reads.
#[derive(Debug, Clone, Default)]
pub struct EffectivePolicy {
    /// `None` when the operator document exists and did not load.
    ///
    /// Not the baked selection, and not the code default either: a device
    /// whose configuration is unreadable does not know which source it
    /// follows, and saying so is the whole point. Every action that turns on the
    /// answer is refused while this is `None`.
    pub selection: Option<Selection>,
    pub workspace: Workspace,
    pub network: NetworkPolicy,
    pub maintenance: MaintenancePolicy,
    pub reboot_gate: RebootGatePolicy,
    pub reboot_policy: RebootPolicy,
    /// `HH:MM` UTC the automatic check is anchored to, when the operator
    /// named one. Layer 2 owns it outright: layer 1 bakes no anchor, so it is
    /// not part of [`Selection`].
    pub check_at: Option<String>,
}

impl EffectivePolicy {
    /// The policy of a device whose operator document did not load.
    pub fn unknown_selection() -> Self {
        Self::default()
    }

    /// Why the automatic path may not install right now, or `None`.
    pub fn auto_window_refusal(&self) -> Option<&'static str> {
        let selection = self.selection.as_ref()?;
        (selection.mode == UpdateMode::Auto && self.maintenance.windows.is_empty())
            .then_some(AUTO_NEEDS_A_WINDOW)
    }
}

/// Resolve layer 2 over layer 1, per key. The one implementation of
/// the precedence; every caller goes through it.
pub fn resolve(baked: &BakedUpdate, document: UpdatesDocument) -> EffectivePolicy {
    EffectivePolicy {
        selection: Some(Selection {
            // `.flatten` is where absent and explicit-`null` become the same
            // answer: both mean "take the baked default". They
            // stay distinguishable in `provisioning_status`, which reports the
            // document rather than the resolution.
            url: document
                .source
                .url
                .flatten()
                .or_else(|| baked.source.clone()),
            mode: document.policy.flatten().unwrap_or(baked.policy),
            check_interval_minutes: document
                .check_interval_minutes
                .flatten()
                .unwrap_or(baked.check_interval_minutes),
        }),
        workspace: Workspace {
            max_bytes: document.source.max_bytes,
        },
        network: document.network,
        maintenance: document.maintenance,
        reboot_gate: document.reboot_gate,
        reboot_policy: document.reboot_policy,
        check_at: document.check_at.flatten(),
    }
}

// ---------------------------------------------------------------------------
// Layer 2: /mica/config/fleet.json
// ---------------------------------------------------------------------------

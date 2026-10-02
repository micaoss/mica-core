//! Storage pressure: the thresholds' hysteresis over time.

use std::collections::BTreeMap;

use super::*;

/// One tier's low-space classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pressure {
    /// Below every threshold, or back below a clear threshold.
    #[default]
    Normal,
    /// At or above [`WARNING_ENTER_PERCENT`].
    Warning,
    /// At or above [`CRITICAL_ENTER_PERCENT`].
    Critical,
}

impl Pressure {
    /// The wire spelling the API and UI consume.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// Apply the hysteresis band to one reading.
#[must_use]
pub fn next_pressure(previous: Pressure, used_percent: u8) -> Pressure {
    if used_percent >= CRITICAL_ENTER_PERCENT {
        return Pressure::Critical;
    }
    if used_percent >= WARNING_ENTER_PERCENT {
        // Falling out of critical needs the clear threshold, not merely
        // dropping below the enter threshold.
        if previous == Pressure::Critical && used_percent >= CRITICAL_CLEAR_PERCENT {
            return Pressure::Critical;
        }
        return Pressure::Warning;
    }
    if used_percent >= WARNING_CLEAR_PERCENT {
        // Between the two warning thresholds: hold whatever was already
        // reported, dropping only from critical to warning.
        return match previous {
            Pressure::Normal => Pressure::Normal,
            _ => Pressure::Warning,
        };
    }
    Pressure::Normal
}

/// Remembers each watched tier's last reported [`Pressure`] so
/// [`next_pressure`]'s hysteresis has a previous state to work from.
///
/// The state is per-daemon and in RAM on purpose: it exists to damp flapping
/// between polls, not to be a history, and a fresh daemon classifying from
/// `Normal` reaches the same steady state within one poll.
#[derive(Debug, Default)]
pub struct PressureTracker {
    pub(super) states: std::sync::Mutex<BTreeMap<String, Pressure>>,
}

impl PressureTracker {
    /// The last classification recorded for `tier`, without taking a new
    /// reading. `Normal` when the tier has never been observed.
    pub fn current(&self, tier: &str) -> Pressure {
        self.states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(tier)
            .copied()
            .unwrap_or_default()
    }

    /// Record `used_percent` for `tier` and return the classification.
    pub fn observe(&self, tier: &str, used_percent: u8) -> Pressure {
        let mut states = self
            .states
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = states.get(tier).copied().unwrap_or_default();
        let next = next_pressure(previous, used_percent);
        states.insert(tier.to_string(), next);
        next
    }
}

//! When the device advertises: the provisioning windows and how they merge
//! (`docs/ble-provisioning.md`, Windows).
//!
//! Times are seconds since boot, so a window measured from boot needs no
//! wall clock and a clock change moves nothing.

/// A boot window's length, from boot.
pub const BOOT_SECONDS: u64 = 10 * 60;
/// How long the default route must be gone before `offline` opens.
pub const OFFLINE_DELAY: u64 = 120;
/// The lengths a `manual` window may have.
pub const MANUAL_SECONDS: [u64; 3] = [5 * 60, 10 * 60, 30 * 60];

/// What opened a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Trigger {
    Unclaimed,
    Boot,
    Offline,
    Manual,
}

/// How a phone authenticates in a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Auth {
    None,
    Password,
    TempCode,
}

impl Auth {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Password => "password",
            Self::TempCode => "temp-code",
        }
    }
}

/// The window open now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Open {
    pub trigger: Trigger,
    pub auth: Auth,
    /// Seconds since boot it closes at; none while it waits for the default
    /// route to return.
    pub closes_at: Option<u64>,
}

/// The windows of one boot.
#[derive(Debug, Clone)]
pub struct Windows {
    /// Whether the device was claimed when it booted: which boot window this
    /// boot has.
    claimed_at_boot: bool,
    /// `finish` closed the boot window.
    boot_closed: bool,
    had_route: bool,
    /// Since when the default route has been gone, once it existed.
    lost_since: Option<u64>,
    /// When the `manual` window closes.
    manual_until: Option<u64>,
}

impl Windows {
    #[must_use]
    pub fn new(claimed_at_boot: bool) -> Self {
        Self {
            claimed_at_boot,
            boot_closed: false,
            had_route: false,
            lost_since: None,
            manual_until: None,
        }
    }

    /// Whether a default route exists at `now`.
    pub fn route(&mut self, now: u64, present: bool) {
        if present {
            self.had_route = true;
            self.lost_since = None;
        } else if self.had_route && self.lost_since.is_none() {
            self.lost_since = Some(now);
        }
    }

    /// Open the `manual` window for `seconds` from `now`.
    pub fn open_manual(&mut self, now: u64, seconds: u64) {
        self.manual_until = Some(now + seconds);
    }

    /// Close the `manual` window: the console, or a lockout.
    pub fn close_manual(&mut self) {
        self.manual_until = None;
    }

    /// `finish` at `now` closes every window; `offline` opens again
    /// [`OFFLINE_DELAY`] later if the route is still gone.
    pub fn finish(&mut self, now: u64) {
        self.boot_closed = true;
        self.manual_until = None;
        if self.lost_since.is_some() {
            self.lost_since = Some(now);
        }
    }

    /// The window open at `now`, if any.
    #[must_use]
    pub fn current(&self, now: u64, claimed: bool, auto_when_offline: bool) -> Option<Open> {
        let boot = !self.boot_closed && now < BOOT_SECONDS;
        if !claimed {
            return (boot && !self.claimed_at_boot).then_some(Open {
                trigger: Trigger::Unclaimed,
                auth: Auth::None,
                closes_at: Some(BOOT_SECONDS),
            });
        }
        if let Some(until) = self.manual_until.filter(|until| now < *until) {
            return Some(Open {
                trigger: Trigger::Manual,
                auth: Auth::TempCode,
                closes_at: Some(until),
            });
        }
        let boot = boot && self.claimed_at_boot;
        let offline = auto_when_offline
            && self
                .lost_since
                .is_some_and(|since| now >= since + OFFLINE_DELAY);
        match (boot, offline) {
            (true, _) => Some(Open {
                trigger: Trigger::Boot,
                auth: Auth::Password,
                closes_at: (!offline).then_some(BOOT_SECONDS),
            }),
            (false, true) => Some(Open {
                trigger: Trigger::Offline,
                auth: Auth::Password,
                closes_at: None,
            }),
            (false, false) => None,
        }
    }
}

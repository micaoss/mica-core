//! Maintenance windows: when an install may start.

use chrono::{DateTime, Datelike, Timelike, Utc};

use super::*;

impl MaintenanceWindow {
    /// Whether `now` (UTC) falls inside this window.
    ///
    /// A window whose end is at or before its start wraps past midnight; a
    /// window listing no days opens every day.
    pub fn contains(&self, now: DateTime<Utc>) -> bool {
        // Validation ran at load; an unparseable window here (impossible via
        // `load_updates`) simply never matches, which is the closed side.
        let Some(start) = minutes_of_day(&self.start) else {
            return false;
        };
        let Some(end) = minutes_of_day(&self.end) else {
            return false;
        };
        let now_week = minutes_of_week(now);
        let days: Vec<u32> = if self.days.is_empty() {
            (0..7).collect()
        } else {
            self.days.iter().filter_map(|day| day_index(day)).collect()
        };
        for day in days {
            let open = day * 1440 + start;
            let span = if end > start {
                end - start
            } else {
                // Wrapping: 23:00–01:00 is two hours into the next day. Equal
                // start and end reads as a full 24 hours.
                1440 - start + end
            };
            let offset = (now_week + WEEK_MINUTES - open) % WEEK_MINUTES;
            if offset < span {
                return true;
            }
        }
        false
    }
}

/// `HH:MM` → minutes since midnight, or `None` when it is not that.
pub(super) fn minutes_of_day(clock: &str) -> Option<u32> {
    let (hours, minutes) = clock.split_once(':')?;
    if hours.len() != 2 || minutes.len() != 2 {
        return None;
    }
    let hours: u32 = hours.parse().ok()?;
    let minutes: u32 = minutes.parse().ok()?;
    (hours < 24 && minutes < 60).then_some(hours * 60 + minutes)
}

/// The most recent occurrence of the clock face `clock` at or before `now`,
/// or `None` when the string is not `HH:MM`.
///
/// The one implementation of "when did today's check time last come round":
/// the driver anchors on it, and a caller that computed it itself would
/// eventually disagree with the document's own validation.
#[must_use]
pub fn last_crossing(clock: &str, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let minutes = minutes_of_day(clock)?;
    let face = now
        .date_naive()
        .and_hms_opt(minutes / 60, minutes % 60, 0)?
        .and_utc();
    Some(if face <= now {
        face
    } else {
        face - chrono::Duration::days(1)
    })
}

/// `mon`..`sun` → 0..6, Monday first (chrono's `num_days_from_monday`).
pub(super) fn day_index(day: &str) -> Option<u32> {
    Some(match day {
        "mon" => 0,
        "tue" => 1,
        "wed" => 2,
        "thu" => 3,
        "fri" => 4,
        "sat" => 5,
        "sun" => 6,
        _ => return None,
    })
}

/// Minutes into the UTC week (Monday 00:00 = 0) for `now`.
pub(super) fn minutes_of_week(now: DateTime<Utc>) -> u32 {
    now.weekday().num_days_from_monday() * 1440 + now.hour() * 60 + now.minute()
}

pub(super) const WEEK_MINUTES: u32 = 7 * 1440;

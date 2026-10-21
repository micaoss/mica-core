//! The windows and their merge rules.

use super::super::window::*;

const MIN: u64 = 60;

#[test]
fn an_unclaimed_boot_opens_ten_minutes_without_a_password() {
    let mut windows = Windows::new(false);
    let open = windows.current(5, false, true).unwrap();
    assert_eq!(open.trigger, Trigger::Unclaimed);
    assert_eq!(open.auth, Auth::None);
    assert_eq!(open.closes_at, Some(10 * MIN));
    assert_eq!(windows.current(10 * MIN, false, true), None);
    // Manual and offline windows are only for claimed devices.
    windows.open_manual(5, 10 * MIN);
    windows.route(5, true);
    windows.route(6, false);
    assert_eq!(windows.current(11 * MIN, false, true), None);
}

#[test]
fn a_claim_elsewhere_closes_the_unclaimed_window_until_the_next_boot() {
    let windows = Windows::new(false);
    assert_eq!(windows.current(60, true, true), None);
}

#[test]
fn finish_closes_the_unclaimed_window() {
    let mut windows = Windows::new(false);
    windows.finish(60);
    assert_eq!(windows.current(61, false, true), None);
}

#[test]
fn a_claimed_boot_opens_ten_minutes_with_the_password() {
    let windows = Windows::new(true);
    let open = windows.current(5, true, false).unwrap();
    assert_eq!((open.trigger, open.auth), (Trigger::Boot, Auth::Password));
    assert_eq!(open.closes_at, Some(10 * MIN));
    assert_eq!(windows.current(10 * MIN, true, false), None);
}

#[test]
fn offline_opens_120_s_after_a_route_that_existed_is_lost_and_closes_when_it_returns() {
    let mut windows = Windows::new(true);
    windows.finish(1);
    // Never had a route: nothing opens however long it lasts.
    windows.route(20 * MIN, false);
    assert_eq!(windows.current(30 * MIN, true, true), None);

    windows.route(30 * MIN, true);
    windows.route(31 * MIN, false);
    assert_eq!(windows.current(31 * MIN + 119, true, true), None);
    let open = windows.current(31 * MIN + 120, true, true).unwrap();
    assert_eq!(
        (open.trigger, open.auth),
        (Trigger::Offline, Auth::Password)
    );
    assert_eq!(open.closes_at, None);
    // Switched off in the console: no offline window.
    assert_eq!(windows.current(31 * MIN + 120, true, false), None);

    windows.route(40 * MIN, true);
    assert_eq!(windows.current(40 * MIN, true, true), None);
}

#[test]
fn finish_closes_offline_and_it_reopens_120_s_later_without_a_route() {
    let mut windows = Windows::new(true);
    windows.finish(1);
    windows.route(20 * MIN, true);
    windows.route(21 * MIN, false);
    assert!(windows.current(25 * MIN, true, true).is_some());
    windows.finish(25 * MIN);
    assert_eq!(windows.current(25 * MIN + 119, true, true), None);
    assert!(windows.current(25 * MIN + 120, true, true).is_some());
}

#[test]
fn boot_and_offline_merge_until_ten_minutes_and_a_route() {
    let mut windows = Windows::new(true);
    windows.route(10, true);
    windows.route(60, false);
    let open = windows.current(3 * MIN, true, true).unwrap();
    assert_eq!(open.trigger, Trigger::Boot);
    assert_eq!(open.closes_at, None, "it waits for the route too");
    let open = windows.current(15 * MIN, true, true).unwrap();
    assert_eq!(open.trigger, Trigger::Offline);
    windows.route(16 * MIN, true);
    assert_eq!(windows.current(16 * MIN, true, true), None);
}

#[test]
fn a_manual_window_decides_the_mode_and_closes_on_its_time() {
    let mut windows = Windows::new(true);
    windows.open_manual(60, 5 * MIN);
    let open = windows.current(61, true, true).unwrap();
    assert_eq!((open.trigger, open.auth), (Trigger::Manual, Auth::TempCode));
    assert_eq!(open.closes_at, Some(6 * MIN));
    // The boot window is still there behind it.
    let open = windows.current(6 * MIN, true, true).unwrap();
    assert_eq!(open.trigger, Trigger::Boot);
    windows.open_manual(20 * MIN, 5 * MIN);
    windows.close_manual();
    assert_eq!(windows.current(21 * MIN, true, true), None);
}

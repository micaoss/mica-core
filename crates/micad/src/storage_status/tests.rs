use super::*;

mod binds;
mod host;
mod media;
mod status;

fn space(total: u64, used: u64, free: u64) -> FsSpace {
    FsSpace {
        total,
        used,
        free,
        reserved: total.saturating_sub(used).saturating_sub(free),
    }
}

fn mounted(device: &str, mount: &str) -> MountEvidence {
    MountEvidence {
        device: device.to_string(),
        root: BINDS
            .iter()
            .find(|spec| spec.mount == mount)
            .and_then(|spec| spec.source.strip_prefix(DATA_MOUNT))
            .unwrap_or("/")
            .to_string(),
        mount: mount.to_string(),
        fstype: "ext4".to_string(),
        read_only: false,
    }
}

/// The band, driven as a sequence rather than as isolated readings: a
/// hysteresis threshold is only meaningful against the state before it.
/// A reading inside a band must HOLD the previous state, which is the
/// property that stops a tier hovering on a threshold from flapping.
#[test]
fn the_pressure_band_holds_its_state_between_the_thresholds() {
    let tracker = PressureTracker::default();
    assert_eq!(tracker.observe("data", 10), Pressure::Normal);
    // 78% is inside the warning band but has not crossed the enter
    // threshold: still normal.
    assert_eq!(tracker.observe("data", 78), Pressure::Normal);
    assert_eq!(tracker.observe("data", 80), Pressure::Warning);
    // Back to 78: the enter threshold was crossed, so it holds until the
    // clear threshold.
    assert_eq!(tracker.observe("data", 78), Pressure::Warning);
    assert_eq!(tracker.observe("data", 74), Pressure::Normal);

    assert_eq!(tracker.observe("data", 91), Pressure::Critical);
    // 86% is below the critical enter threshold and above its clear
    // threshold: still critical.
    assert_eq!(tracker.observe("data", 86), Pressure::Critical);
    assert_eq!(tracker.observe("data", 84), Pressure::Warning);
    assert_eq!(tracker.observe("data", 60), Pressure::Normal);

    // Each tier carries its own state; one tier's pressure must not
    // decide another's.
    assert_eq!(tracker.observe("state", 86), Pressure::Warning);
    assert_eq!(tracker.observe("data", 86), Pressure::Warning);
}

/// Without hysteresis both readings below would classify identically.
/// This is the mutation control for [`next_pressure`]: collapse the two
/// thresholds into one and this test fails.
#[test]
fn the_same_reading_classifies_differently_by_where_it_came_from() {
    assert_eq!(next_pressure(Pressure::Normal, 77), Pressure::Normal);
    assert_eq!(next_pressure(Pressure::Warning, 77), Pressure::Warning);
    assert_eq!(next_pressure(Pressure::Critical, 87), Pressure::Critical);
    assert_eq!(next_pressure(Pressure::Warning, 87), Pressure::Warning);
}

fn data_tier(device: &str) -> TierEvidence {
    TierEvidence {
        device: Some(device.to_string()),
        mount: Some(mounted(device, DATA_MOUNT)),
        space: Some(space(1000, 100, 900)),
        ..TierEvidence::default()
    }
}

fn bound(device: &str) -> BindEvidence {
    BindEvidence {
        mount: Some(mounted(device, "/mica")),
        source_is_directory: Some(true),
        probe: Some(ProbeOutcome::Passed),
    }
}

use super::*;

#[test]
fn active_states_that_mean_running_or_coming_up() {
    assert!(is_active("active"));
    assert!(is_active("activating"));
    assert!(is_active("reloading"));
    assert!(!is_active("inactive"));
    assert!(!is_active("deactivating"));
    assert!(!is_active("failed"));
}

#[test]
fn enablement_states_that_mean_already_enabled() {
    assert!(is_enabled("enabled"));
    assert!(is_enabled("enabled-runtime"));
    assert!(is_enabled("static"));
    assert!(!is_enabled("disabled"));
    assert!(!is_enabled("masked"));
    assert!(!is_enabled("linked"));
}

#[tokio::test]
async fn mock_records_mutations_and_models_their_transitions() {
    use mock::MockUnitControl;

    let control = MockUnitControl::new("inactive", "disabled");
    control.enable("u.service").await.unwrap();
    control.start("u.service").await.unwrap();

    assert_eq!(control.active_state("u.service").await.unwrap(), "active");
    assert_eq!(
        control.unit_file_state("u.service").await.unwrap(),
        "enabled-runtime"
    );

    control.stop("u.service").await.unwrap();
    control.disable("u.service").await.unwrap();

    assert_eq!(control.active_state("u.service").await.unwrap(), "inactive");
    assert_eq!(
        control.unit_file_state("u.service").await.unwrap(),
        "disabled"
    );
    assert_eq!(
        control.calls(),
        vec![
            "enable u.service".to_string(),
            "start u.service".to_string(),
            "stop u.service".to_string(),
            "disable u.service".to_string(),
        ]
    );
}

#[tokio::test]
async fn mock_reset_failed_clears_a_failed_unit_and_leaves_others_alone() {
    use mock::MockUnitControl;

    let control = MockUnitControl::new("active", "enabled");
    control.set_active_state("broken.service", "failed");

    control.reset_failed("broken.service").await.unwrap();
    control.reset_failed("healthy.service").await.unwrap();

    assert_eq!(
        control.active_state("broken.service").await.unwrap(),
        "inactive",
        "reset-failed clears the failure, which is what unblocks the start after it"
    );
    assert_eq!(
        control.active_state("healthy.service").await.unwrap(),
        "active",
        "reset-failed on a unit that is not failed does nothing to it"
    );
    assert_eq!(
        control.calls(),
        vec![
            "reset-failed broken.service".to_string(),
            "reset-failed healthy.service".to_string(),
        ]
    );
}

#[tokio::test]
async fn mock_can_refuse_one_units_start_without_touching_another() {
    use mock::MockUnitControl;

    let control = MockUnitControl::new("inactive", "enabled");
    control.refuse_start("limited.service");

    let error = control.start("limited.service").await.unwrap_err();
    control.start("other.service").await.unwrap();

    assert!(
        error.to_string().contains("repeated too quickly"),
        "unexpected error: {error}"
    );
    // Refused, so the unit did NOT run: a mock that marked it active here
    // would let a caller's next read claim a broker that never started.
    assert_eq!(
        control.active_state("limited.service").await.unwrap(),
        "inactive"
    );
    assert_eq!(
        control.active_state("other.service").await.unwrap(),
        "active"
    );
    // The attempt is recorded either way, so a test can assert both that
    // the start was tried and what happened after it was refused.
    assert_eq!(
        control.calls(),
        vec![
            "start limited.service".to_string(),
            "start other.service".to_string(),
        ]
    );
}

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

/// Stand-in for systemd's manager on a private bus: it records each member
/// called, and reports a change for every unit except `unchanged.service`.
struct FakeManager {
    calls: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl FakeManager {
    fn record(&self, call: String) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call);
    }

    fn changes(files: &[String], kind: &str) -> Vec<(String, String, String)> {
        files
            .iter()
            .filter(|file| file.as_str() != "unchanged.service")
            .map(|file| {
                (
                    kind.to_string(),
                    format!("/run/systemd/system/multi-user.target.wants/{file}"),
                    format!("/usr/lib/systemd/system/{file}"),
                )
            })
            .collect()
    }
}

#[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
impl FakeManager {
    #[zbus(name = "EnableUnitFiles")]
    async fn enable_unit_files(
        &self,
        files: Vec<String>,
        runtime: bool,
        force: bool,
    ) -> (bool, Vec<(String, String, String)>) {
        self.record(format!("EnableUnitFiles {files:?} {runtime} {force}"));
        (true, Self::changes(&files, "symlink"))
    }

    #[zbus(name = "DisableUnitFiles")]
    async fn disable_unit_files(
        &self,
        files: Vec<String>,
        runtime: bool,
    ) -> Vec<(String, String, String)> {
        self.record(format!("DisableUnitFiles {files:?} {runtime}"));
        Self::changes(&files, "unlink")
    }

    #[zbus(name = "Reload")]
    async fn reload(&self) {
        self.record("Reload".to_string());
    }
}

/// Enabling or disabling a unit file that changes something reloads the
/// manager, as `systemctl enable` and `disable` do: until a reload, systemd
/// reports every unit's configuration as changed on disk. A call that changes
/// nothing leaves the manager alone.
#[tokio::test(flavor = "multi_thread")]
async fn a_changed_enablement_reloads_the_manager() -> anyhow::Result<()> {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};

    let Some(dbus_daemon) = ["/usr/bin/dbus-daemon", "/bin/dbus-daemon"]
        .into_iter()
        .find(|path| std::path::Path::new(path).exists())
    else {
        eprintln!("skipping a_changed_enablement_reloads_the_manager: dbus-daemon not found");
        return Ok(());
    };
    // A private session bus; never the host's system bus.
    let mut child = Command::new(dbus_daemon)
        .args(["--session", "--print-address=1", "--nofork"])
        .stdout(Stdio::piped())
        .spawn()?;
    let mut address = String::new();
    BufReader::new(child.stdout.take().expect("piped stdout")).read_line(&mut address)?;
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _bus = Kill(child);
    let address = address.trim();

    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let _server = zbus::connection::Builder::address(address)
        .name(MANAGER_DESTINATION)
        .serve_at(
            MANAGER_PATH,
            FakeManager {
                calls: std::sync::Arc::clone(&calls),
            },
        )
        .build()
        .await?;
    let systemd = Systemd::on(zbus::connection::Builder::address(address).build().await?);

    systemd.enable("mica-containerd.service").await?;
    systemd.enable("unchanged.service").await?;
    systemd.disable("mica-containerd.service").await?;
    systemd.disable("unchanged.service").await?;

    assert_eq!(
        *calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        [
            r#"EnableUnitFiles ["mica-containerd.service"] true true"#,
            "Reload",
            r#"EnableUnitFiles ["unchanged.service"] true true"#,
            r#"DisableUnitFiles ["mica-containerd.service"] true"#,
            "Reload",
            r#"DisableUnitFiles ["unchanged.service"] true"#,
        ]
    );
    Ok(())
}

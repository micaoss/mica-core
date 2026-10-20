use std::sync::{Arc, Mutex};

use micad_settings::{Init, ServicesSettings, UserUnit};

use super::*;
use crate::openrc::FakeCommands;

/// The init's verbs, recorded; every unit is `active`.
#[derive(Default)]
pub(crate) struct FakeControl {
    calls: Mutex<Vec<String>>,
}

impl FakeControl {
    pub(crate) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, verb: &str, unit: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("{verb} {unit}"));
        Ok(())
    }
}

#[async_trait::async_trait]
impl UnitControl for FakeControl {
    async fn active_state(&self, _unit: &str) -> Result<String> {
        Ok("active".to_string())
    }
    async fn unit_file_state(&self, _unit: &str) -> Result<String> {
        Ok("static".to_string())
    }
    async fn start(&self, unit: &str) -> Result<()> {
        self.record("start", unit)
    }
    async fn stop(&self, unit: &str) -> Result<()> {
        self.record("stop", unit)
    }
    async fn restart(&self, unit: &str) -> Result<()> {
        self.record("restart", unit)
    }
    async fn reset_failed(&self, unit: &str) -> Result<()> {
        self.record("reset-failed", unit)
    }
    async fn enable(&self, unit: &str) -> Result<()> {
        self.record("enable", unit)
    }
    async fn disable(&self, unit: &str) -> Result<()> {
        self.record("disable", unit)
    }
}

const SERVICE: &str = "[Service]\nExecStart=/srv/x\n[Install]\nWantedBy=multi-user.target\n";
const SCRIPT: &str = "#!/sbin/openrc-run\ncommand=/srv/x\n";

struct Rig {
    dir: tempfile::TempDir,
    commands: Arc<FakeCommands>,
    control: Arc<FakeControl>,
    units: UserUnits,
}

impl Rig {
    /// A root of `init` with mica-unit, its directory listing `listed`.
    fn new(init: Init, listed: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("usr/sbin/mica-unit");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        let commands = Arc::new(FakeCommands::default());
        commands.answer("/usr/sbin/mica-unit list", 0, listed);
        let control = Arc::new(FakeControl::default());
        let units = UserUnits::new(
            init,
            dir.path(),
            Arc::clone(&commands) as Arc<dyn Commands>,
            Arc::clone(&control) as Arc<dyn UnitControl>,
        );
        Self {
            dir,
            commands,
            control,
            units,
        }
    }

    /// The mica-unit command lines run, without the program and the list,
    /// with the scratch directory as `$T`.
    fn mica_unit(&self) -> Vec<String> {
        let scratch = self.dir.path().join(SCRATCH).display().to_string();
        self.commands
            .calls()
            .iter()
            .filter_map(|line| line.strip_prefix("/usr/sbin/mica-unit "))
            .filter(|line| *line != "list")
            .map(|line| line.replace(&scratch, "$T"))
            .collect()
    }

    /// Put `text` where mica-unit keeps `relative`.
    fn installed(&self, relative: &str, text: &str) {
        let path = self.dir.path().join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn record(&self, record: &[(&str, bool)]) {
        let record: BTreeMap<&str, bool> = record.iter().copied().collect();
        self.installed(RECORD, &serde_json::to_string(&record).unwrap());
    }

    fn recorded(&self) -> BTreeMap<String, bool> {
        self.units.read_record()
    }
}

fn services(units: &[(&str, Init, &str, bool)]) -> ServicesSettings {
    ServicesSettings {
        units: units
            .iter()
            .map(|(name, kind, text, enabled)| {
                (
                    (*name).to_string(),
                    UserUnit {
                        kind: *kind,
                        text: (*text).to_string(),
                        conf: None,
                        enabled: *enabled,
                    },
                )
            })
            .collect(),
    }
}

const SYSTEMD_DIR: &str = "usr/local/lib/systemd/system";

#[tokio::test]
async fn a_new_enabled_unit_is_installed_linked_and_started_once() {
    let rig = Rig::new(Init::Systemd, "");
    let declared = services(&[("x.service", Init::Systemd, SERVICE, true)]);

    let live = rig.units.apply(&declared).await.unwrap();

    assert_eq!(
        rig.mica_unit(),
        ["install $T/x.service", "enable x.service"]
    );
    assert_eq!(rig.control.calls(), ["start x.service"]);
    assert_eq!(rig.recorded(), BTreeMap::from([("x.service".into(), true)]));
    assert_eq!(live["units"]["x.service"]["enabled"], true);
}

#[tokio::test]
async fn an_unchanged_unit_is_left_alone() {
    let rig = Rig::new(Init::Systemd, "x.service\tenabled\n");
    rig.installed(&format!("{SYSTEMD_DIR}/x.service"), SERVICE);
    rig.record(&[("x.service", true)]);

    rig.units
        .apply(&services(&[("x.service", Init::Systemd, SERVICE, true)]))
        .await
        .unwrap();

    assert!(rig.mica_unit().is_empty(), "{:?}", rig.mica_unit());
    assert!(rig.control.calls().is_empty(), "{:?}", rig.control.calls());
}

#[tokio::test]
async fn a_changed_enabled_unit_is_linked_again_and_restarted() {
    let rig = Rig::new(Init::Systemd, "x.service\tenabled\n");
    rig.installed(
        &format!("{SYSTEMD_DIR}/x.service"),
        "[Service]\nExecStart=/old\n",
    );
    rig.record(&[("x.service", true)]);

    rig.units
        .apply(&services(&[("x.service", Init::Systemd, SERVICE, true)]))
        .await
        .unwrap();

    assert_eq!(
        rig.mica_unit(),
        [
            "install $T/x.service",
            "disable x.service",
            "enable x.service"
        ]
    );
    assert_eq!(rig.control.calls(), ["restart x.service"]);
}

#[tokio::test]
async fn a_unit_that_stops_being_enabled_is_unlinked_and_stopped() {
    let rig = Rig::new(Init::Systemd, "x.service\tenabled\n");
    rig.installed(&format!("{SYSTEMD_DIR}/x.service"), SERVICE);
    rig.record(&[("x.service", true)]);

    rig.units
        .apply(&services(&[("x.service", Init::Systemd, SERVICE, false)]))
        .await
        .unwrap();

    assert_eq!(rig.mica_unit(), ["disable x.service"]);
    assert_eq!(rig.control.calls(), ["stop x.service"]);
    assert_eq!(
        rig.recorded(),
        BTreeMap::from([("x.service".into(), false)])
    );
}

#[tokio::test]
async fn a_disabled_unit_is_never_stopped_again() {
    let rig = Rig::new(Init::Systemd, "x.timer\tenabled\nx.service\tdisabled\n");
    rig.installed(&format!("{SYSTEMD_DIR}/x.service"), SERVICE);
    rig.record(&[("x.service", false)]);

    rig.units
        .apply(&services(&[("x.service", Init::Systemd, SERVICE, false)]))
        .await
        .unwrap();

    assert!(rig.control.calls().is_empty(), "{:?}", rig.control.calls());
}

#[tokio::test]
async fn only_what_micad_applied_is_removed() {
    let rig = Rig::new(
        Init::Systemd,
        "gone.service\tenabled\nhand.service\tenabled\n",
    );
    rig.record(&[("gone.service", true), ("cleared.service", true)]);

    rig.units.apply(&ServicesSettings::default()).await.unwrap();

    assert_eq!(rig.mica_unit(), ["remove gone.service"]);
    assert_eq!(rig.control.calls(), ["stop gone.service"]);
    assert!(rig.recorded().is_empty(), "{:?}", rig.recorded());
}

#[tokio::test]
async fn a_hand_installed_unit_is_taken_over_when_declared() {
    let rig = Rig::new(Init::Systemd, "x.service\tenabled\n");
    rig.installed(&format!("{SYSTEMD_DIR}/x.service"), SERVICE);

    rig.units
        .apply(&services(&[("x.service", Init::Systemd, SERVICE, true)]))
        .await
        .unwrap();

    assert!(rig.mica_unit().is_empty(), "{:?}", rig.mica_unit());
    assert_eq!(rig.control.calls(), ["start x.service"]);
    assert_eq!(rig.recorded(), BTreeMap::from([("x.service".into(), true)]));
}

#[tokio::test]
async fn a_unit_of_the_other_init_is_stored_not_applied() {
    let rig = Rig::new(Init::Systemd, "");

    let live = rig
        .units
        .apply(&services(&[("x", Init::Openrc, SCRIPT, true)]))
        .await
        .unwrap();

    assert!(rig.mica_unit().is_empty());
    assert_eq!(live["refused"], serde_json::json!(["x"]));
}

#[tokio::test]
async fn a_unit_that_fails_is_named_and_the_others_still_apply() {
    let rig = Rig::new(Init::Systemd, "");
    rig.commands
        .answer("/usr/sbin/mica-unit enable bad.service", 1, "");

    let err = rig
        .units
        .apply(&services(&[
            ("bad.service", Init::Systemd, SERVICE, true),
            ("good.service", Init::Systemd, SERVICE, true),
        ]))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("bad.service"), "{err}");
    assert_eq!(rig.control.calls(), ["start good.service"]);
    assert_eq!(
        rig.recorded(),
        BTreeMap::from([("good.service".into(), true)])
    );
}

#[tokio::test]
async fn without_mica_unit_nothing_can_be_declared() {
    let rig = Rig::new(Init::Systemd, "");
    std::fs::remove_file(rig.dir.path().join("usr/sbin/mica-unit")).unwrap();

    let live = rig.units.apply(&ServicesSettings::default()).await.unwrap();
    assert_eq!(live["available"], false);
    let err = rig
        .units
        .apply(&services(&[("x.service", Init::Systemd, SERVICE, true)]))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no /usr/sbin/mica-unit"), "{err}");
}

#[tokio::test]
async fn an_openrc_script_and_its_conf_are_installed_and_started_by_name() {
    let rig = Rig::new(Init::Openrc, "");
    let mut declared = services(&[("tailscaled", Init::Openrc, SCRIPT, true)]);
    declared.units.get_mut("tailscaled").unwrap().conf = Some("PORT=41641\n".into());

    rig.units.apply(&declared).await.unwrap();

    assert_eq!(
        rig.mica_unit(),
        [
            "install $T/tailscaled",
            "install --conf $T/tailscaled.conf",
            "enable tailscaled"
        ]
    );
    assert_eq!(rig.control.calls(), ["start tailscaled.service"]);
}

#[tokio::test]
async fn an_openrc_script_whose_conf_changed_is_restarted_without_relinking() {
    let rig = Rig::new(Init::Openrc, "tailscaled\tenabled\n");
    rig.installed("usr/local/etc/init.d/tailscaled", SCRIPT);
    rig.installed("usr/local/etc/conf.d/tailscaled", "PORT=1\n");
    rig.record(&[("tailscaled", true)]);
    let mut declared = services(&[("tailscaled", Init::Openrc, SCRIPT, true)]);
    declared.units.get_mut("tailscaled").unwrap().conf = Some("PORT=2\n".into());

    rig.units.apply(&declared).await.unwrap();

    assert_eq!(rig.mica_unit(), ["install --conf $T/tailscaled.conf"]);
    assert_eq!(rig.control.calls(), ["restart tailscaled.service"]);
}

#[test]
fn a_write_the_device_cannot_apply_is_refused() {
    let rig = Rig::new(Init::Systemd, "");
    rig.installed("usr/lib/systemd/system/dropbear.service", "");
    let none = ServicesSettings::default();
    let refused = |next: ServicesSettings, says: &str| {
        let err = rig.units.check_written(&none, &next).unwrap_err();
        assert!(err.contains(says), "{err}");
    };

    rig.units
        .check_written(
            &none,
            &services(&[("x.service", Init::Systemd, SERVICE, true)]),
        )
        .unwrap();
    refused(
        services(&[("x", Init::Openrc, SCRIPT, true)]),
        "this device runs systemd",
    );
    refused(
        services(&[("dropbear.service", Init::Systemd, SERVICE, true)]),
        "dropbear.service is shipped by the root",
    );
    let alias = format!("{SERVICE}Alias=dropbear.service\n");
    refused(
        services(&[("x.service", Init::Systemd, &alias, true)]),
        "dropbear.service is shipped by the root",
    );

    // An entry the write does not touch is not held to it.
    let stored = services(&[("x", Init::Openrc, SCRIPT, true)]);
    rig.units.check_written(&stored, &stored).unwrap();

    std::fs::remove_file(rig.dir.path().join("usr/sbin/mica-unit")).unwrap();
    refused(
        services(&[("x.service", Init::Systemd, SERVICE, true)]),
        "no /usr/sbin/mica-unit",
    );
}

#[tokio::test]
async fn the_live_state_names_every_unit_and_who_manages_it() {
    let rig = Rig::new(
        Init::Systemd,
        "x.service\tenabled\nhand.service\tdisabled\n",
    );
    let declared = services(&[
        ("x.service", Init::Systemd, SERVICE, true),
        ("y", Init::Openrc, SCRIPT, true),
    ]);

    let live = rig.units.live(&declared).await.unwrap();

    assert_eq!(live["init"], "systemd");
    let units = live["units"].as_array().unwrap();
    let by = |name: &str| units.iter().find(|unit| unit["name"] == name).unwrap();
    assert_eq!(by("x.service")["managed"], true);
    assert_eq!(by("x.service")["enabled"], true);
    assert_eq!(by("x.service")["active"], "active");
    assert_eq!(by("hand.service")["managed"], false);
    assert_eq!(by("hand.service")["declared"], false);
    assert_eq!(by("y")["installed"], false);
    assert!(by("y")["refused"].as_str().unwrap().contains("systemd"));
}

#[tokio::test]
async fn a_verb_reaches_only_an_installed_unit() {
    let rig = Rig::new(Init::Openrc, "tailscaled\tenabled\n");

    rig.units.require_installed("tailscaled").await.unwrap();
    rig.units.act("tailscaled", Verb::Restart).await.unwrap();
    let err = rig.units.require_installed("sshd").await.unwrap_err();

    assert_eq!(rig.control.calls(), ["restart tailscaled.service"]);
    assert!(
        err.to_string().contains("no service named \"sshd\""),
        "{err}"
    );
}

/// The texts are written in a directory made afresh: one planted where it
/// goes is replaced, never written through.
#[tokio::test]
async fn a_planted_scratch_directory_is_not_written_through() {
    let rig = Rig::new(Init::Openrc, "");
    let elsewhere = rig.dir.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let scratch = rig.dir.path().join(SCRATCH);
    std::fs::create_dir_all(scratch.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &scratch).unwrap();

    rig.units
        .apply(&services(&[("x", Init::Openrc, SCRIPT, false)]))
        .await
        .unwrap();

    assert_eq!(rig.mica_unit(), ["install $T/x"]);
    assert!(std::fs::read_dir(&elsewhere).unwrap().next().is_none());
    assert!(!scratch.exists());
}

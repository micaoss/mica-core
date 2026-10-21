use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use micad_settings::{Init, ServicesSettings, UserUnit};
use serde_json::json;

use super::bundle::tests::{Member, write_bundle};
use super::*;
use crate::openrc::{Commands, Output};
use crate::reconciler::systemd::UnitControl;

/// `mica-unit`, kept in a map of unit to enabled.
#[derive(Default)]
struct FakeMicaUnit {
    units: Mutex<BTreeMap<String, bool>>,
    texts: Mutex<BTreeMap<String, String>>,
    calls: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl Commands for FakeMicaUnit {
    async fn run(&self, _program: &str, args: &[&str]) -> Result<Output> {
        let mut units = self.units.lock().unwrap();
        let mut stdout = String::new();
        match args {
            ["list"] => {
                for (name, enabled) in units.iter() {
                    let state = if *enabled { "enabled" } else { "disabled" };
                    stdout.push_str(&format!("{name}\t{state}\n"));
                }
            }
            ["install", path] => {
                let name = std::path::Path::new(path).file_name().unwrap();
                self.texts.lock().unwrap().insert(
                    name.to_string_lossy().into_owned(),
                    std::fs::read_to_string(path).unwrap_or_default(),
                );
                units
                    .entry(name.to_string_lossy().into_owned())
                    .or_insert(false);
                self.calls
                    .lock()
                    .unwrap()
                    .push(format!("install {}", name.to_string_lossy()));
            }
            ["remove", name] => {
                units.remove(*name);
                self.calls.lock().unwrap().push(format!("remove {name}"));
            }
            ["enable", name] => {
                units.insert((*name).to_string(), true);
            }
            ["disable", name] => {
                units.insert((*name).to_string(), false);
            }
            _ => {}
        }
        Ok(Output {
            code: 0,
            stdout,
            stderr: String::new(),
        })
    }
}

impl FakeMicaUnit {
    fn units(&self) -> BTreeMap<String, bool> {
        self.units.lock().unwrap().clone()
    }

    fn text(&self, unit: &str) -> String {
        self.texts
            .lock()
            .unwrap()
            .get(unit)
            .cloned()
            .unwrap_or_default()
    }
}

/// The init: every unit `active` once started, but one told to fail.
#[derive(Default)]
struct FakeControl {
    states: Mutex<BTreeMap<String, String>>,
    fail_next: Mutex<BTreeSet<String>>,
    calls: Mutex<Vec<String>>,
}

impl FakeControl {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn set(&self, unit: &str, state: &str) {
        self.states
            .lock()
            .unwrap()
            .insert(unit.into(), state.into());
    }
}

#[async_trait::async_trait]
impl UnitControl for FakeControl {
    async fn active_state(&self, unit: &str) -> Result<String> {
        Ok(self
            .states
            .lock()
            .unwrap()
            .get(unit)
            .cloned()
            .unwrap_or_else(|| "inactive".into()))
    }
    async fn unit_file_state(&self, _unit: &str) -> Result<String> {
        Ok("static".into())
    }
    async fn start(&self, unit: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("start {unit}"));
        let failed = self.fail_next.lock().unwrap().remove(unit);
        self.set(unit, if failed { "failed" } else { "active" });
        Ok(())
    }
    async fn stop(&self, unit: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("stop {unit}"));
        self.set(unit, "inactive");
        Ok(())
    }
    async fn restart(&self, unit: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("restart {unit}"));
        Ok(())
    }
    async fn reset_failed(&self, _unit: &str) -> Result<()> {
        Ok(())
    }
    async fn enable(&self, _unit: &str) -> Result<()> {
        Ok(())
    }
    async fn disable(&self, _unit: &str) -> Result<()> {
        Ok(())
    }
}

/// URLs answered from a map.
#[derive(Default)]
struct FakeFetch {
    files: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl Fetch for FakeFetch {
    fn fetch(&self, url: &str, path: &std::path::Path, limit: u64) -> Result<()> {
        let files = self.files.lock().unwrap();
        let body = files
            .get(url)
            .ok_or_else(|| anyhow::anyhow!("{url}: 404"))?;
        anyhow::ensure!(body.len() as u64 <= limit, "{url}: above {limit} bytes");
        std::fs::write(path, body)?;
        Ok(())
    }
}

struct Rig {
    dir: tempfile::TempDir,
    mica: Arc<FakeMicaUnit>,
    control: Arc<FakeControl>,
    fetch: Arc<FakeFetch>,
    apps: Arc<Apps>,
}

impl Rig {
    fn new(init: Init) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let program = dir.path().join("usr/sbin/mica-unit");
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "#!/bin/sh\n").unwrap();
        let mica = Arc::new(FakeMicaUnit::default());
        let control = Arc::new(FakeControl::default());
        let fetch = Arc::new(FakeFetch::default());
        let units = Arc::new(UserUnits::new(
            init,
            dir.path(),
            Arc::clone(&mica) as Arc<dyn Commands>,
            Arc::clone(&control) as Arc<dyn UnitControl>,
        ));
        // setpriv's stand-in runs the command as the test's own user, which
        // is the first app uid.
        let setpriv = dir.path().join("setpriv");
        std::fs::write(
            &setpriv,
            "#!/bin/sh\nwhile [ \"$1\" != -- ]; do shift; done\nshift\nexec \"$@\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&setpriv, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut apps = Apps::new(dir.path(), units, Arc::clone(&fetch) as Arc<dyn Fetch>);
        apps.uid_base = rustix::process::geteuid().as_raw();
        apps.setpriv = setpriv;
        let apps = Arc::new(apps);
        Self {
            dir,
            mica,
            control,
            fetch,
            apps,
        }
    }

    fn path(&self, relative: &str) -> std::path::PathBuf {
        self.dir.path().join(relative)
    }

    /// A bundle of `name` at `version` with `extra` in its manifest, and a
    /// first-run hook when `hook` is given.
    fn bundle(
        &self,
        name: &str,
        version: &str,
        extra: serde_json::Value,
        hook: Option<&str>,
    ) -> std::path::PathBuf {
        // An app of its own user declares services; the others ship units.
        let own_user = extra.get("services").is_some();
        let mut manifest = json!({
            "schemaVersion": 1,
            "name": name,
            "version": version,
            "arch": ["all"],
        });
        if !own_user {
            manifest["units"] =
                json!([{ "systemd": format!("{name}.service"), "openrc": name, "enable": true }]);
            manifest["privileges"] = json!({ "user": "root" });
        }
        for (key, value) in extra.as_object().unwrap() {
            manifest[key] = value.clone();
        }
        if hook.is_some() {
            manifest["firstRun"] = json!("hooks/first-run");
        }
        let manifest = manifest.to_string();
        let unit = format!("[Service]\nExecStart=/mica/apps/{name}/current/files/run\n");
        let path = self.path(&format!("{name}-{version}.tar.gz"));
        let mut members = vec![
            Member::File("mica-app.json", manifest.as_bytes(), 0o644),
            Member::File("files/run", b"#!/bin/sh\nexec sleep 1000\n", 0o755),
        ];
        let unit_path = format!("units/systemd/{name}.service");
        let script_path = format!("units/openrc/{name}");
        if !own_user {
            members.push(Member::File(&unit_path, unit.as_bytes(), 0o644));
            members.push(Member::File(&script_path, b"#!/sbin/openrc-run\n", 0o755));
        }
        if let Some(hook) = hook {
            members.push(Member::File("hooks/first-run", hook.as_bytes(), 0o755));
        }
        write_bundle(&path, &members);
        path
    }

    async fn install(
        &self,
        archive: &std::path::Path,
        reinstall: bool,
    ) -> Result<serde_json::Value, AppError> {
        self.apps
            .install(
                archive,
                "upload",
                InstallOptions {
                    reinstall,
                    ..Default::default()
                },
                &ServicesSettings::default(),
            )
            .await
    }

    fn current(&self, name: &str) -> String {
        std::fs::read_link(self.path(&format!("mica/apps/{name}/current")))
            .unwrap()
            .display()
            .to_string()
    }

    fn record(&self) -> Record {
        self.apps.read_record()
    }
}

fn rules(err: AppError) -> Vec<&'static str> {
    match err {
        AppError::Invalid(problems) => problems.iter().map(|p| p.rule).collect(),
        other => panic!("not refused as invalid: {other}"),
    }
}

#[tokio::test]
async fn an_app_installs_upgrades_and_rolls_back() {
    let rig = Rig::new(Init::Systemd);
    rig.install(
        &rig.bundle("demo", "1.0", json!({ "env": { "PORT": "8080" } }), None),
        false,
    )
    .await
    .unwrap();
    assert_eq!(rig.current("demo"), "1.0");
    assert_eq!(
        rig.mica.units(),
        BTreeMap::from([("demo.service".into(), true)])
    );
    assert!(rig.control.calls().contains(&"start demo.service".into()));
    assert_eq!(
        std::fs::read_to_string(rig.path("mica/apps/demo/app.env")).unwrap(),
        "APP_NAME=\"demo\"\nAPP_VERSION=\"1.0\"\nAPP_DIR=\"/mica/apps/demo/current\"\n\
         APP_DATA=\"/srv/apps/demo\"\nPORT=\"8080\"\n"
    );
    let data = std::fs::metadata(rig.path("srv/apps/demo")).unwrap();
    assert_eq!(data.permissions().mode() & 0o777, 0o700);

    rig.install(&rig.bundle("demo", "2.0", json!({}), None), false)
        .await
        .unwrap();
    rig.install(&rig.bundle("demo", "3.0", json!({}), None), false)
        .await
        .unwrap();
    assert_eq!(rig.current("demo"), "3.0");
    let entry = &rig.record().apps["demo"];
    assert_eq!(entry.previous.as_ref().unwrap().version, "2.0");
    assert!(
        !rig.path("mica/apps/demo/1.0").exists(),
        "only two versions are kept"
    );

    rig.apps.rollback("demo").await.unwrap();
    assert_eq!(rig.current("demo"), "2.0");
    let entry = &rig.record().apps["demo"];
    assert_eq!(entry.current.version, "2.0");
    assert_eq!(entry.previous.as_ref().unwrap().version, "3.0");
    let view = rig.apps.view(Some("demo")).await.unwrap();
    assert_eq!(view["version"], json!("2.0"));
    assert_eq!(view["previous"], json!("3.0"));
    assert_eq!(
        view["units"][0],
        json!({ "name": "demo.service", "enabled": true, "active": "active" })
    );
}

#[tokio::test]
async fn the_same_version_is_installed_again_only_on_reinstall() {
    let rig = Rig::new(Init::Systemd);
    let bundle = rig.bundle("demo", "1.0", json!({}), None);
    rig.install(&bundle, false).await.unwrap();
    assert!(matches!(
        rig.install(&bundle, false).await,
        Err(AppError::Installed(_))
    ));
    rig.install(&bundle, true).await.unwrap();
    assert_eq!(rig.current("demo"), "1.0");
    assert!(rig.record().apps["demo"].previous.is_none());
    assert!(!rig.path("mica/apps/demo/.1.0.replaced").exists());
}

#[tokio::test]
async fn a_failing_upgrade_goes_back_to_what_ran() {
    let rig = Rig::new(Init::Systemd);
    rig.install(&rig.bundle("demo", "1.0", json!({}), None), false)
        .await
        .unwrap();
    rig.control
        .fail_next
        .lock()
        .unwrap()
        .insert("demo.service".into());
    let Err(AppError::Failed(err)) = rig
        .install(&rig.bundle("demo", "2.0", json!({}), None), false)
        .await
    else {
        panic!("the upgrade fails")
    };
    assert!(format!("{err:#}").contains("rolled back to 1.0"), "{err:#}");
    assert_eq!(rig.current("demo"), "1.0");
    assert!(!rig.path("mica/apps/demo/2.0").exists());
    let entry = &rig.record().apps["demo"];
    assert_eq!(entry.current.version, "1.0");
    assert_eq!(entry.last.as_ref().unwrap().outcome, "failed");
    assert_eq!(
        rig.control.active_state("demo.service").await.unwrap(),
        "active"
    );
}

#[tokio::test]
async fn a_failing_first_install_leaves_nothing() {
    let rig = Rig::new(Init::Systemd);
    let bundle = rig.bundle(
        "demo",
        "1.0",
        json!({}),
        Some("#!/bin/sh\necho broken\nexit 3\n"),
    );
    assert!(matches!(
        rig.install(&bundle, false).await,
        Err(AppError::Failed(_))
    ));
    assert!(!rig.path("mica/apps/demo").exists());
    assert!(!rig.path("srv/apps/demo").exists());
    assert!(rig.mica.units().is_empty());
    assert!(rig.record().apps.is_empty());
}

#[tokio::test]
async fn the_first_run_hook_runs_once_per_version() {
    let rig = Rig::new(Init::Systemd);
    let hook = "#!/bin/sh\necho \"$APP_NAME $APP_VERSION $APP_DATA\" >> ../ran\necho hello\n";
    let bundle = rig.bundle("demo", "1.0", json!({}), Some(hook));
    rig.install(&bundle, false).await.unwrap();
    rig.install(&bundle, true).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(rig.path("mica/apps/demo/ran")).unwrap(),
        "demo 1.0 /srv/apps/demo\n"
    );
    assert_eq!(
        std::fs::read_to_string(rig.path("mica/apps/demo/first-run.log")).unwrap(),
        "hello\n"
    );
    rig.install(&rig.bundle("demo", "2.0", json!({}), Some(hook)), false)
        .await
        .unwrap();
    rig.apps.rollback("demo").await.unwrap();
    assert_eq!(
        std::fs::read_to_string(rig.path("mica/apps/demo/ran")).unwrap(),
        "demo 1.0 /srv/apps/demo\ndemo 2.0 /srv/apps/demo\n"
    );
}

#[tokio::test]
async fn a_unit_someone_else_has_is_refused() {
    let rig = Rig::new(Init::Systemd);
    rig.install(&rig.bundle("one", "1.0", json!({}), None), false)
        .await
        .unwrap();
    let taken = json!({ "units": [{ "systemd": "one.service", "enable": true }] });
    let err = rig
        .apps
        .admit(
            &rig.bundle("two", "1.0", taken, None),
            InstallOptions::default(),
            &ServicesSettings::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(rules(err), ["unit_taken"]);

    let services = ServicesSettings {
        units: BTreeMap::from([(
            "demo.service".to_string(),
            UserUnit {
                kind: Init::Systemd,
                text: String::new(),
                conf: None,
                enabled: true,
            },
        )]),
    };
    let bundle = rig.bundle("demo", "1.0", json!({}), None);
    let err = rig
        .apps
        .admit(&bundle, InstallOptions::default(), &services)
        .await
        .unwrap_err();
    assert_eq!(rules(err), ["unit_taken"]);

    std::fs::create_dir_all(rig.path("usr/lib/systemd/system")).unwrap();
    std::fs::write(rig.path("usr/lib/systemd/system/demo.service"), "").unwrap();
    let err = rig
        .apps
        .admit(
            &bundle,
            InstallOptions::default(),
            &ServicesSettings::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(rules(err), ["unit_shipped_by_root"]);
    assert_eq!(
        rig.apps.unit_owners(),
        BTreeMap::from([("one.service".to_string(), "one".to_string())])
    );
}

#[tokio::test]
async fn an_upgrade_of_an_absent_app_and_a_bundle_for_the_other_init_are_refused() {
    let rig = Rig::new(Init::Openrc);
    let bundle = rig.bundle(
        "demo",
        "1.0",
        json!({ "units": [{ "systemd": "demo.service", "enable": true }] }),
        None,
    );
    let err = rig
        .apps
        .admit(
            &bundle,
            InstallOptions::default(),
            &ServicesSettings::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(rules(err), ["init"]);
    let bundle = rig.bundle("demo", "1.0", json!({}), None);
    assert!(matches!(
        rig.apps
            .admit(
                &bundle,
                InstallOptions {
                    upgrade: true,
                    ..Default::default()
                },
                &ServicesSettings::default()
            )
            .await,
        Err(AppError::NotFound(_))
    ));
    rig.install(&bundle, false).await.unwrap();
    assert_eq!(rig.mica.units(), BTreeMap::from([("demo".into(), true)]));
    assert!(rig.control.calls().contains(&"start demo.service".into()));
}

#[tokio::test]
async fn remove_keeps_the_data_unless_purged() {
    let rig = Rig::new(Init::Systemd);
    rig.install(&rig.bundle("demo", "1.0", json!({}), None), false)
        .await
        .unwrap();
    rig.apps.remove("demo", false).await.unwrap();
    assert!(!rig.path("mica/apps/demo").exists());
    assert!(rig.path("srv/apps/demo").exists());
    assert!(rig.mica.units().is_empty());
    assert!(rig.control.calls().contains(&"stop demo.service".into()));
    rig.install(&rig.bundle("demo", "1.0", json!({}), None), false)
        .await
        .unwrap();
    rig.apps.remove("demo", true).await.unwrap();
    assert!(!rig.path("srv/apps/demo").exists());
    assert!(matches!(
        rig.apps.remove("demo", true).await,
        Err(AppError::NotFound(_))
    ));
}

/// An app of its own user: micad writes its units, which run it through
/// setpriv with what it declares, its hook runs as it, and its uid is kept
/// through a remove until a purge.
#[tokio::test]
async fn an_app_runs_as_its_own_user() {
    let rig = Rig::new(Init::Systemd);
    let base = rig.apps.uid_base;
    let own = json!({
        "privileges": { "capabilities": ["CAP_NET_ADMIN"], "devices": ["/dev/net/tun"] },
        "services": [{ "name": "demo", "exec": ["files/run", "--data=${APP_DATA}"], "enable": true }],
    });
    let hook = "#!/bin/sh\necho \"$HOME\" > ../home\n";
    rig.install(&rig.bundle("demo", "1.0", own.clone(), Some(hook)), false)
        .await
        .unwrap();
    let text = rig.mica.text("demo.service");
    assert!(
        text.contains(&format!(
            "ExecStart=\"/usr/bin/setpriv\" \"--reuid={base}\" \"--regid={base}\" \"--clear-groups\" \"--bounding-set=-all,+net_admin\""
        )),
        "{text}"
    );
    assert!(text.contains("\"/mica/apps/demo/current/files/run\" \"--data=/srv/apps/demo\""));
    assert!(text.contains("DeviceAllow=/dev/net/tun rw"));
    assert_eq!(
        rig.mica.units(),
        BTreeMap::from([("demo.service".into(), true)])
    );
    assert_eq!(
        std::fs::read_to_string(rig.path("mica/apps/demo/home")).unwrap(),
        "/srv/apps/demo\n"
    );
    assert_eq!(rig.record().uids, BTreeMap::from([("demo".into(), base)]));
    let view = rig.apps.view(Some("demo")).await.unwrap();
    assert_eq!(view["privileges"]["user"], json!("app"));
    assert_eq!(view["privileges"]["account"], json!("mica-app-demo"));
    assert_eq!(view["privileges"]["uid"], json!(base));
    assert_eq!(view["privileges"]["capabilities"], json!(["CAP_NET_ADMIN"]));

    // A second app gets the next uid; a removed one keeps its own.
    let other = json!({ "services": [{ "name": "other", "exec": ["files/run"] }] });
    rig.install(&rig.bundle("other", "1.0", other, None), false)
        .await
        .unwrap();
    rig.apps.remove("demo", false).await.unwrap();
    assert_eq!(rig.record().uids["demo"], base);
    assert_eq!(rig.record().uids["other"], base + 1);
    rig.install(&rig.bundle("demo", "1.0", own, None), false)
        .await
        .unwrap();
    assert_eq!(rig.record().uids["demo"], base);
    rig.apps.remove("demo", true).await.unwrap();
    assert!(!rig.record().uids.contains_key("demo"));

    // A root app shows as root.
    rig.install(&rig.bundle("tool", "1.0", json!({}), None), false)
        .await
        .unwrap();
    let view = rig.apps.view(Some("tool")).await.unwrap();
    assert_eq!(view["privileges"]["user"], json!("root"));
    assert_eq!(view["privileges"]["uid"], json!(null));
}

/// A device whose group reaches past it, or a node that is not a character
/// device, is refused: its group would be the app's.
#[tokio::test]
async fn a_device_that_reaches_past_the_app_is_refused() {
    use std::os::unix::fs::MetadataExt;
    let rig = Rig::new(Init::Systemd);
    std::fs::create_dir_all(rig.path("dev")).unwrap();
    std::fs::write(rig.path("dev/sdz"), "").unwrap();
    let gid = std::fs::metadata(rig.path("dev/sdz")).unwrap().gid();
    std::fs::create_dir_all(rig.path("etc")).unwrap();
    std::fs::write(rig.path("etc/group"), format!("disk:x:{gid}:\n")).unwrap();
    let own = json!({
        "privileges": { "devices": ["/dev/sdz"] },
        "services": [{ "name": "demo", "exec": ["files/run"] }],
    });
    let err = rig
        .apps
        .admit(
            &rig.bundle("demo", "1.0", own, None),
            InstallOptions::default(),
            &ServicesSettings::default(),
        )
        .await
        .unwrap_err();
    assert_eq!(rules(err), ["device"]);
}

#[tokio::test]
async fn a_character_device_of_group_0_is_given() {
    let rig = Rig::new(Init::Systemd);
    std::fs::create_dir_all(rig.path("dev")).unwrap();
    std::os::unix::fs::symlink("/dev/null", rig.path("dev/null")).unwrap();
    let own = json!({
        "privileges": { "devices": ["/dev/null"] },
        "services": [{ "name": "demo", "exec": ["files/run"] }],
    });
    rig.apps
        .admit(
            &rig.bundle("demo", "1.0", own, None),
            InstallOptions::default(),
            &ServicesSettings::default(),
        )
        .await
        .unwrap();
}

#[test]
fn a_group_that_reaches_past_the_app_or_has_no_name_is_refused() {
    let groups = "root:x:0:\ndisk:x:6:\nwheel:x:10:\ndialout:x:20:\n";
    assert_eq!(flow::group_refusal(0, None), None);
    assert_eq!(flow::group_refusal(20, Some(groups)), None);
    for (gid, text) in [
        (6, Some(groups)),
        (10, Some(groups)),
        (44, Some(groups)),
        (20, None),
    ] {
        assert!(flow::group_refusal(gid, text).is_some(), "{gid}");
    }
}

/// Under OpenRC the service is a script micad writes, under supervise-daemon.
#[tokio::test]
async fn an_openrc_app_of_its_own_user_gets_a_written_script() {
    let rig = Rig::new(Init::Openrc);
    let own = json!({ "services": [{ "name": "demo", "exec": ["files/run"], "enable": true }] });
    rig.install(&rig.bundle("demo", "1.0", own, None), false)
        .await
        .unwrap();
    let text = rig.mica.text("demo");
    assert!(text.starts_with("#!/sbin/openrc-run\n"));
    assert!(text.contains("supervisor=supervise-daemon"));
    assert!(text.contains("command=/usr/bin/setpriv"));
}

#[tokio::test]
async fn one_flow_runs_at_a_time() {
    let rig = Rig::new(Init::Systemd);
    let busy = rig.apps.begin().unwrap();
    assert!(matches!(rig.apps.begin(), Err(AppError::Busy)));
    drop(busy);
    rig.apps.begin().unwrap();
    assert!(matches!(
        Arc::new(Apps::unavailable()).begin(),
        Err(AppError::Refused(_))
    ));
}

/// Put `bundle` in the image as `name`'s seed.
fn seed_with(rig: &Rig, name: &str, bundle: &std::path::Path) {
    let dir = rig.path("usr/share/mica/apps");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(bundle, dir.join(format!("{name}.tar.gz"))).unwrap();
}

#[tokio::test]
async fn a_seed_is_installed_once_and_followed_while_untouched() {
    let rig = Rig::new(Init::Systemd);
    let services = ServicesSettings::default();
    seed_with(&rig, "demo", &rig.bundle("demo", "1.0", json!({}), None));
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "demo": "installed" })
    );
    assert_eq!(rig.record().apps["demo"].current.source, "seed");
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "demo": "unchanged" })
    );

    // A later image with a newer seed: the untouched app follows it.
    seed_with(&rig, "demo", &rig.bundle("demo", "2.0", json!({}), None));
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "demo": "upgraded" })
    );
    assert_eq!(rig.current("demo"), "2.0");

    // An older image after a system rollback never downgrades.
    seed_with(&rig, "demo", &rig.bundle("demo", "1.0", json!({}), None));
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "demo": "not newer" })
    );

    // The user's own version is never overwritten.
    rig.install(&rig.bundle("demo", "2.5", json!({}), None), false)
        .await
        .unwrap();
    seed_with(&rig, "demo", &rig.bundle("demo", "3.0", json!({}), None));
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "demo": "changed by the user" })
    );
    assert_eq!(rig.current("demo"), "2.5");
    // Rolled back to the seeded version, it follows the image again.
    rig.apps.rollback("demo").await.unwrap();
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "demo": "upgraded" })
    );
    assert_eq!(rig.current("demo"), "3.0");
    assert_eq!(
        rig.apps.view(Some("demo")).await.unwrap()["seed"]["version"],
        json!("3.0")
    );
}

#[tokio::test]
async fn a_seed_follows_its_policy_and_the_users_app() {
    let rig = Rig::new(Init::Systemd);
    let services = ServicesSettings::default();
    rig.install(&rig.bundle("mine", "1.0", json!({}), None), false)
        .await
        .unwrap();
    seed_with(&rig, "mine", &rig.bundle("mine", "0.5", json!({}), None));
    seed_with(
        &rig,
        "once",
        &rig.bundle("once", "1.0", json!({ "seedPolicy": "once" }), None),
    );
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "mine": "skipped", "once": "installed" })
    );
    seed_with(
        &rig,
        "once",
        &rig.bundle("once", "2.0", json!({ "seedPolicy": "once" }), None),
    );
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "mine": "unchanged", "once": "once" })
    );
    assert_eq!(rig.current("mine"), "1.0");
    assert_eq!(rig.current("once"), "1.0");
}

#[tokio::test]
async fn a_failing_seed_is_tried_at_one_more_boot() {
    let rig = Rig::new(Init::Systemd);
    let services = ServicesSettings::default();
    seed_with(&rig, "demo", &rig.bundle("other", "1.0", json!({}), None));
    let first = rig.apps.seed(&services).await;
    assert!(
        first["demo"]["error"]
            .as_str()
            .unwrap()
            .contains("holds app other")
    );
    let second = rig.apps.seed(&services).await;
    assert!(second["demo"]["error"].is_string(), "tried again");
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "demo": "unchanged" })
    );
    let record = rig.apps.read_seed_record();
    assert_eq!(
        (record["demo"].result.as_str(), record["demo"].tries),
        ("failed", 2)
    );
    // A new image's seed is tried.
    seed_with(&rig, "demo", &rig.bundle("demo", "1.0", json!({}), None));
    assert_eq!(
        rig.apps.seed(&services).await,
        json!({ "demo": "installed" })
    );
}

#[tokio::test]
async fn an_update_is_offered_fetched_and_checked() {
    let rig = Rig::new(Init::Systemd);
    let services = ServicesSettings::default();
    let update = json!({ "update": { "url": "https://apps.example/demo/latest.json" } });
    rig.install(&rig.bundle("demo", "1.0", update.clone(), None), false)
        .await
        .unwrap();
    let next = rig.bundle("demo", "2.0", update, None);
    let body = std::fs::read(&next).unwrap();
    let sha256 = bundle::sha256_file(&next).unwrap();
    let version_file = |sha256: &str| {
        json!({
            "name": "demo",
            "version": "2.0",
            "bundles": { "all": { "url": "demo-2.0.tar.gz", "sha256": sha256, "size": body.len() } },
        })
        .to_string()
        .into_bytes()
    };
    {
        let mut files = rig.fetch.files.lock().unwrap();
        files.insert(
            "https://apps.example/demo/latest.json".into(),
            version_file(&"0".repeat(64)),
        );
        files.insert(
            "https://apps.example/demo/demo-2.0.tar.gz".into(),
            body.clone(),
        );
    }
    let checked = rig.apps.check_update("demo").await.unwrap();
    assert_eq!(checked["available"], json!(true));
    let Err(AppError::Failed(err)) = rig.apps.update("demo", &services).await else {
        panic!("a bundle of another sha256 is refused")
    };
    assert!(format!("{err:#}").contains("not the one"), "{err:#}");
    assert_eq!(
        rig.apps.check_update("demo").await.unwrap()["available"],
        json!(false)
    );

    let mut record = rig.record();
    record.apps.get_mut("demo").unwrap().update.failed = None;
    rig.apps.write_record(&record).unwrap();
    rig.fetch.files.lock().unwrap().insert(
        "https://apps.example/demo/latest.json".into(),
        version_file(&sha256),
    );
    rig.apps.update("demo", &services).await.unwrap();
    let entry = &rig.record().apps["demo"];
    assert_eq!(
        (
            entry.current.version.as_str(),
            entry.current.source.as_str()
        ),
        ("2.0", "url")
    );
    assert!(matches!(
        rig.apps.update("demo", &services).await,
        Err(AppError::Refused(_))
    ));
    assert!(
        std::fs::read_dir(rig.path("mica/apps/.incoming"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[tokio::test]
async fn the_logs_are_the_units_and_the_first_run() {
    let rig = Rig::new(Init::Systemd);
    rig.install(
        &rig.bundle("demo", "1.0", json!({}), Some("#!/bin/sh\necho ready\n")),
        false,
    )
    .await
    .unwrap();
    let logs = rig.apps.logs("demo", &crate::logs::NoLogs).await.unwrap();
    assert_eq!(logs["firstRun"], json!(["ready"]));
    assert_eq!(logs["units"][0]["unit"], json!("demo.service"));
}

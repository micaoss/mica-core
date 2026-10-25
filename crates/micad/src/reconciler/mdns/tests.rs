use std::sync::{Arc, Mutex};

use micad_settings::WebMode;

use super::*;

/// What the recording responder was asked, in order.
type Calls = Arc<Mutex<Vec<String>>>;

struct Recorder(Calls);

impl Responder for Recorder {
    fn publish(&mut self, hostname: &str, service: &Service) -> Result<()> {
        self.0.lock().unwrap().push(format!(
            "publish {} on {hostname}.local",
            service.fullname()
        ));
        Ok(())
    }

    fn withdraw(&mut self, fullname: &str) -> Result<()> {
        self.0.lock().unwrap().push(format!("withdraw {fullname}"));
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("stop".to_string());
    }
}

struct StartRecorder(Calls);

impl StartResponder for StartRecorder {
    fn start(&self) -> Result<Box<dyn Responder>> {
        self.0.lock().unwrap().push("start".to_string());
        Ok(Box::new(Recorder(Arc::clone(&self.0))))
    }
}

fn product(default_on: bool) -> Product {
    Product {
        name: Some("cx3576.full".to_string()),
        default_on,
    }
}

/// Apps' entries as a test sets them.
#[derive(Default)]
struct FakeApps(Mutex<Vec<(String, Vec<entries::Entry>)>>);

#[async_trait::async_trait]
impl AppEntries for FakeApps {
    async fn running(&self) -> Vec<(String, Vec<entries::Entry>)> {
        self.0.lock().unwrap().clone()
    }
}

/// A reconciler with empty drop-in directories, and the directories.
struct Rig {
    reconciler: MdnsReconciler,
    calls: Calls,
    apps: Arc<FakeApps>,
    image: tempfile::TempDir,
    runtime: tempfile::TempDir,
}

fn rig(default_on: bool) -> Rig {
    let calls = Calls::default();
    let apps = Arc::new(FakeApps::default());
    let (image, runtime) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let reconciler = MdnsReconciler::new(
        product(default_on),
        Sources {
            image_dir: image.path().to_path_buf(),
            runtime_dir: runtime.path().to_path_buf(),
            owner: rustix::process::geteuid().as_raw(),
            apps: Some(Arc::clone(&apps) as Arc<dyn AppEntries>),
        },
        Box::new(StartRecorder(Arc::clone(&calls))),
    );
    Rig {
        reconciler,
        calls,
        apps,
        image,
        runtime,
    }
}

fn reconciler(default_on: bool) -> (MdnsReconciler, Calls) {
    let rig = rig(default_on);
    (rig.reconciler, rig.calls)
}

fn entry(kind: &str, port: u16) -> entries::Entry {
    entries::Entry {
        instance: None,
        kind: kind.to_string(),
        port,
        txt: BTreeMap::new(),
        service: None,
    }
}

fn taken(calls: &Calls) -> Vec<String> {
    std::mem::take(&mut *calls.lock().unwrap())
}

fn settings() -> Settings {
    Settings {
        hostname: "mica-ab12".to_string(),
        ..Settings::default()
    }
}

fn kinds(services: &[Service]) -> Vec<&str> {
    services.iter().map(|s| s.kind.as_str()).collect()
}

#[test]
fn a_default_device_publishes_itself_and_its_console() {
    let services = built_in_services(&settings(), &product(true));

    assert_eq!(kinds(&services), ["_mica._tcp", "_http._tcp"]);
    let mica = &services[0];
    assert_eq!(mica.instance, "mica-ab12");
    assert_eq!(mica.port, 8080);
    assert_eq!(
        mica.txt,
        BTreeMap::from([
            ("txtvers".to_string(), "1".to_string()),
            ("product".to_string(), "cx3576.full".to_string()),
            ("hostname".to_string(), "mica-ab12".to_string()),
        ])
    );
    assert_eq!(services[1].port, 8080);
    assert_eq!(services[1].txt["path"], "/");
    assert!(services.iter().all(|s| s.source == "built-in"));
}

/// The TXT says what the device is and nothing that opens it: no version,
/// no claim state, no device code.
#[test]
fn the_mica_txt_carries_txtvers_product_and_hostname_only() {
    let services = built_in_services(&settings(), &product(true));
    let keys: Vec<&str> = services[0].txt.keys().map(String::as_str).collect();
    assert_eq!(keys, ["hostname", "product", "txtvers"]);

    let nameless = Product {
        name: None,
        default_on: true,
    };
    let services = built_in_services(&settings(), &nameless);
    assert!(!services[0].txt.contains_key("product"));
}

#[test]
fn the_console_is_https_on_its_port_when_https_is_on() {
    let mut settings = settings();
    settings.access.web.https_enabled = true;
    settings.access.web.https_port = 9443;

    let services = built_in_services(&settings, &product(true));

    assert_eq!(kinds(&services), ["_mica._tcp", "_https._tcp"]);
    assert_eq!(services[0].port, 9443);
    assert_eq!(services[1].port, 9443);
}

/// No console on the network, no console announced; the device itself still
/// is, with port 0.
#[test]
fn a_console_off_the_network_is_not_announced() {
    for mode in [WebMode::Local, WebMode::Off] {
        let mut settings = settings();
        settings.access.web.mode = mode;

        let services = built_in_services(&settings, &product(true));

        assert_eq!(kinds(&services), ["_mica._tcp"], "{mode:?}");
        assert_eq!(services[0].port, 0);
    }
}

#[tokio::test]
async fn on_starts_the_responder_and_publishes_once() {
    let (reconciler, calls) = reconciler(true);

    let state = reconciler.apply(&settings()).await.unwrap();

    assert_eq!(
        taken(&calls),
        [
            "start",
            "publish mica-ab12._mica._tcp.local. on mica-ab12.local",
            "publish mica-ab12._http._tcp.local. on mica-ab12.local",
        ]
    );
    assert_eq!(state["enabled"], json!(true));
    assert_eq!(state["source"], json!("product"));
    assert_eq!(state["hostname"], json!("mica-ab12.local"));
    assert_eq!(state["services"].as_array().unwrap().len(), 2);
    assert_eq!(reconciler.name(), "mdns");
    assert_eq!(reconciler.subtree(), "access.mdns");
    assert_eq!(reconciler.reads(), ["hostname", "access.web"]);

    // The same settings again change nothing on the wire.
    reconciler.apply(&settings()).await.unwrap();
    assert!(taken(&calls).is_empty());
}

#[tokio::test]
async fn a_change_withdraws_what_went_and_publishes_what_came() {
    let (reconciler, calls) = reconciler(true);
    reconciler.apply(&settings()).await.unwrap();
    taken(&calls);

    let mut next = settings();
    next.access.web.https_enabled = true;
    // SSH is never announced.
    next.access.ssh.enabled = true;
    reconciler.apply(&next).await.unwrap();

    assert_eq!(
        taken(&calls),
        [
            // The _mica port moved to the HTTPS port, so it is withdrawn too.
            "withdraw mica-ab12._http._tcp.local.",
            "withdraw mica-ab12._mica._tcp.local.",
            "publish mica-ab12._mica._tcp.local. on mica-ab12.local",
            "publish mica-ab12._https._tcp.local. on mica-ab12.local",
        ]
    );
}

#[tokio::test]
async fn a_new_hostname_withdraws_every_record_and_publishes_under_it() {
    let (reconciler, calls) = reconciler(true);
    reconciler.apply(&settings()).await.unwrap();
    taken(&calls);

    let mut renamed = settings();
    renamed.hostname = "bridge".to_string();
    let state = reconciler.apply(&renamed).await.unwrap();

    assert_eq!(
        taken(&calls),
        [
            "withdraw mica-ab12._http._tcp.local.",
            "withdraw mica-ab12._mica._tcp.local.",
            "publish bridge._mica._tcp.local. on bridge.local",
            "publish bridge._http._tcp.local. on bridge.local",
        ]
    );
    assert_eq!(state["hostname"], json!("bridge.local"));
}

/// Off is no responder at all: everything withdrawn, the socket closed, and
/// nothing started while it stays off.
#[tokio::test]
async fn off_withdraws_everything_and_stops_the_responder() {
    let (reconciler, calls) = reconciler(true);
    reconciler.apply(&settings()).await.unwrap();
    taken(&calls);

    let mut off = settings();
    off.access.mdns.enabled = Some(false);
    let state = reconciler.apply(&off).await.unwrap();

    assert_eq!(
        taken(&calls),
        [
            "withdraw mica-ab12._http._tcp.local.",
            "withdraw mica-ab12._mica._tcp.local.",
            "stop",
        ]
    );
    assert_eq!(state["enabled"], json!(false));
    assert_eq!(state["source"], json!("setting"));
    // Still listed, as not published.
    let services = state["services"].as_array().unwrap();
    assert_eq!(services.len(), 2);
    assert!(services.iter().all(|s| s["published"] == json!(false)));

    reconciler.apply(&off).await.unwrap();
    assert!(taken(&calls).is_empty());

    // Back on, it starts again.
    reconciler.apply(&settings()).await.unwrap();
    assert_eq!(taken(&calls)[0], "start");
}

/// A product with `MDNS=off` starts nothing until the setting turns it on.
#[tokio::test]
async fn the_product_default_holds_until_the_setting_is_set() {
    let (reconciler, calls) = reconciler(false);

    let state = reconciler.apply(&settings()).await.unwrap();
    assert!(taken(&calls).is_empty());
    assert_eq!(state["enabled"], json!(false));
    assert_eq!(state["source"], json!("product"));

    let mut on = settings();
    on.access.mdns.enabled = Some(true);
    let state = reconciler.apply(&on).await.unwrap();
    assert_eq!(taken(&calls)[0], "start");
    assert_eq!(state["source"], json!("setting"));
}

#[test]
fn the_silent_prefixes_cover_loopback_containers_wireguard_and_can() {
    for name in [
        "lo",
        "veth3a",
        "podman0",
        "cni-podman0",
        "docker0",
        "dummy0",
        "wg0",
        "can0",
        "vcan0",
    ] {
        assert!(
            SILENT_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix)),
            "{name}"
        );
    }
    for name in ["eth0", "end0", "wlan0", "br0", "eth0.20", "usb0"] {
        assert!(
            !SILENT_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix)),
            "{name}"
        );
    }
}

/// The drop-ins of the image and of the running system and the running
/// apps' entries are published after the built-in services, each with its
/// source; a broken file is refused by name and the rest goes on.
#[tokio::test]
async fn drop_ins_and_apps_are_published_with_their_source() {
    let rig = rig(true);
    std::fs::write(
        rig.image.path().join("grafana.json"),
        r#"{"version":1,"services":[{"instance":"Grafana","type":"_http._tcp","port":3000}]}"#,
    )
    .unwrap();
    std::fs::write(rig.runtime.path().join("broken.json"), "{").unwrap();
    *rig.apps.0.lock().unwrap() = vec![("printer".to_string(), vec![entry("_ipp._tcp", 631)])];

    let state = rig.reconciler.apply(&settings()).await.unwrap();

    assert_eq!(
        taken(&rig.calls),
        [
            "start",
            "publish mica-ab12._mica._tcp.local. on mica-ab12.local",
            "publish mica-ab12._http._tcp.local. on mica-ab12.local",
            "publish Grafana._http._tcp.local. on mica-ab12.local",
            "publish mica-ab12._ipp._tcp.local. on mica-ab12.local",
        ]
    );
    let sources: Vec<&str> = state["services"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["source"].as_str().unwrap())
        .collect();
    assert_eq!(
        sources,
        ["built-in", "built-in", "image:grafana", "app:printer"]
    );
    assert_eq!(state["refused"][0]["source"], json!("runtime:broken"));

    // The app stops and the file goes: both are withdrawn.
    rig.apps.0.lock().unwrap().clear();
    std::fs::remove_file(rig.image.path().join("grafana.json")).unwrap();
    rig.reconciler.apply(&settings()).await.unwrap();
    assert_eq!(
        taken(&rig.calls),
        [
            "withdraw Grafana._http._tcp.local.",
            "withdraw mica-ab12._ipp._tcp.local.",
        ]
    );
}

/// A name taken already, by the console or by an earlier file, is refused;
/// so is what goes past the limit.
#[tokio::test]
async fn a_taken_name_or_one_past_the_limit_is_refused() {
    let rig = rig(true);
    let many = |app: usize| -> Vec<_> {
        (0..entries::MAX_SERVICES)
            .map(|i| entries::Entry {
                instance: Some(format!("a{app}-s{i}")),
                ..entry("_x._tcp", 1)
            })
            .collect()
    };
    // The console's own name first, then 8 apps of 8: 2 past the limit.
    let apps: Vec<_> = [("console".to_string(), vec![entry("_http._tcp", 80)])]
        .into_iter()
        .chain((0..8).map(|app| (format!("a{app}"), many(app))))
        .collect();
    *rig.apps.0.lock().unwrap() = apps;

    let state = rig.reconciler.apply(&settings()).await.unwrap();

    assert_eq!(
        state["services"].as_array().unwrap().len(),
        entries::MAX_PUBLISHED
    );
    let refused: Vec<&str> = state["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["source"].as_str().unwrap())
        .collect();
    assert_eq!(refused, ["app:a7", "app:console"]);
}

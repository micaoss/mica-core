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

fn reconciler(default_on: bool) -> (MdnsReconciler, Calls) {
    let calls = Calls::default();
    let reconciler = MdnsReconciler::new(
        product(default_on),
        Box::new(StartRecorder(Arc::clone(&calls))),
    );
    (reconciler, calls)
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
    assert!(services.iter().all(|s| s.addresses.is_empty()));
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

#[test]
fn ssh_is_announced_while_it_is_on_where_it_listens() {
    let mut settings = settings();
    settings.access.ssh.enabled = true;
    settings.access.ssh.port = 2222;

    let services = built_in_services(&settings, &product(true));
    let ssh = services.iter().find(|s| s.kind == "_ssh._tcp").unwrap();
    assert_eq!(ssh.port, 2222);
    assert!(ssh.addresses.is_empty());

    settings.access.ssh.listen_addresses = vec![
        "192.0.2.5".to_string(),
        "2001:db8::5".to_string(),
        "[2001:db8::8]:2022".to_string(),
    ];
    let services = built_in_services(&settings, &product(true));
    let ssh = services.iter().find(|s| s.kind == "_ssh._tcp").unwrap();
    assert_eq!(
        ssh.addresses,
        [
            "192.0.2.5".parse::<IpAddr>().unwrap(),
            "2001:db8::5".parse().unwrap()
        ]
    );

    settings.access.ssh.listen_addresses = vec!["[2001:db8::8]:2022".to_string()];
    let services = built_in_services(&settings, &product(true));
    assert!(!kinds(&services).contains(&"_ssh._tcp"));

    settings.access.ssh.enabled = false;
    settings.access.ssh.listen_addresses.clear();
    let services = built_in_services(&settings, &product(true));
    assert!(!kinds(&services).contains(&"_ssh._tcp"));
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
    assert_eq!(reconciler.reads(), ["hostname", "access.web", "access.ssh"]);

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
            "publish mica-ab12._ssh._tcp.local. on mica-ab12.local",
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
    assert_eq!(state["services"], json!([]));

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

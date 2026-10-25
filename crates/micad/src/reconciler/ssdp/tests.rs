use std::sync::{Arc, Mutex};

use super::*;

/// What the recording responder was asked, in order.
type Calls = Arc<Mutex<Vec<String>>>;

struct Recorder(Calls);

impl Responder for Recorder {
    fn update(&mut self, announcement: &Announcement) -> Result<()> {
        self.0.lock().unwrap().push(format!(
            "update {} {}",
            announcement.hostname, announcement.http_port
        ));
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.0.lock().unwrap().push("byebye".to_string());
    }
}

struct StartRecorder(Calls);

impl StartResponder for StartRecorder {
    fn start(&self, announcement: &Announcement) -> Result<Box<dyn Responder>> {
        self.0.lock().unwrap().push(format!(
            "start {} {}",
            announcement.hostname, announcement.http_port
        ));
        Ok(Box::new(Recorder(Arc::clone(&self.0))))
    }
}

const MACHINE_ID: &str = "4f2e9c1a7b3d4e5f8a9b0c1d2e3f4a5b";

fn reconciler(default_on: bool, machine_id: Option<&str>) -> (SsdpReconciler, Calls) {
    let calls = Calls::default();
    let reconciler = SsdpReconciler::new(
        Product {
            name: Some("cx3576.full".to_string()),
            default_on,
        },
        machine_id.map(identity),
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

fn announcement() -> Announcement {
    Announcement {
        udn: identity(MACHINE_ID).udn,
        hostname: "mica-ab12".to_string(),
        http_port: 8080,
    }
}

#[test]
fn the_identity_is_stable_opaque_and_a_uuid() {
    let one = identity(MACHINE_ID);
    assert_eq!(one, identity(&format!("{MACHINE_ID}\n")));
    assert_ne!(one, identity("0123456789abcdef0123456789abcdef"));

    let uuid = one.udn.strip_prefix("uuid:").unwrap();
    let groups: Vec<usize> = uuid.split('-').map(str::len).collect();
    assert_eq!(groups, [8, 4, 4, 4, 12]);
    assert_eq!(&uuid[14..15], "8", "version 8");
    assert!("89ab".contains(&uuid[19..20]), "the RFC variant");
    assert_eq!(one.serial.len(), 16);
    assert!(!one.udn.contains(MACHINE_ID) && !MACHINE_ID.contains(&one.serial));
    assert!(!uuid.replace('-', "").contains(&one.serial));
}

#[test]
fn a_search_is_an_m_search_for_ssdp_discover() {
    let search = |text: &str| search_target(text.as_bytes());
    assert_eq!(
        search(
            "M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: ssdp:all\r\n\r\n"
        ),
        Some("ssdp:all".to_string())
    );
    assert_eq!(
        search("M-SEARCH * HTTP/1.1\r\nst: upnp:rootdevice\r\nman:\"ssdp:discover\"\r\n\r\n"),
        Some("upnp:rootdevice".to_string())
    );
    for not_one in [
        "NOTIFY * HTTP/1.1\r\nNT: upnp:rootdevice\r\nNTS: ssdp:alive\r\n\r\n",
        "M-SEARCH * HTTP/1.1\r\nST: ssdp:all\r\n\r\n",
        "M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:discover\"\r\n\r\n",
        "M-SEARCH * HTTP/1.1\r\nMAN: \"ssdp:other\"\r\nST: ssdp:all\r\n\r\n",
        "GET / HTTP/1.1\r\n\r\n",
    ] {
        assert_eq!(search(not_one), None, "{not_one}");
    }
    assert_eq!(search_target(&[0xff, 0xfe]), None);
}

/// `ssdp:all` is three answers; each other target the device has is one;
/// any other none.
#[test]
fn a_search_is_answered_for_the_devices_own_targets_only() {
    let announcement = announcement();
    let udn = announcement.udn.clone();
    let sts = |st: &str| -> Vec<String> {
        announcement
            .answers(st)
            .into_iter()
            .map(|(st, _)| st)
            .collect()
    };
    assert_eq!(
        sts("ssdp:all"),
        ["upnp:rootdevice", udn.as_str(), DEVICE_TYPE]
    );
    assert_eq!(sts("upnp:rootdevice"), ["upnp:rootdevice"]);
    assert_eq!(sts(DEVICE_TYPE), [DEVICE_TYPE]);
    assert_eq!(sts(&udn), [udn.as_str()]);
    assert!(sts("urn:schemas-upnp-org:device:MediaServer:1").is_empty());
    assert!(sts("uuid:00000000-0000-8000-8000-000000000000").is_empty());
}

#[test]
fn answers_and_announcements_are_small_and_name_the_description() {
    let announcement = announcement();
    let address = Ipv4Addr::new(192, 168, 1, 24);
    let usn = format!("{}::upnp:rootdevice", announcement.udn);

    let answer = response(
        &announcement,
        "upnp:rootdevice",
        &usn,
        address,
        7,
        "Sun, 25 Oct 2026 10:00:00 GMT",
    );
    assert!(answer.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(answer.contains("\r\nLOCATION: http://192.168.1.24:8080/ssdp/device.xml\r\n"));
    assert!(answer.contains(&format!("\r\nUSN: {usn}\r\n")));
    assert!(answer.contains("\r\nEXT:\r\n"));
    assert!(answer.contains("\r\nBOOTID.UPNP.ORG: 7\r\n"));
    assert!(answer.ends_with("\r\n\r\n"));
    assert!(answer.len() < 400, "{}", answer.len());

    let alive = notify(&announcement, DEVICE_TYPE, &usn, Some(address), 7);
    assert!(alive.contains("\r\nNTS: ssdp:alive\r\n"));
    assert!(alive.contains("\r\nLOCATION: http://192.168.1.24:8080/ssdp/device.xml\r\n"));
    assert!(alive.contains("\r\nHOST: 239.255.255.250:1900\r\n"));
    let byebye = notify(&announcement, DEVICE_TYPE, &usn, None, 7);
    assert!(byebye.contains("\r\nNTS: ssdp:byebye\r\n"));
    assert!(!byebye.contains("LOCATION"));
    assert!(alive.len() < 400 && byebye.len() < 400);
}

/// Only a searcher in a subnet of the interface the search came in on is
/// answered, from that subnet's address.
#[test]
fn only_a_searcher_on_the_link_is_answered() {
    let served = [
        Served {
            index: 2,
            address: Ipv4Addr::new(192, 168, 1, 24),
            netmask: Ipv4Addr::new(255, 255, 255, 0),
        },
        Served {
            index: 2,
            address: Ipv4Addr::new(10, 0, 0, 1),
            netmask: Ipv4Addr::new(255, 0, 0, 0),
        },
        Served {
            index: 3,
            address: Ipv4Addr::new(172, 16, 0, 1),
            netmask: Ipv4Addr::new(255, 255, 0, 0),
        },
    ];
    assert_eq!(
        on_link(&served, 2, Ipv4Addr::new(192, 168, 1, 77)),
        Some(Ipv4Addr::new(192, 168, 1, 24))
    );
    assert_eq!(
        on_link(&served, 2, Ipv4Addr::new(10, 9, 8, 7)),
        Some(Ipv4Addr::new(10, 0, 0, 1))
    );
    // Another interface's subnet, or none of them: not answered.
    assert_eq!(on_link(&served, 2, Ipv4Addr::new(172, 16, 5, 5)), None);
    assert_eq!(on_link(&served, 3, Ipv4Addr::new(192, 168, 1, 77)), None);
    assert_eq!(on_link(&served, 2, Ipv4Addr::new(8, 8, 8, 8)), None);
    assert_eq!(on_link(&served, 9, Ipv4Addr::new(192, 168, 1, 77)), None);
}

#[test]
fn answers_are_limited_per_searcher_and_in_all() {
    let mut limit = RateLimit::default();
    let now = Instant::now();
    let one = Ipv4Addr::new(192, 168, 1, 2);
    assert!(limit.allow(one, now));
    assert!(limit.allow(one, now));
    assert!(
        !limit.allow(one, now),
        "two searches a second of one searcher"
    );

    for i in 0..18 {
        assert!(limit.allow(Ipv4Addr::new(192, 168, 2, i), now));
    }
    assert!(
        !limit.allow(Ipv4Addr::new(192, 168, 3, 1), now),
        "twenty searches a second in all"
    );

    let later = now + Duration::from_secs(1);
    assert!(limit.allow(one, later), "a new second");
}

#[tokio::test]
async fn on_starts_the_responder_and_reports_the_description() {
    let (reconciler, calls) = reconciler(true, Some(MACHINE_ID));

    let state = reconciler.apply(&settings()).await.unwrap();

    assert_eq!(taken(&calls), ["start mica-ab12 8080"]);
    assert_eq!(state["enabled"], json!(true));
    assert_eq!(state["active"], json!(true));
    assert_eq!(state["source"], json!("product"));
    assert_eq!(state["udn"], json!(identity(MACHINE_ID).udn));
    assert_eq!(state["serialNumber"], json!(identity(MACHINE_ID).serial));
    assert_eq!(state["friendlyName"], json!("mica-ab12"));
    assert_eq!(state["modelName"], json!("cx3576.full"));
    assert_eq!(state["manufacturer"], json!("Mica"));
    assert_eq!(state["console"], json!({ "scheme": "http", "port": 8080 }));
    assert_eq!(reconciler.name(), "ssdp");
    assert_eq!(reconciler.subtree(), "access.ssdp");
    assert_eq!(reconciler.reads(), ["hostname", "access.web"]);

    reconciler.apply(&settings()).await.unwrap();
    assert!(
        taken(&calls).is_empty(),
        "the same settings again change nothing"
    );
}

#[tokio::test]
async fn a_new_hostname_or_port_re_announces_and_https_moves_the_console() {
    let (reconciler, calls) = reconciler(true, Some(MACHINE_ID));
    reconciler.apply(&settings()).await.unwrap();
    taken(&calls);

    let mut next = settings();
    next.hostname = "bridge".to_string();
    next.access.web.https_enabled = true;
    next.access.web.https_port = 9443;
    let state = reconciler.apply(&next).await.unwrap();

    assert_eq!(taken(&calls), ["update bridge 8080"]);
    assert_eq!(state["console"], json!({ "scheme": "https", "port": 9443 }));
}

/// Off, or the console off the network, is no socket: the responder is
/// dropped, saying byebye.
#[tokio::test]
async fn off_or_no_console_on_the_network_stops_the_responder() {
    let (reconciler, calls) = reconciler(true, Some(MACHINE_ID));
    reconciler.apply(&settings()).await.unwrap();
    taken(&calls);

    let mut local = settings();
    local.access.web.mode = WebMode::Local;
    let state = reconciler.apply(&local).await.unwrap();
    assert_eq!(taken(&calls), ["byebye"]);
    assert_eq!(state["enabled"], json!(true));
    assert_eq!(state["active"], json!(false));
    assert!(state["reason"].is_string());
    assert!(state.get("udn").is_none());

    reconciler.apply(&settings()).await.unwrap();
    taken(&calls);
    let mut off = settings();
    off.access.ssdp.enabled = Some(false);
    let state = reconciler.apply(&off).await.unwrap();
    assert_eq!(taken(&calls), ["byebye"]);
    assert_eq!(state["source"], json!("setting"));
    assert_eq!(state["active"], json!(false));
}

#[tokio::test]
async fn the_product_default_and_a_missing_machine_id_hold_it_off() {
    let (held, calls) = reconciler(false, Some(MACHINE_ID));
    let state = held.apply(&settings()).await.unwrap();
    assert!(taken(&calls).is_empty());
    assert_eq!(state["enabled"], json!(false));

    let mut on = settings();
    on.access.ssdp.enabled = Some(true);
    held.apply(&on).await.unwrap();
    assert_eq!(taken(&calls), ["start mica-ab12 8080"]);

    let (nameless, calls) = reconciler(true, None);
    let state = nameless.apply(&settings()).await.unwrap();
    assert!(taken(&calls).is_empty());
    assert_eq!(state["active"], json!(false));
    assert!(state["reason"].is_string());
}

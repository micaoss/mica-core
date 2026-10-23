//! The apply transaction: waiting for changed interfaces, and putting the
//! previous files back when they do not come up.

use super::settle::{Verdict, verdict};
use super::*;

/// A [`LinkSettle`] that records what it was asked to wait for and answers
/// `answer`.
struct ScriptedSettle {
    asked: Asked,
    answer: Result<(), String>,
}

#[async_trait::async_trait]
impl LinkSettle for ScriptedSettle {
    async fn settle(&self, links: &BTreeMap<String, bool>) -> Result<(), String> {
        self.asked.lock().unwrap().push(links.clone());
        self.answer.clone()
    }
}

/// What a [`ScriptedSettle`] was asked, one entry per wait.
type Asked = Arc<Mutex<Vec<BTreeMap<String, bool>>>>;

fn settling_in(
    dir: &std::path::Path,
    answer: Result<(), String>,
) -> (
    NetworkReconciler<MockReload, MockLink>,
    Arc<Mutex<Vec<String>>>,
    Asked,
) {
    let (reconciler, calls) = reconciler_in(dir);
    let asked = Arc::new(Mutex::new(Vec::new()));
    let reconciler = reconciler.with_settle(Arc::new(ScriptedSettle {
        asked: Arc::clone(&asked),
        answer,
    }));
    (reconciler, calls, asked)
}

fn read(dir: &std::path::Path, file_name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(file_name)).ok()
}

#[tokio::test]
pub(super) async fn the_first_apply_waits_for_nothing_and_later_ones_for_what_changed() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls, asked) = settling_in(dir.path(), Ok(()));
    let base = settings_with(&[("eth0", dhcp_iface())]);
    reconciler.apply(&base).await.unwrap();
    assert!(asked.lock().unwrap().is_empty());

    reconciler.apply(&base).await.unwrap();
    assert!(asked.lock().unwrap().is_empty(), "nothing changed");

    reconciler
        .apply(&settings_with(&[
            ("eth0", dhcp_iface()),
            ("eth0.100", vlan_iface("eth0", 100)),
        ]))
        .await
        .unwrap();
    // The parent's unit names the new VLAN, so both changed; only the VLAN
    // is a device the apply creates.
    assert_eq!(
        *asked.lock().unwrap(),
        [BTreeMap::from([
            ("eth0".to_string(), false),
            ("eth0.100".to_string(), true),
        ])]
    );
}

#[tokio::test]
pub(super) async fn an_apply_that_does_not_come_up_puts_the_previous_files_back() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls, _asked) =
        settling_in(dir.path(), Err("eth0.100: it is off".to_string()));
    reconciler
        .apply(&settings_with(&[("eth0", dhcp_iface())]))
        .await
        .unwrap();
    calls.lock().unwrap().clear();

    let error = reconciler
        .apply(&settings_with(&[
            ("eth0", dhcp_iface()),
            ("eth0.100", vlan_iface("eth0", 100)),
        ]))
        .await
        .unwrap_err();

    assert!(error.downcast_ref::<RolledBack>().is_some(), "{error:#}");
    assert_eq!(
        error.to_string(),
        "eth0.100: it is off; the previous network configuration was restored"
    );
    assert_eq!(
        read(dir.path(), "50-mica-eth0.network").as_deref(),
        Some(GOLDEN_DHCP)
    );
    assert_eq!(read(dir.path(), "50-mica-eth0.100.network"), None);
    assert_eq!(read(dir.path(), "50-mica-eth0.100.netdev"), None);
    // The VLAN networkd built is deleted, and networkd reloads the old files.
    assert_eq!(*calls.lock().unwrap(), ["reload", "del eth0.100", "reload"]);
    // Nothing is left half-written beside the units.
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(names, ["50-mica-eth0.network"]);
}

fn describe(link: serde_json::Value) -> serde_json::Value {
    json!({ "Interfaces": [link] })
}

#[test]
fn a_link_is_up_when_networkd_configured_it_or_it_has_an_address() {
    let configured = describe(json!({
        "Name": "eth0", "AdministrativeState": "configured", "OperationalState": "carrier",
    }));
    assert_eq!(verdict(&configured, "eth0", false), Verdict::Settled);
    let degraded = describe(json!({
        "Name": "eth0", "AdministrativeState": "configuring",
        "CarrierState": "carrier", "OperationalState": "degraded",
    }));
    assert_eq!(verdict(&degraded, "eth0", false), Verdict::Settled);
    let port = describe(json!({
        "Name": "eth1", "AdministrativeState": "configuring",
        "CarrierState": "carrier", "OperationalState": "enslaved",
    }));
    assert_eq!(verdict(&port, "eth1", false), Verdict::Settled);
}

#[test]
fn a_link_without_carrier_or_device_is_not_waited_for_unless_the_apply_creates_it() {
    let unplugged = describe(json!({
        "Name": "eth0", "AdministrativeState": "configuring",
        "CarrierState": "no-carrier", "OperationalState": "no-carrier",
    }));
    assert_eq!(verdict(&unplugged, "eth0", false), Verdict::Settled);
    assert_eq!(verdict(&unplugged, "eth9", false), Verdict::Settled);
    assert_eq!(
        verdict(&unplugged, "eth0.100", true),
        Verdict::Pending("the device was not created".to_string())
    );
}

#[test]
fn a_link_networkd_gave_up_on_fails_and_one_coming_up_is_pending() {
    let failed = describe(json!({ "Name": "eth0", "AdministrativeState": "failed" }));
    assert_eq!(
        verdict(&failed, "eth0", false),
        Verdict::Failed("networkd could not configure it".to_string())
    );
    let configuring = describe(json!({
        "Name": "eth0", "AdministrativeState": "configuring",
        "CarrierState": "carrier", "OperationalState": "carrier",
    }));
    assert_eq!(
        verdict(&configuring, "eth0", false),
        Verdict::Pending("it is carrier".to_string())
    );
}

/// A rollback puts back a netdev that names the deleted tunnel's key file, so
/// the key goes only once the apply converged.
#[tokio::test]
pub(super) async fn a_deleted_tunnel_keeps_its_key_until_the_apply_converged() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("networkd-secrets/wg-wg0.key");
    let tunnel = || ("wg0", wireguard_iface(Some(51820), vec![peer(1)]));
    let (reconciler, _calls, _asked) =
        settling_in(dir.path(), Err("eth0.100: it is off".to_string()));
    reconciler
        .apply(&settings_with(&[("eth0", dhcp_iface()), tunnel()]))
        .await
        .unwrap();
    let private_key = std::fs::read_to_string(&key).unwrap();

    reconciler
        .apply(&settings_with(&[
            ("eth0", dhcp_iface()),
            ("eth0.100", vlan_iface("eth0", 100)),
        ]))
        .await
        .unwrap_err();
    assert_eq!(std::fs::read_to_string(&key).unwrap(), private_key);
    assert!(read(dir.path(), "50-mica-wg0.netdev").is_some());

    let (reconciler, _calls, _asked) = settling_in(dir.path(), Ok(()));
    reconciler
        .apply(&settings_with(&[("eth0", dhcp_iface())]))
        .await
        .unwrap();
    assert!(!key.exists());
}

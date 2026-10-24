//! VLAN and bridge devices.

use super::*;

#[tokio::test]
pub(super) async fn renders_a_vlan_netdev_and_names_the_child_in_its_parent() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[
        ("eth0", dhcp_iface()),
        ("eth0.100", vlan_iface("eth0", 100)),
    ]);

    reconciler.apply(&settings).await.unwrap();

    let netdev = std::fs::read_to_string(dir.path().join("50-mica-eth0.100.netdev")).unwrap();
    let parent = std::fs::read_to_string(dir.path().join("50-mica-eth0.network")).unwrap();
    let child = std::fs::read_to_string(dir.path().join("50-mica-eth0.100.network")).unwrap();
    assert_eq!(netdev, GOLDEN_VLAN_NETDEV);
    // networkd creates the VLAN only because the parent's unit names it.
    assert_eq!(parent, GOLDEN_VLAN_PARENT);
    assert_eq!(child, GOLDEN_VLAN_CHILD);
    // A device that did not exist before is not deleted on the way in.
    assert_eq!(*calls.lock().unwrap(), vec!["reload".to_string()]);
}

#[tokio::test]
pub(super) async fn renders_a_bridge_netdev_and_gives_its_port_only_the_master() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    let settings = settings_with(&[("br0", bridge_iface(&["eth1"])), ("eth1", port_iface())]);

    reconciler.apply(&settings).await.unwrap();

    let netdev = std::fs::read_to_string(dir.path().join("50-mica-br0.netdev")).unwrap();
    let port = std::fs::read_to_string(dir.path().join("50-mica-eth1.network")).unwrap();
    let bridge = std::fs::read_to_string(dir.path().join("50-mica-br0.network")).unwrap();
    assert_eq!(netdev, GOLDEN_BRIDGE_NETDEV);
    assert_eq!(port, GOLDEN_BRIDGE_PORT);
    // The bridge itself carries the addressing its ports gave up.
    assert_eq!(
        bridge,
        "[Match]\nName=br0\n\n[Network]\nDHCP=yes\nLinkLocalAddressing=yes\n\n[DHCPv4]\nRouteMetric=100\n"
    );
    // A bridge has no netdev-less port file left behind and no VLAN line.
    assert!(!dir.path().join("50-mica-eth1.netdev").exists());
}

#[tokio::test]
pub(super) async fn deletes_the_kernel_device_of_a_removed_vlan() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let with_vlan = settings_with(&[
        ("eth0", dhcp_iface()),
        ("eth0.100", vlan_iface("eth0", 100)),
    ]);
    reconciler.apply(&with_vlan).await.unwrap();

    reconciler
        .apply(&settings_with(&[("eth0", dhcp_iface())]))
        .await
        .unwrap();

    assert!(!dir.path().join("50-mica-eth0.100.netdev").exists());
    assert!(!dir.path().join("50-mica-eth0.100.network").exists());
    // The device is deleted BEFORE the reload, and the swept `.network`
    // asks for no deletion of its own: networkd would otherwise leave the
    // VLAN passing traffic until the next boot.
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            "reload".to_string(),
            "del eth0.100".to_string(),
            "reload".to_string(),
        ]
    );
}

#[tokio::test]
pub(super) async fn recreates_a_vlan_whose_id_changed() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    reconciler
        .apply(&settings_with(&[
            ("eth0", dhcp_iface()),
            ("eth0.100", vlan_iface("eth0", 100)),
        ]))
        .await
        .unwrap();

    reconciler
        .apply(&settings_with(&[
            ("eth0", dhcp_iface()),
            ("eth0.100", vlan_iface("eth0", 200)),
        ]))
        .await
        .unwrap();

    // Netdev properties are applied when the device is created, so a
    // rewritten file alone would leave the old id in the kernel.
    let netdev = std::fs::read_to_string(dir.path().join("50-mica-eth0.100.netdev")).unwrap();
    assert!(netdev.contains("Id=200"), "{netdev}");
    assert_eq!(
        *calls.lock().unwrap(),
        vec![
            "reload".to_string(),
            "del eth0.100".to_string(),
            "reload".to_string(),
        ]
    );
}

#[tokio::test]
pub(super) async fn reapplying_an_unchanged_vlan_deletes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, calls) = reconciler_in(dir.path());
    let settings = settings_with(&[
        ("eth0", dhcp_iface()),
        ("eth0.100", vlan_iface("eth0", 100)),
    ]);
    reconciler.apply(&settings).await.unwrap();

    reconciler.apply(&settings).await.unwrap();

    // A convergent reconcile that tore its own VLAN down every pass would
    // drop the link on every settings write in the tree.
    assert_eq!(
        *calls.lock().unwrap(),
        vec!["reload".to_string(), "reload".to_string()]
    );
}

#[tokio::test]
pub(super) async fn sweeps_a_stale_netdev_but_spares_a_foreign_one() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _calls) = reconciler_in(dir.path());
    std::fs::write(dir.path().join("50-mica-br9.netdev"), "stale").unwrap();
    std::fs::write(dir.path().join("70-vpn.netdev"), "foreign").unwrap();

    reconciler
        .apply(&settings_with(&[("eth0", dhcp_iface())]))
        .await
        .unwrap();

    assert!(!dir.path().join("50-mica-br9.netdev").exists());
    assert!(dir.path().join("70-vpn.netdev").exists());
}

/// A kernel claiming fixed features.
struct Claims(Option<Vec<String>>);

#[async_trait::async_trait]
impl KernelFeatures for Claims {
    async fn features(&self) -> Option<Vec<String>> {
        self.0.clone()
    }
}

fn claiming(dir: &std::path::Path, features: Option<&[&str]>) -> impl Reconciler {
    let (reconciler, _calls) = reconciler_in(dir);
    reconciler.with_kernel(Arc::new(Claims(
        features.map(|words| words.iter().map(|word| (*word).to_string()).collect()),
    )))
}

/// A VLAN or a bridge needs `netdev`, a tunnel `wireguard`, from a kernel that
/// claims features; nothing is written for a refused tree. A kernel that
/// claims none is held to none.
#[tokio::test]
pub(super) async fn virtual_devices_are_held_to_the_kernel_s_features() {
    let vlan = settings_with(&[
        ("eth0", dhcp_iface()),
        ("eth0.100", vlan_iface("eth0", 100)),
    ]);
    let bridge = settings_with(&[("br0", bridge_iface(&["eth1"])), ("eth1", port_iface())]);
    let tunnel = settings_with(&[("wg0", wireguard_iface(None, vec![peer(1)]))]);
    for (settings, refusal) in [
        (&vlan, "a vlan device needs the kernel feature netdev"),
        (&bridge, "a bridge device needs the kernel feature netdev"),
        (
            &tunnel,
            "a wireguard device needs the kernel feature wireguard",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let error = claiming(dir.path(), Some(&["containers"]))
            .apply(settings)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(refusal), "{error}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    for (settings, features) in [
        (&vlan, Some(&["netdev"][..])),
        (&bridge, Some(&["netdev"][..])),
        (&tunnel, Some(&["wireguard"][..])),
        (&vlan, None),
        (&tunnel, None),
    ] {
        let dir = tempfile::tempdir().unwrap();
        claiming(dir.path(), features)
            .apply(settings)
            .await
            .unwrap();
    }
    let dir = tempfile::tempdir().unwrap();
    claiming(dir.path(), Some(&[]))
        .apply(&settings_with(&[("eth0", dhcp_iface())]))
        .await
        .unwrap();
}

/// A `mica-deploy` answering `status` from a list, counting the calls.
struct StatusClient(Mutex<Vec<anyhow::Result<String>>>, Arc<Mutex<usize>>);

#[async_trait::async_trait]
impl crate::update_lifecycle::UpdateClient for StatusClient {
    fn unavailable(&self) -> Option<String> {
        None
    }

    async fn run(
        &self,
        args: &[String],
        _timeout: std::time::Duration,
    ) -> anyhow::Result<crate::update_lifecycle::ClientOutput> {
        assert_eq!(args, ["status"]);
        *self.1.lock().unwrap() += 1;
        let stdout = self.0.lock().unwrap().remove(0)?;
        Ok(crate::update_lifecycle::ClientOutput {
            code: Some(0),
            signal: None,
            stdout,
            stderr: String::new(),
        })
    }
}

/// The booted deployment's kernel features, read once; a status that cannot
/// be read claims nothing and is asked again.
#[tokio::test]
pub(super) async fn the_deployed_kernel_s_features_are_the_booted_deployment_s() {
    let mut status = crate::deployment::tests::fixture();
    status["deployments"][0]["kernelFeatures"] = serde_json::json!(["netdev"]);
    let calls = Arc::new(Mutex::new(0));
    let kernel = DeployedKernel::new(Arc::new(StatusClient(
        Mutex::new(vec![
            Err(anyhow::anyhow!("no client")),
            Ok(status.to_string()),
        ]),
        Arc::clone(&calls),
    )));
    assert_eq!(kernel.features().await, None);
    assert_eq!(kernel.features().await, Some(vec!["netdev".to_string()]));
    assert_eq!(kernel.features().await, Some(vec!["netdev".to_string()]));
    assert_eq!(*calls.lock().unwrap(), 2);

    let absent = DeployedKernel::new(Arc::new(StatusClient(
        Mutex::new(vec![Ok(crate::deployment::tests::fixture().to_string())]),
        Arc::new(Mutex::new(0)),
    )));
    assert_eq!(absent.features().await, None);
    assert_eq!(absent.features().await, None);
}

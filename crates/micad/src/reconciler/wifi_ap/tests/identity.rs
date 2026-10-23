//! The SSID and key derived from the device's identity.

use super::*;

#[test]
pub(super) fn the_derived_ssid_is_stable_and_distinct_per_device() {
    assert_eq!(derived_ssid(DEVICE_ID), derived_ssid(DEVICE_ID));
    assert_eq!(derived_ssid(DEVICE_ID), "mica-1a2b3c4d");
    assert_ne!(
        derived_ssid(DEVICE_ID),
        derived_ssid("ffffffffffffffffffffffffffffffff"),
        "two devices would advertise the same SSID"
    );
    assert!(
        validate_ssid(&derived_ssid(DEVICE_ID)).is_ok(),
        "a derived SSID must itself be a legal SSID"
    );
}

#[tokio::test]
pub(super) async fn an_absent_ssid_is_derived_from_the_device_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let ap = WifiApSettings {
        ssid: None,
        ..lab_ap()
    };

    let state = reconciler.apply(&settings_with(ap)).await.unwrap();

    assert_eq!(state["ssid"], json!("mica-1a2b3c4d"));
    assert_eq!(state["ssidSource"], json!("derived"));
    assert!(
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains("ssid=mica-1a2b3c4d\n")
    );
}

#[tokio::test]
pub(super) async fn a_configured_ssid_is_used_verbatim_and_never_derived() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");

    let state = reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    assert_eq!(state["ssid"], json!("mica-lab"));
    assert_eq!(state["ssidSource"], json!("settings"));
    let rendered = std::fs::read_to_string(&paths.config).unwrap();
    assert!(rendered.contains("ssid=mica-lab\n"), "{rendered}");
    assert!(
        !rendered.contains("mica-1a2b3c4d"),
        "the configured SSID was overridden by the derived one: {rendered}"
    );
}

#[tokio::test]
pub(super) async fn a_device_with_no_identity_and_no_ssid_refuses_to_start_the_radio() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let mut settings = settings_with(WifiApSettings {
        ssid: None,
        ..lab_ap()
    });
    settings.provisioning.device_id = None;

    let err = reconciler.apply(&settings).await.unwrap_err();

    assert!(format!("{err:#}").contains("wifi.ap.ssid"), "{err:#}");
    assert!(!paths.config.exists());
    assert!(reconciler.control.calls().is_empty());
}

// ---- where the key comes from -----------------------------------------

#[tokio::test]
pub(super) async fn an_absent_key_comes_from_the_per_device_state_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    seed_ap_psk(&paths.state_dir, SECRET_PSK);
    let ap = WifiApSettings {
        psk: None,
        ..lab_ap()
    };

    reconciler.apply(&settings_with(ap)).await.unwrap();

    assert!(
        std::fs::read_to_string(&paths.config)
            .unwrap()
            .contains(&format!("wpa_passphrase={SECRET_PSK}\n")),
        "the per-device key on STATE must be what the access point uses"
    );
}

#[tokio::test]
pub(super) async fn a_configured_key_wins_over_the_state_secret() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    seed_ap_psk(&paths.state_dir, SECRET_PSK);

    reconciler.apply(&settings_with(lab_ap())).await.unwrap();

    let rendered = std::fs::read_to_string(&paths.config).unwrap();
    assert!(
        rendered.contains("wpa_passphrase=labsecret1\n"),
        "{rendered}"
    );
    assert!(
        !rendered.contains(SECRET_PSK),
        "the settings key was ignored in favour of the state secret: {rendered}"
    );
}

#[tokio::test]
pub(super) async fn a_device_with_no_key_anywhere_refuses_rather_than_using_a_constant() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let ap = WifiApSettings {
        psk: None,
        ..lab_ap()
    };

    let err = reconciler.apply(&settings_with(ap)).await.unwrap_err();

    let chain = format!("{err:#}");
    assert!(chain.contains("fleet-wide"), "{chain}");
    assert!(
        !paths.config.exists(),
        "a configuration was written without a key"
    );
    assert!(reconciler.control.calls().is_empty());
}

// ---- address and DHCP pool --------------------------------------------

// ---- the device code ----------------------------------------------------

/// A device root whose eMMC carries the first documented vector's CID, and
/// whose product secret is `0x00..=0x1f`.
fn device_root(dir: &Path) -> PathBuf {
    let root = dir.join("root");
    let mmc = root.join("sys/block/mmcblk0/device");
    std::fs::create_dir_all(&mmc).unwrap();
    std::fs::write(mmc.join("type"), "MMC\n").unwrap();
    std::fs::write(mmc.join("cid"), "150100444a54443452a1b2c3d4e5f600\n").unwrap();
    let conf = root.join(device_code::CONFIG_PATH);
    std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
    std::fs::write(&conf, "source=emmc-cid\n").unwrap();
    let secret = root.join(device_code::SECRET_PATH);
    std::fs::write(&secret, (0u8..32).collect::<Vec<_>>()).unwrap();
    std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o600)).unwrap();
    root
}

fn unset_ap() -> WifiApSettings {
    WifiApSettings {
        ssid: None,
        psk: None,
        ..lab_ap()
    }
}

#[tokio::test]
pub(super) async fn an_unclaimed_device_with_a_code_uses_the_labelled_ssid_and_key() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let reconciler = reconciler.with_device_root(device_root(dir.path()));
    seed_ap_psk(&paths.state_dir, SECRET_PSK);

    let state = reconciler.apply(&settings_with(unset_ap())).await.unwrap();

    assert_eq!(state["ssid"], json!("mica-789Z"));
    assert_eq!(state["pskSource"], json!("derived"));
    let rendered = std::fs::read_to_string(&paths.config).unwrap();
    assert!(rendered.contains("ssid=mica-789Z\n"), "{rendered}");
    assert!(
        rendered.contains("wpa_passphrase=U8A5TU7RT6A6E8TV\n"),
        "{rendered}"
    );
}

#[tokio::test]
pub(super) async fn a_claimed_device_keeps_the_derived_key_until_a_key_is_set() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let reconciler = reconciler.with_device_root(device_root(dir.path()));
    seed_ap_psk(&paths.state_dir, SECRET_PSK);
    let mut settings = settings_with(unset_ap());
    settings.access.web_admin = Some(micad_settings::WebAdminSettings {
        password_hash: "$argon2id$x".to_string(),
    });

    let state = reconciler.apply(&settings).await.unwrap();

    assert_eq!(
        state["ssid"],
        json!("mica-789Z"),
        "the SSID stays the label's"
    );
    assert_eq!(
        state["pskSource"],
        json!("derived"),
        "claiming changes nothing about the key"
    );
    let rendered = std::fs::read_to_string(&paths.config).unwrap();
    assert!(
        rendered.contains("wpa_passphrase=U8A5TU7RT6A6E8TV\n"),
        "{rendered}"
    );
}

#[tokio::test]
pub(super) async fn a_set_key_wins_over_the_derived_one() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, _paths) = fixture(dir.path(), "inactive", "disabled");
    let reconciler = reconciler.with_device_root(device_root(dir.path()));

    let state = reconciler
        .apply(&settings_with(WifiApSettings {
            ssid: None,
            ..lab_ap()
        }))
        .await
        .unwrap();

    assert_eq!(state["pskSource"], json!("settings"));
}

#[tokio::test]
pub(super) async fn a_secret_others_can_read_derives_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (reconciler, paths) = fixture(dir.path(), "inactive", "disabled");
    let root = device_root(dir.path());
    std::fs::set_permissions(
        root.join(device_code::SECRET_PATH),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let reconciler = reconciler.with_device_root(root);
    seed_ap_psk(&paths.state_dir, SECRET_PSK);

    let state = reconciler.apply(&settings_with(unset_ap())).await.unwrap();

    assert_eq!(state["ssid"], json!("mica-789Z"));
    assert_eq!(state["pskSource"], json!("generated"));
}

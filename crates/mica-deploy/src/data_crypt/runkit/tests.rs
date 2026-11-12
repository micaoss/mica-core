use std::fs::OpenOptions;
use std::os::unix::fs::MetadataExt;

use super::*;

fn encryption(json: &str) -> Result<Encryption> {
    Ok(serde_json::from_str(json)?)
}

#[test]
fn the_boot_policy_field_is_the_contracts_spelling() {
    let parsed = encryption(
        r#"{"mode":"on","keyProviders":["tpm2"],"noProvider":"refuse","cipher":"aes-xts-plain64"}"#,
    )
    .unwrap();
    assert_eq!(parsed.key_providers, [ProviderKind::Tpm2]);
    assert_eq!(parsed.no_provider, NoProvider::Refuse);
    assert_eq!(parsed.validate().unwrap(), Cipher::AesXts);
    let adiantum = encryption(
        r#"{"mode":"on","keyProviders":["tpm2"],"noProvider":"plain","cipher":"xchacha12,aes-adiantum-plain64"}"#,
    )
    .unwrap();
    assert_eq!(adiantum.validate().unwrap(), Cipher::Adiantum);
    assert_eq!(adiantum.no_provider, NoProvider::Plain);
}

#[test]
fn a_boot_policy_field_outside_the_contract_is_refused() {
    for json in [
        r#"{"mode":"off","keyProviders":["tpm2"],"noProvider":"refuse","cipher":"aes-xts-plain64"}"#,
        r#"{"mode":"on","keyProviders":["tee"],"noProvider":"refuse","cipher":"aes-xts-plain64"}"#,
        r#"{"mode":"on","keyProviders":["tpm2"],"noProvider":"maybe","cipher":"aes-xts-plain64"}"#,
        r#"{"mode":"on","keyProviders":["tpm2"],"cipher":"aes-xts-plain64"}"#,
        r#"{"mode":"on","keyProviders":["tpm2"],"noProvider":"refuse","cipher":"aes-xts-plain64","extra":1}"#,
    ] {
        assert!(encryption(json).is_err(), "{json}");
    }
    for json in [
        r#"{"mode":"on","keyProviders":[],"noProvider":"refuse","cipher":"aes-xts-plain64"}"#,
        r#"{"mode":"on","keyProviders":["tpm2","tpm2"],"noProvider":"refuse","cipher":"aes-xts-plain64"}"#,
        r#"{"mode":"on","keyProviders":["tpm2"],"noProvider":"refuse","cipher":"aes-cbc-essiv:sha256"}"#,
    ] {
        assert!(encryption(json).unwrap().validate().is_err(), "{json}");
    }
}

fn disk() -> (tempfile::TempDir, File) {
    let dir = tempfile::tempdir().unwrap();
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(dir.path().join("data.img"))
        .unwrap();
    file.set_len(20 * 1024 * 1024).unwrap();
    (dir, file)
}

#[test]
fn only_a_disk_with_neither_header_nor_ext4_is_fresh() {
    let (_dir, device) = disk();
    assert!(fresh(&device).unwrap(), "zeros");
    device.write_all_at(&[0x53, 0xEF], 1080).unwrap();
    assert!(!fresh(&device).unwrap(), "an ext4 superblock");
    device.write_all_at(&[0, 0], 1080).unwrap();
    device.write_all_at(b"SKUL\xba\xbe", 16384).unwrap();
    assert!(!fresh(&device).unwrap(), "a LUKS2 secondary header");
    let (_dir, empty) = disk();
    empty.set_len(0).unwrap();
    assert!(fresh(&empty).unwrap());
}

#[test]
fn the_crypt_table_maps_whole_sectors_past_the_header_by_keyring_reference() {
    let bytes = DATA_OFFSET + 10 * 4096 + 512;
    let target = table(Cipher::AesXts, 259, 3, bytes).unwrap();
    assert_eq!(target.kind, "crypt");
    assert_eq!(target.sector, 0);
    assert_eq!(target.length, 80, "10 whole 4096-byte sectors");
    assert_eq!(
        target.parameters,
        "aes-xts-plain64 :64:logon:mica:data 0 259:3 32768 2 allow_discards sector_size:4096"
    );
    let adiantum = table(Cipher::Adiantum, 8, 3, bytes).unwrap();
    assert!(
        adiantum
            .parameters
            .starts_with("capi:adiantum(xchacha12,aes)-plain64 :32:logon:mica:data 0 8:3 ")
    );
    assert!(table(Cipher::AesXts, 8, 3, DATA_OFFSET).is_err());
}

#[test]
fn the_report_carries_a_recovery_passphrase_only_when_one_was_made() {
    let report = Report {
        state: State::Locked,
        reason: Some("unseal".into()),
        device: None,
        formatted: false,
        cipher: None,
        providers: vec![ProviderReport {
            kind: "tpm2".into(),
            enrolled: false,
            unlocked_this_boot: false,
            failure: Some("TPM rc 0x99d (TPM_RC_POLICY_FAIL)".into()),
        }],
        recovery: None,
        seal_off_failures: Vec::new(),
    };
    let text = serde_json::to_string(&report).unwrap();
    assert!(!text.contains("recovery"), "{text}");
    assert!(text.contains(r#""unlockedThisBoot":false"#), "{text}");
    assert_eq!(serde_json::from_str::<Report>(&text).unwrap(), report);
}

/// Remove the live [`MAPPING`], as the exitrd does.
fn unmap() {
    let control = crate::boot::startup::verity::control().unwrap();
    let rdev = fs::metadata(MAPPED).unwrap().rdev();
    let status = lifecycle_sys::dm_status_writable(&control, rdev).unwrap();
    lifecycle_sys::dm_remove(&control, &status).unwrap();
    fs::remove_file(MAPPED).unwrap();
}

fn mount_data() {
    rustix::mount::mount(
        MAPPED,
        "/mnt",
        "ext4",
        rustix::mount::MountFlags::empty(),
        c"",
    )
    .unwrap();
}

fn unmount_data() {
    rustix::mount::unmount("/mnt", rustix::mount::UnmountFlags::empty()).unwrap();
}

const ON: &str =
    r#"{"mode":"on","keyProviders":["tpm2"],"noProvider":"refuse","cipher":"aes-xts-plain64"}"#;
const ADIANTUM: &str = r#"{"mode":"on","keyProviders":["tpm2"],"noProvider":"plain","cipher":"xchacha12,aes-adiantum-plain64"}"#;

/// One boot of the W1 VM check (a uefi-x64 kernel, swtpm as its TPM,
/// `/dev/vdb` the DATA under test, `/dev/vdc` a second fresh disk, `/dev/vdd`
/// scratch for the recovery passphrase), the step in `MICA_VM_STEP`. Run by
/// the step's VM driver, not in CI.
#[test]
#[ignore = "runs as the init of a VM with a TPM"]
fn vm_step() {
    let step = std::env::var("MICA_VM_STEP").expect("MICA_VM_STEP");
    let format = DataFormat::default_for(true);
    let on = encryption(ON).unwrap();
    let open = |disk: &str, encryption: Option<&Encryption>| {
        open_data(Path::new("/"), "amd64", disk, encryption, &format).unwrap()
    };
    match step.as_str() {
        "format" => {
            let report = open("/dev/vdb1", Some(&on));
            assert_eq!(report.state, State::On);
            assert!(report.formatted);
            assert_eq!(report.device.as_deref(), Some(MAPPED));
            assert!(report.providers[0].enrolled, "{report:?}");
            assert!(report.seal_off_failures.is_empty(), "{report:?}");
            let recovery = report.recovery.clone().unwrap();
            mount_data();
            fs::write("/mnt/proof", "mica w1 step 1").unwrap();
            unmount_data();
            // The exitrd sees the mapping as MICA's and releases it with its
            // own worker.
            let live = crate::boot::shutdown::scan().unwrap();
            let partition =
                crate::boot::shutdown::Device::from_raw(fs::metadata("/dev/vdb1").unwrap().rdev());
            let block = live
                .blocks
                .iter()
                .find(|b| b.mapping.as_ref().is_some_and(|m| m.name == MAPPING))
                .unwrap();
            let mapping = block.mapping.clone().unwrap();
            assert_eq!(block.slaves, std::collections::BTreeSet::from([partition]));
            assert!(
                mapping
                    .table
                    .contains(" crypt aes-xts-plain64 :64:logon:mica:data 0 "),
                "{}",
                mapping.table
            );
            println!("VM format: the exitrd scan sees {}", mapping.table);
            let remove = crate::boot::shutdown::Operation::RemoveMapping(mapping);
            crate::boot::shutdown::worker(&[
                "--lifecycle-worker".into(),
                serde_json::to_string(&remove).unwrap(),
            ])
            .unwrap()
            .unwrap();
            assert!(fs::metadata(MAPPED).is_err(), "released");
            fs::remove_file(MAPPED).unwrap();
            println!("VM format: the exitrd worker released the mapping");
            OpenOptions::new()
                .write(true)
                .open("/dev/vdd")
                .unwrap()
                .write_all_at(format!("{recovery}\n").as_bytes(), 0)
                .unwrap();
            println!(
                "VM format: formatted, mapped, written; {}",
                serde_json::to_string(&Report {
                    recovery: None,
                    ..report
                })
                .unwrap()
            );

            // Sealed off: the same boot cannot unseal again.
            let again = open("/dev/vdb1", Some(&on));
            assert_eq!(again.state, State::Locked, "{again:?}");
            assert_eq!(again.reason.as_deref(), Some("unseal"));
            println!(
                "VM format: after seal-off: {}",
                serde_json::to_string(&again).unwrap()
            );

            // A cipher the kernel lacks: refused before anything is written,
            // even with plain allowed.
            let adiantum = open("/dev/vdc", Some(&encryption(ADIANTUM).unwrap()));
            assert_eq!(adiantum.state, State::Locked, "{adiantum:?}");
            assert!(
                adiantum
                    .reason
                    .as_deref()
                    .unwrap()
                    .starts_with("cipher-unavailable")
            );
            let vdc = File::open("/dev/vdc").unwrap();
            assert!(fresh(&vdc).unwrap(), "untouched");
            println!(
                "VM format: adiantum without kernel support: {}",
                serde_json::to_string(&adiantum).unwrap()
            );

            // A header is honoured with encryption off.
            let off = open("/dev/vdb1", None);
            assert_eq!(off.state, State::Locked, "{off:?}");
            println!("VM_STEP format PASS");
        }
        "unlock" => {
            let report = open("/dev/vdb1", Some(&on));
            assert_eq!(report.state, State::On, "{report:?}");
            assert!(!report.formatted);
            assert!(report.providers[0].unlocked_this_boot);
            assert!(report.recovery.is_none());
            mount_data();
            assert_eq!(fs::read_to_string("/mnt/proof").unwrap(), "mica w1 step 1");
            unmount_data();
            unmap();
            println!("VM unlock: {}", serde_json::to_string(&report).unwrap());
            println!("VM_STEP unlock PASS");
        }
        "cleared" => {
            let report = open("/dev/vdb1", Some(&on));
            assert_eq!(report.state, State::Locked, "{report:?}");
            assert_eq!(report.reason.as_deref(), Some("unseal"));
            let failure = report.providers[0].failure.clone().unwrap();
            assert!(failure.contains("not the one sealed to"), "{failure}");
            println!("VM cleared: {}", serde_json::to_string(&report).unwrap());
            println!("VM_STEP cleared PASS");
        }
        other => panic!("unknown step {other}"),
    }
}

use super::*;
use serde_json::Value;

const VECTOR: &str = include_str!("../../tests/component-contracts/driver-ext.json");

/// The shared vector's descriptor and manifest: `rtl8125`, one module and
/// one firmware file.
fn manifest() -> Manifest {
    let vector: Value = serde_json::from_str(VECTOR).unwrap();
    let descriptor =
        crate::driver_ext::parse_descriptor(vector["descriptorBytes"].as_str().unwrap().as_bytes())
            .unwrap();
    crate::driver_ext::parse_manifest(
        vector["manifestBytes"].as_str().unwrap().as_bytes(),
        &descriptor.descriptor,
    )
    .unwrap()
}

/// The vector's image tree, as it mounts.
fn image(root: &Path, manifest: &Manifest) {
    let modules = root.join(manifest.module_directory());
    fs::create_dir_all(&modules).unwrap();
    fs::write(modules.join("r8125.ko"), b"module").unwrap();
    fs::create_dir_all(root.join("firmware/rtl_nic")).unwrap();
    fs::write(root.join("firmware/rtl_nic/rtl8125b-2.fw"), b"firmware").unwrap();
    fs::write(root.join("manifest.json"), b"{}").unwrap();
}

#[test]
fn the_state_names_a_build_per_kernel_and_name() {
    let system = tempfile::tempdir().unwrap();
    assert_eq!(read_state(system.path()).unwrap(), State::default());
    let mut state = State::default();
    state
        .builds
        .entry("a".repeat(64))
        .or_default()
        .insert("rtl8125".into(), "b".repeat(64));
    write_state(system.path(), &state).unwrap();
    assert_eq!(read_state(system.path()).unwrap(), state);
    assert_eq!(state.ids(), BTreeSet::from(["b".repeat(64).as_str()]));

    for bad in [
        r#"{"builds":{"x":{"rtl8125":"bbbb"}}}"#.to_owned(),
        format!(
            r#"{{"builds":{{"{}":{{"Bad":"{}"}}}}}}"#,
            "a".repeat(64),
            "b".repeat(64)
        ),
        format!(r#"{{"builds":{{"{}":{{}}}}}}"#, "a".repeat(64)),
        r#"{"builds":{},"pending":1}"#.to_owned(),
    ] {
        fs::write(dir(system.path()).join("state.json"), &bad).unwrap();
        assert!(read_state(system.path()).is_err(), "{bad}");
    }

    write_state(system.path(), &State::default()).unwrap();
    assert!(
        !dir(system.path()).join("state.json").exists(),
        "empty is no file"
    );
    assert_eq!(read_state(system.path()).unwrap(), State::default());
}

#[test]
fn an_image_holds_what_its_manifest_lists_and_nothing_else() {
    let manifest = manifest();
    let mount = tempfile::tempdir().unwrap();
    image(mount.path(), &manifest);
    check_tree(mount.path(), &manifest).unwrap();

    fs::write(mount.path().join("firmware/extra.fw"), b"x").unwrap();
    let error = check_tree(mount.path(), &manifest).unwrap_err().to_string();
    assert!(
        error.contains("does not list: firmware/extra.fw"),
        "{error}"
    );
    fs::remove_file(mount.path().join("firmware/extra.fw")).unwrap();

    fs::remove_file(mount.path().join("firmware/rtl_nic/rtl8125b-2.fw")).unwrap();
    let error = check_tree(mount.path(), &manifest).unwrap_err().to_string();
    assert!(error.contains("lacks what its manifest lists"), "{error}");
    std::os::unix::fs::symlink(
        "/etc/shadow",
        mount.path().join("firmware/rtl_nic/rtl8125b-2.fw"),
    )
    .unwrap();
    let error = check_tree(mount.path(), &manifest).unwrap_err().to_string();
    assert!(error.contains("not a file"), "{error}");
}

#[test]
fn a_later_extension_may_not_repeat_an_earlier_ones_module_or_firmware() {
    let first = manifest();
    let mut claims = Claims::default();
    claims.claim(&first).unwrap();

    let mut module = manifest();
    module.name = "other".into();
    module.firmware.clear();
    let error = claims.claim(&module).unwrap_err().to_string();
    assert!(error.contains("module r8125"), "{error}");

    let mut firmware = manifest();
    firmware.modules = vec!["other.ko".into()];
    let error = claims.claim(&firmware).unwrap_err().to_string();
    assert!(error.contains("firmware rtl_nic/rtl8125b-2.fw"), "{error}");

    let mut distinct = manifest();
    distinct.modules = vec!["other.ko".into()];
    distinct.firmware = vec!["other/other.fw".into()];
    claims.claim(&distinct).unwrap();
}

#[test]
fn options_and_loads_go_to_the_run_directories_as_kmod_reads_them() {
    let manifest = manifest();
    assert_eq!(modprobe_conf(&manifest), "options r8125 aspm=0\n");
    assert_eq!(modules_load(&manifest), "r8125\n");
}

#[test]
fn the_boot_record_names_what_was_mapped() {
    let record = BootExtensions {
        skipped: None,
        trial: None,
        extensions: vec![
            BootExtension {
                name: "a".into(),
                id: "1".repeat(64),
                version: Some("1".into()),
                state: BootState::Mapped,
                reason: None,
            },
            BootExtension {
                name: "b".into(),
                id: "2".repeat(64),
                version: None,
                state: BootState::Refused,
                reason: Some("why".into()),
            },
            BootExtension {
                name: "c".into(),
                id: "3".repeat(64),
                version: Some("1".into()),
                state: BootState::Unused,
                reason: Some("why".into()),
            },
        ],
    };
    assert_eq!(
        record.mapped(),
        BTreeSet::from(["1".repeat(64), "3".repeat(64)])
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("extensions.json");
    assert_eq!(booted(&path).unwrap(), None);
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert_eq!(booted(&path).unwrap(), Some(record));
}

/// A change alone is tried only on a boot of a kernel whose builds it
/// changes, so a boot of another kernel spends nothing of its trial; a pair
/// maps only with its deployment.
#[test]
fn a_boot_maps_the_pending_change_only_where_it_is_tried() {
    let build = |id: &str| BTreeMap::from([("rtl8125".to_owned(), id.repeat(64))]);
    let (running, other) = ("a".repeat(64), "b".repeat(64));
    let mut state = State {
        builds: Builds::from([(running.clone(), build("1")), (other.clone(), build("2"))]),
        pending: Some(Builds::from([
            (running.clone(), build("3")),
            (other.clone(), build("2")),
        ])),
        pair: None,
    };
    let refuse = |_: &str| false;
    let (builds, trial) = boot_choice(&state, "d", &other, |_| panic!("spent"));
    assert_eq!((builds, trial), (build("2"), None));
    assert_eq!(
        boot_choice(&state, "d", &running, refuse),
        (build("1"), None)
    );
    let (builds, trial) = boot_choice(&state, "d", &running, |id| {
        assert_eq!(Some(id.to_owned()), state.pending_id());
        true
    });
    assert_eq!(builds, build("3"));
    assert!(!trial.unwrap().paired);
    state.pair = Some("e".repeat(64));
    let (builds, trial) = boot_choice(&state, &"e".repeat(64), &running, |_| panic!("spent"));
    assert_eq!(builds, build("3"));
    assert!(trial.unwrap().paired);
    assert_eq!(
        boot_choice(&state, "d", &running, |_| panic!("spent")),
        (build("1"), None)
    );
}

/// The state with a change on trial reads back as it was written, and a
/// pending change names its builds for collection.
#[test]
fn a_pending_change_is_kept_and_its_builds_are_named() {
    let dir = tempfile::TempDir::new().unwrap();
    let state = State {
        builds: Builds::new(),
        pending: Some(Builds::from([(
            "a".repeat(64),
            BTreeMap::from([("rtl8125".to_owned(), "3".repeat(64))]),
        )])),
        pair: Some("e".repeat(64)),
    };
    write_state(dir.path(), &state).unwrap();
    assert_eq!(read_state(dir.path()).unwrap(), state);
    assert!(state.ids().contains("3".repeat(64).as_str()));
    assert_eq!(state.paired(), Some("e".repeat(64).as_str()));
    write_state(dir.path(), &State::default()).unwrap();
    assert!(!dir.path().join("extensions/state.json").exists());
}

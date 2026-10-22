use super::*;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::PathBuf;

fn staged() -> Request {
    Request {
        v: 1,
        requested: 1_700_000_000,
        by: Requester::Console,
        source: "192.0.2.7".into(),
    }
}

/// A DATA root as a device leaves it after use, with `outside` a directory
/// the erase must not reach through a link.
fn seed(data: &Path, outside: &Path) {
    for dir in ["state", "meta", "cache", "tmp", "var", "mica", "srv"] {
        fs::create_dir(data.join(dir)).unwrap();
    }
    fs::write(data.join("meta/firmware.json"), "receipt").unwrap();
    fs::write(data.join("meta/catalog.json"), "checkpoints").unwrap();
    fs::write(data.join("state/settings.json"), "admin").unwrap();
    fs::write(
        data.join("state/machine-id"),
        "0123456789abcdef0123456789abcdef\n",
    )
    .unwrap();
    fs::create_dir_all(data.join("state/ssh")).unwrap();
    fs::write(data.join("state/ssh/ssh_host_ed25519_key"), "key").unwrap();
    let mut deep = data.join("srv");
    for level in 0..20 {
        deep.push(format!("d{level}"));
    }
    fs::create_dir_all(&deep).unwrap();
    fs::write(deep.join("leaf"), "data").unwrap();
    fs::create_dir_all(data.join("containers/overlay/l")).unwrap();
    fs::write(data.join("containers/overlay/l/file"), "layer").unwrap();
    fs::write(data.join("var/log"), "journal").unwrap();
    fs::write(outside.join("kept"), "not DATA's").unwrap();
    symlink(outside, data.join("mica/link")).unwrap();
    symlink(outside.join("kept"), data.join("mica/file-link")).unwrap();
    fs::create_dir(data.join("mica/locked")).unwrap();
    fs::write(data.join("mica/locked/inside"), "x").unwrap();
    fs::set_permissions(data.join("mica/locked"), fs::Permissions::from_mode(0o500)).unwrap();
    fs::write(data.join("loose"), "top-level file").unwrap();
}

fn tree(root: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            out.push(path.strip_prefix(root).unwrap().display().to_string());
            if path.symlink_metadata().unwrap().is_dir() {
                walk(root, &path, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

fn factory_tree() -> Vec<String> {
    let mut expected: Vec<String> = [
        "cache",
        "mica",
        "meta",
        "meta/catalog.json",
        "meta/factory-reset-done.json",
        "meta/firmware.json",
        "srv",
        "state",
        "tmp",
        "var",
    ]
    .map(String::from)
    .to_vec();
    expected.sort();
    expected
}

fn erase(data: &Path, now: u64) -> Result<Option<Done>> {
    apply(data, now, &mut || Ok(()), &mut |line| eprintln!("{line}"))
}

/// The erase over a seeded DATA, on `data` (a plain directory here, an ext4
/// mount in `factory_reset_ext4`).
fn erases_a_seeded_tree(data: &Path, outside: &Path) {
    seed(data, outside);
    request(&data.join("meta"), &staged()).unwrap();
    assert!(pending(&data.join("meta")));
    let done = erase(data, 1_700_000_100).unwrap().unwrap();
    assert_eq!(
        done,
        Done {
            v: 1,
            requested: 1_700_000_000,
            by: Requester::Console,
            source: "192.0.2.7".into(),
            completed: 1_700_000_100,
            unremoved: 0,
        }
    );
    assert_eq!(tree(data), factory_tree());
    assert!(!pending(&data.join("meta")));
    assert_eq!(super::done(&data.join("meta")).unwrap(), Some(done));
    assert_eq!(
        fs::read_to_string(data.join("meta/firmware.json")).unwrap(),
        "receipt"
    );
    assert_eq!(
        fs::read_to_string(outside.join("kept")).unwrap(),
        "not DATA's"
    );
    for (name, mode) in SKELETON {
        assert_eq!(
            fs::metadata(data.join(name)).unwrap().mode() & 0o7777,
            mode,
            "{name}"
        );
    }
    assert_eq!(
        fs::metadata(data.join("meta").join(DONE)).unwrap().mode() & 0o777,
        0o600
    );
    // Nothing staged: the next boot leaves DATA alone.
    fs::write(data.join("state/fresh"), "first boot").unwrap();
    assert_eq!(erase(data, 1_700_000_200).unwrap(), None);
    assert!(data.join("state/fresh").exists());
}

#[test]
fn a_staged_reset_erases_everything_but_meta() {
    let dir = tempfile::tempdir().unwrap();
    let (data, outside) = (dir.path().join("data"), dir.path().join("outside"));
    fs::create_dir(&data).unwrap();
    fs::create_dir(&outside).unwrap();
    erases_a_seeded_tree(&data, &outside);
}

/// The same erase on a real ext4, when the environment names a mounted one
/// (`MICA_TEST_EXT4`, an empty ext4 mount): the gate runs it in a privileged
/// container on a loop device.
#[test]
fn factory_reset_ext4() {
    let Some(mount) = std::env::var_os("MICA_TEST_EXT4").map(PathBuf::from) else {
        return;
    };
    let data = mount.join("data");
    let outside = mount.join("outside");
    fs::create_dir(&data).unwrap();
    fs::create_dir(&outside).unwrap();
    erases_a_seeded_tree(&data, &outside);
}

#[test]
fn an_interrupted_erase_resumes_at_the_next_boot() {
    // Interrupt the erase before each of its removals in turn, as a power
    // cut or a watchdog reset would; the next boot must finish it.
    let mut interrupted = 0;
    for stop_at in 0.. {
        let dir = tempfile::tempdir().unwrap();
        let (data, outside) = (dir.path().join("data"), dir.path().join("outside"));
        fs::create_dir(&data).unwrap();
        fs::create_dir(&outside).unwrap();
        seed(&data, &outside);
        request(&data.join("meta"), &staged()).unwrap();
        let mut calls = 0;
        let first = apply(
            &data,
            1,
            &mut || {
                calls += 1;
                ensure!(calls <= stop_at, "power cut");
                Ok(())
            },
            &mut |_| {},
        );
        if first.is_ok() {
            break;
        }
        interrupted += 1;
        assert!(
            pending(&data.join("meta")),
            "the request outlives an interruption"
        );
        assert_eq!(super::done(&data.join("meta")).unwrap(), None);
        let done = erase(&data, 2).unwrap().unwrap();
        assert_eq!((done.completed, done.unremoved), (2, 0));
        assert_eq!(tree(&data), factory_tree());
        assert_eq!(
            fs::read_to_string(outside.join("kept")).unwrap(),
            "not DATA's"
        );
    }
    assert!(interrupted > 20, "only {interrupted} interruption points");
}

#[test]
fn a_reset_interrupted_after_its_record_erases_again() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    fs::create_dir(data.join("meta")).unwrap();
    request(&data.join("meta"), &staged()).unwrap();
    erase(data, 5).unwrap().unwrap();
    // The record was written and the request not yet removed.
    request(&data.join("meta"), &staged()).unwrap();
    fs::write(data.join("state/settings.json"), "written before the cut?").unwrap();
    let done = erase(data, 6).unwrap().unwrap();
    assert_eq!(done.completed, 6);
    assert!(!data.join("state/settings.json").exists());
}

#[test]
fn an_unreadable_request_erases_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path();
    fs::create_dir(data.join("meta")).unwrap();
    fs::create_dir(data.join("state")).unwrap();
    fs::write(data.join("state/settings.json"), "admin").unwrap();
    for bad in [
        r#"{"v":2,"requested":1,"by":"cli","source":""}"#,
        r#"{"v":1,"requested":1,"by":"ble","source":""}"#,
        r#"{"v":1,"requested":1,"by":"cli","source":"a\nb"}"#,
        "{",
    ] {
        fs::write(data.join("meta").join(REQUEST), bad).unwrap();
        let mut lines = Vec::new();
        assert_eq!(
            apply(data, 1, &mut || Ok(()), &mut |l| lines.push(l.to_string())).unwrap(),
            None
        );
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(!pending(&data.join("meta")));
        assert_eq!(
            fs::read_to_string(data.join("meta").join(INVALID)).unwrap(),
            bad
        );
        assert!(data.join("state/settings.json").exists());
    }
}

#[test]
fn a_request_is_checked_before_it_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut bad = staged();
    bad.source = "x".repeat(257);
    assert!(request(dir.path(), &bad).is_err());
    assert!(!pending(dir.path()));
    request(dir.path(), &staged()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Request>(&fs::read(dir.path().join(REQUEST)).unwrap()).unwrap(),
        staged()
    );
    assert_eq!(
        fs::metadata(dir.path().join(REQUEST)).unwrap().mode() & 0o777,
        0o600
    );
}

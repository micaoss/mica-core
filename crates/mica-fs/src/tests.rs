use std::os::unix::fs::{MetadataExt, PermissionsExt};

use super::*;

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn replace_writes_the_mode_and_leaves_no_temporary() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    Replace::new(&path, "tmp")
        .unwrap()
        .mode(0o600)
        .write("first")
        .unwrap();
    let before = fs::metadata(&path).unwrap().ino();
    Replace::new(&path, "tmp")
        .unwrap()
        .mode(0o640)
        .write("second")
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
    let metadata = fs::metadata(&path).unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, 0o640);
    assert_ne!(metadata.ino(), before, "a replace is a new inode");
    assert_eq!(names(dir.path()), ["state.json"]);
}

#[test]
fn a_leftover_temporary_is_replaced_not_reused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("conf");
    let leftover = dir.path().join(".conf.tmp");
    fs::write(&leftover, "stale").unwrap();
    fs::set_permissions(&leftover, fs::Permissions::from_mode(0o666)).unwrap();
    Replace::new(&path, "tmp")
        .unwrap()
        .mode(0o600)
        .write("new")
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(names(dir.path()), ["conf"]);
}

#[test]
fn a_symlink_planted_as_the_temporary_is_not_followed() {
    let dir = tempfile::tempdir().unwrap();
    let victim = dir.path().join("victim");
    fs::write(&victim, "untouched").unwrap();
    std::os::unix::fs::symlink(&victim, dir.path().join(".conf.tmp")).unwrap();
    Replace::new(&dir.path().join("conf"), "tmp")
        .unwrap()
        .write("new")
        .unwrap();
    assert_eq!(fs::read_to_string(&victim).unwrap(), "untouched");
}

#[test]
fn a_directory_under_the_temporary_name_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("deployments");
    let temp = dir.path().join("deployments.pending");
    fs::create_dir(&temp).unwrap();
    assert!(Replace::via(&path, temp.clone()).write("x").is_err());
    assert!(temp.is_dir() && !path.exists());
}

#[test]
fn bounded_reads_refuse_what_they_must() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("file");
    fs::write(&file, "12345").unwrap();
    assert_eq!(read_bounded(&file, 5).unwrap(), b"12345");
    assert!(read_bounded(&file, 4).is_err());
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&file, &link).unwrap();
    assert!(read_bounded(&link, 5).is_err());
    assert!(read_bounded(dir.path(), 5).is_err());
}

#[test]
fn trimmed_reads_drop_whitespace_and_nuls() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("model");
    fs::write(&file, "board\0\n").unwrap();
    assert_eq!(read_trimmed(&file).as_deref(), Some("board"));
    fs::write(&file, " \n").unwrap();
    assert_eq!(read_trimmed(&file), None);
    assert_eq!(read_trimmed(&dir.path().join("absent")), None);
}

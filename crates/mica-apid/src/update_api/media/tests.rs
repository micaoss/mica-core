use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use tempfile::TempDir;

use super::*;

const VOLUME: &str = "0123456789abcdef0123";

fn volume() -> TempDir {
    let root = TempDir::new().unwrap();
    fs::create_dir(root.path().join(VOLUME)).unwrap();
    root
}

fn names(root: &Path) -> Vec<String> {
    let (found, _) = scan(root, VOLUME).unwrap();
    found.into_iter().map(|f| f.candidate.name).collect()
}

/// The root and `mica-updates/` only, regular `.micaupd` files only, in name
/// order; a link, a directory, a deeper file and another suffix are not
/// archives here.
#[test]
fn only_regular_archives_at_the_root_and_in_mica_updates_are_listed() {
    let root = volume();
    let at = root.path().join(VOLUME);
    fs::write(at.join("b.micaupd"), b"MICAUPD1b").unwrap();
    fs::write(at.join("a.micaupd"), b"MICAUPD1a").unwrap();
    fs::write(at.join("notes.txt"), b"x").unwrap();
    fs::write(at.join(".hidden.micaupd"), b"x").unwrap();
    fs::create_dir(at.join("dir.micaupd")).unwrap();
    symlink(at.join("a.micaupd"), at.join("link.micaupd")).unwrap();
    symlink("/etc/shadow", at.join("shadow.micaupd")).unwrap();
    fs::create_dir_all(at.join("mica-updates/deeper")).unwrap();
    fs::write(at.join("mica-updates/c.micaupd"), b"MICAUPD1c").unwrap();
    fs::write(at.join("mica-updates/deeper/d.micaupd"), b"MICAUPD1d").unwrap();
    fs::create_dir(at.join("other")).unwrap();
    fs::write(at.join("other/e.micaupd"), b"MICAUPD1e").unwrap();

    assert_eq!(
        names(root.path()),
        ["a.micaupd", "b.micaupd", "mica-updates/c.micaupd"]
    );
}

/// `mica-updates` as a link to elsewhere is not followed.
#[test]
fn a_linked_updates_directory_is_not_followed() {
    let root = volume();
    let elsewhere = TempDir::new().unwrap();
    fs::write(elsewhere.path().join("x.micaupd"), b"MICAUPD1").unwrap();
    symlink(
        elsewhere.path(),
        root.path().join(VOLUME).join("mica-updates"),
    )
    .unwrap();

    assert!(names(root.path()).is_empty());
}

#[test]
fn at_most_a_hundred_archives_are_listed_and_the_rest_is_said() {
    let root = volume();
    for n in 0..105 {
        fs::write(
            root.path().join(VOLUME).join(format!("{n:03}.micaupd")),
            b"MICAUPD1",
        )
        .unwrap();
    }
    let (found, more) = scan(root.path(), VOLUME).unwrap();
    assert_eq!(found.len(), 100);
    assert!(more);
    let listed = list(root.path(), None);
    assert_eq!(listed.truncated, [VOLUME]);
}

/// A file larger than an upload may be is listed, not importable, and its
/// import is refused before anything is read.
#[test]
fn an_archive_too_large_is_listed_and_refused() {
    let root = volume();
    let file = fs::File::create(root.path().join(VOLUME).join("big.micaupd")).unwrap();
    file.set_len(MAX_ARCHIVE_BYTES + 1).unwrap();

    let (found, _) = scan(root.path(), VOLUME).unwrap();
    assert!(!found[0].candidate.importable);
    assert_eq!(found[0].candidate.reason, Some("archive_too_large"));
    let id = found[0].candidate.id.clone();
    assert_eq!(
        open_candidate(root.path(), VOLUME, &id).err(),
        Some(Refusal::TooLarge)
    );
}

/// The id follows the file: rewritten or replaced, the archive listed is no
/// longer the one asked for.
#[test]
fn a_changed_or_replaced_archive_gets_another_id() {
    let root = volume();
    let path = root.path().join(VOLUME).join("u.micaupd");
    fs::write(&path, b"MICAUPD1one").unwrap();
    let (found, _) = scan(root.path(), VOLUME).unwrap();
    let id = found[0].candidate.id.clone();
    assert!(open_candidate(root.path(), VOLUME, &id).is_ok());

    fs::write(&path, b"MICAUPD1three").unwrap();
    assert_eq!(
        open_candidate(root.path(), VOLUME, &id).err(),
        Some(Refusal::Changed)
    );

    // The same name and size, another file.
    let (found, _) = scan(root.path(), VOLUME).unwrap();
    let id = found[0].candidate.id.clone();
    fs::remove_file(&path).unwrap();
    fs::write(root.path().join(VOLUME).join("spare"), b"MICAUPD1other").unwrap();
    fs::rename(root.path().join(VOLUME).join("spare"), &path).unwrap();
    assert_eq!(
        open_candidate(root.path(), VOLUME, &id).err(),
        Some(Refusal::Changed)
    );

    // A link put where the file was.
    fs::remove_file(&path).unwrap();
    symlink("/etc/hostname", &path).unwrap();
    assert_eq!(
        open_candidate(root.path(), VOLUME, &id).err(),
        Some(Refusal::Changed)
    );
}

#[test]
fn a_volume_that_is_gone_is_removed() {
    let root = TempDir::new().unwrap();
    assert_eq!(
        open_candidate(root.path(), VOLUME, "00").err(),
        Some(Refusal::Removed)
    );
    assert_eq!(list(root.path(), Some(VOLUME)).unreadable, [VOLUME]);
}

#[test]
fn every_volume_is_listed_without_a_media_id() {
    let root = volume();
    let other = "ffffffffffffffffffff";
    fs::create_dir(root.path().join(other)).unwrap();
    fs::create_dir(root.path().join("not-a-volume")).unwrap();
    fs::write(root.path().join(VOLUME).join("a.micaupd"), b"MICAUPD1").unwrap();
    fs::write(root.path().join(other).join("b.micaupd"), b"MICAUPD1").unwrap();
    fs::write(
        root.path().join("not-a-volume").join("c.micaupd"),
        b"MICAUPD1",
    )
    .unwrap();

    let listed = list(root.path(), None);
    let found: Vec<_> = listed
        .items
        .iter()
        .map(|c| (c.media_id.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(found, [(VOLUME, "a.micaupd"), (other, "b.micaupd")]);
}

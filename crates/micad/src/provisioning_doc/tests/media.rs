//! Which medium a document is read from, and how.

use micad_settings::Settings;
use std::fs;
use tempfile::TempDir;

use super::*;

// The preference order: the BOOT medium wins, and the other source is not
// consulted at all.
#[test]
pub(super) fn the_boot_medium_wins_over_removable_media() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let root = stage(
        dir.path(),
        Source::Boot,
        "version = 1\n\n[time]\ntimezone = \"Europe/Berlin\"\n",
    );
    stage(
        dir.path(),
        Source::Media,
        "version = 1\n\n[time]\ntimezone = \"Asia/Shanghai\"\n",
    );
    let mut settings = Settings::default();

    let outcome = import(&store, &mut settings, &root).expect("import");
    let Outcome::Applied { source, .. } = outcome else {
        panic!("expected an applied document, got {outcome:?}");
    };
    assert_eq!(source, Source::Boot);
    assert_eq!(settings.time.timezone, "Europe/Berlin");
}

// With no boot document the media one is taken, which is the whole of
// path 2.
#[test]
pub(super) fn a_removable_medium_is_taken_when_the_boot_medium_carries_nothing() {
    let dir = TempDir::new().expect("tempdir");
    let (_, settings, outcome) = import_body(dir.path(), Source::Media, time_only_document());
    let Outcome::Applied { source, .. } = outcome else {
        panic!("expected an applied document, got {outcome:?}");
    };
    assert_eq!(source, Source::Media);
    assert_eq!(settings.time.timezone, "Europe/Berlin");
}

// No medium, or a medium with no document: nothing is read, nothing is
// written, and the record of the document this device DID apply survives.
#[test]
pub(super) fn no_document_writes_nothing_and_forgets_nothing() {
    let dir = TempDir::new().expect("tempdir");
    let store = store_in(dir.path());
    let root = stage(dir.path(), Source::Boot, time_only_document());
    let mut settings = Settings::default();
    import(&store, &mut settings, &root).expect("import");
    let applied = settings.clone();
    let bytes = fs::read(settings_path(dir.path())).expect("read settings file");

    // The medium is gone on the next boot.
    fs::remove_file(root.join("boot").join(DOCUMENT_FILE_NAME)).expect("remove");
    let mut reloaded = store.load().expect("reload");
    assert_eq!(
        import(&store, &mut reloaded, &root).expect("import"),
        Outcome::NoDocument
    );
    assert_eq!(reloaded, applied);
    assert_eq!(
        fs::read(settings_path(dir.path())).expect("re-read"),
        bytes,
        "a boot with no document must write nothing"
    );

    // And a staging root that was never created at all.
    let mut reloaded = store.load().expect("reload");
    assert_eq!(
        import(&store, &mut reloaded, &dir.path().join("absent")).expect("import"),
        Outcome::NoDocument
    );
    assert_eq!(reloaded, applied);
}

// Only a regular file at the one documented path is read. A symbolic link
// planted on operator media would otherwise name a path on the DEVICE, and
// a character device would hang early boot on a read that never ends.
#[test]
pub(super) fn only_a_regular_file_at_the_documented_path_is_read() {
    let dir = TempDir::new().expect("tempdir");
    let root = dir.path().join("staging");
    let boot = root.join("boot");
    fs::create_dir_all(&boot).expect("create staging dir");

    // A document at a name this code does not look for is not found.
    fs::write(boot.join("provisioning.toml"), time_only_document()).expect("write");
    fs::write(root.join(DOCUMENT_FILE_NAME), time_only_document()).expect("write");
    assert_eq!(find_document(&root), None);

    // A directory under the document's name is not a document.
    fs::create_dir(boot.join(DOCUMENT_FILE_NAME)).expect("create dir");
    assert_eq!(find_document(&root), None);
    fs::remove_dir(boot.join(DOCUMENT_FILE_NAME)).expect("remove dir");

    // A symbolic link, even one pointing at a perfectly good document.
    let target = dir.path().join("elsewhere.toml");
    fs::write(&target, time_only_document()).expect("write");
    std::os::unix::fs::symlink(&target, boot.join(DOCUMENT_FILE_NAME)).expect("symlink");
    assert_eq!(find_document(&root), None);
    fs::remove_file(boot.join(DOCUMENT_FILE_NAME)).expect("remove link");

    // The positive control: the same bytes, as a regular file, ARE found.
    fs::write(boot.join(DOCUMENT_FILE_NAME), time_only_document()).expect("write");
    assert_eq!(
        find_document(&root),
        Some((Source::Boot, boot.join(DOCUMENT_FILE_NAME)))
    );
}

// A document too large to be one is refused before its bytes are read, so
// a stick carrying a huge file is a legible refusal and not an
// out-of-memory kill during early boot.
#[test]
pub(super) fn an_oversized_document_is_refused_without_being_read() {
    let dir = TempDir::new().expect("tempdir");
    let body = "#".repeat(usize::try_from(MAX_DOCUMENT_BYTES).expect("fits") + 1);
    let (_, settings, outcome) = import_body(dir.path(), Source::Media, &body);
    let Outcome::Rejected { rejection, .. } = outcome else {
        panic!("expected a rejection, got {outcome:?}");
    };
    assert_eq!(rejection.key, "");
    assert!(rejection.reason.contains("maximum"), "{rejection:?}");
    assert!(settings.access.web_admin.is_none());
}

// A file that is not TOML is a refusal, and the parser's own message —
// which quotes source text — is not what is reported.
#[test]
pub(super) fn a_file_that_is_not_toml_is_refused_without_quoting_it() {
    let dir = TempDir::new().expect("tempdir");
    let body = format!("this is not TOML {SECRET_PASSWORD}");
    let (_, _, outcome) = import_body(dir.path(), Source::Boot, &body);
    let Outcome::Rejected { rejection, .. } = outcome else {
        panic!("expected a rejection, got {outcome:?}");
    };
    assert_eq!(rejection.key, "");
    assert_eq!(rejection.reason, "the document is not valid TOML");
    assert!(!rejection.to_string().contains(SECRET_PASSWORD));
}
